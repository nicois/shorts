use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ruff_python_parser::{parse_unchecked, Mode, ParseOptions};
use ruff_text_size::Ranged;
use serde::{Deserialize, Serialize};

use crate::git::GitRepo;
use crate::symbols::{self, Symbol};

const CACHE_DIR: &str = ".shorts";
const CACHE_FILE: &str = "cache.json";

#[derive(Serialize, Deserialize, Clone)]
struct CacheEntry {
    mtime_secs: u64,
    mtime_nanos: u32,
    imports: Vec<String>,
    /// Hash of non-trivia tokens (ignores comments, whitespace, blank lines).
    /// Used to detect whether a file has semantically changed.
    #[serde(default)]
    content_hash: u64,
    /// Per-symbol hashes for fine-grained change detection.
    /// Keys are symbol cache keys (e.g. "fn:foo", "cls:Bar", "__body__").
    /// None = old cache format, triggers re-parse.
    #[serde(default)]
    symbol_hashes: Option<HashMap<String, u64>>,
}

/// On-disk cache mapping absolute file paths to their extracted imports
/// and a semantic content hash. Invalidated per-file when mtime changes.
#[derive(Serialize, Deserialize, Default)]
pub struct ImportCache {
    entries: HashMap<PathBuf, CacheEntry>,
}

/// Compute a hash of the semantic content of Python source code.
/// Parses the source and hashes all non-trivia tokens (skipping comments
/// and non-logical newlines), using both token kind and source text.
pub fn semantic_hash(source: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    let parsed = parse_unchecked(source, ParseOptions::from(Mode::Module));
    for token in parsed.tokens() {
        if token.kind().is_trivia() {
            continue;
        }
        token.kind().hash(&mut hasher);
        let range = token.range();
        let start: usize = range.start().into();
        let end: usize = range.end().into();
        source[start..end].hash(&mut hasher);
    }
    hasher.finish()
}

impl ImportCache {
    /// Load cache from `.shorts/cache.json` under the given base directory.
    /// Returns an empty cache on any failure.
    pub fn load(base: &Path) -> Self {
        let path = base.join(CACHE_DIR).join(CACHE_FILE);
        fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Save cache to `.shorts/cache.json` under the given base directory.
    pub fn save(&self, base: &Path) {
        let dir = base.join(CACHE_DIR);
        if fs::create_dir_all(&dir).is_err() {
            log::warn!("failed to create cache directory: {}", dir.display());
            return;
        }
        let path = dir.join(CACHE_FILE);
        match serde_json::to_string(self) {
            Ok(json) => {
                if let Err(e) = fs::write(&path, json) {
                    log::warn!("failed to write cache: {}", e);
                }
            }
            Err(e) => log::warn!("failed to serialize cache: {}", e),
        }
    }

    /// Look up cached imports for a file. Returns `Some(imports)` if the file's
    /// mtime matches the cached mtime, `None` otherwise.
    pub fn get(&self, path: &Path) -> Option<&[String]> {
        let entry = self.entries.get(path)?;
        let meta = fs::metadata(path).ok()?;
        let mtime = meta.modified().ok()?;
        let duration = mtime.duration_since(SystemTime::UNIX_EPOCH).ok()?;
        if duration.as_secs() == entry.mtime_secs && duration.subsec_nanos() == entry.mtime_nanos {
            Some(&entry.imports)
        } else {
            None
        }
    }

    /// Insert or update a cache entry for a file.
    pub fn insert(
        &mut self,
        path: PathBuf,
        imports: Vec<String>,
        content_hash: u64,
        symbol_hashes: HashMap<String, u64>,
    ) {
        let mtime = fs::metadata(&path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok());
        if let Some(duration) = mtime {
            self.entries.insert(
                path,
                CacheEntry {
                    mtime_secs: duration.as_secs(),
                    mtime_nanos: duration.subsec_nanos(),
                    imports,
                    content_hash,
                    symbol_hashes: Some(symbol_hashes),
                },
            );
        }
    }

    /// Filter a set of changed files down to those that have actually changed
    /// semantically (i.e., non-comment/whitespace changes) compared to the
    /// upstream git version.
    ///
    /// Compares the semantic hash of the current file against the upstream
    /// version at the merge-base. Files that are new (not in upstream) or
    /// unreadable are always considered changed.
    pub fn filter_semantically_changed(
        &self,
        files: &HashSet<PathBuf>,
        repo: &GitRepo,
        upstream_ref: &str,
    ) -> HashSet<PathBuf> {
        files
            .iter()
            .filter(|path| {
                let current_source = match fs::read_to_string(path) {
                    Ok(s) => s,
                    Err(_) => return true, // can't read — assume changed
                };
                let upstream_source = match repo.file_content_at_ref(path, upstream_ref) {
                    Some(s) => s,
                    None => return true, // new file or not in upstream — assume changed
                };
                semantic_hash(&current_source) != semantic_hash(&upstream_source)
            })
            .cloned()
            .collect()
    }

    /// Determine which specific symbols changed in each file compared to
    /// the upstream git version.
    ///
    /// Returns `(symbol_changes, fallback_files)`:
    /// - `symbol_changes`: files where we can identify specific changed symbols
    /// - `fallback_files`: files that must use module-level tracking (can't get upstream)
    pub fn detect_changed_symbols(
        files: &HashSet<PathBuf>,
        repo: &GitRepo,
        upstream_ref: &str,
    ) -> (HashMap<PathBuf, HashSet<Symbol>>, HashSet<PathBuf>) {
        let mut symbol_changes: HashMap<PathBuf, HashSet<Symbol>> = HashMap::new();
        let mut fallback_files: HashSet<PathBuf> = HashSet::new();

        for path in files {
            let current_source = match fs::read_to_string(path) {
                Ok(s) => s,
                Err(_) => {
                    fallback_files.insert(path.clone());
                    continue;
                }
            };

            let upstream_source = match repo.file_content_at_ref(path, upstream_ref) {
                Some(s) => s,
                None => {
                    // New file or can't get upstream — fall back to module-level
                    fallback_files.insert(path.clone());
                    continue;
                }
            };

            let current = symbols::extract_symbol_hashes(&current_source);
            let current_keyed: HashMap<String, u64> = current
                .iter()
                .map(|(sym, hash)| (sym.cache_key(), *hash))
                .collect();

            let upstream = symbols::extract_symbol_hashes(&upstream_source);
            let upstream_keyed: HashMap<String, u64> = upstream
                .iter()
                .map(|(sym, hash)| (sym.cache_key(), *hash))
                .collect();

            let mut changed = HashSet::new();

            // Check for changed or removed symbols
            for (key, old_hash) in &upstream_keyed {
                match current_keyed.get(key) {
                    Some(new_hash) if new_hash == old_hash => {} // unchanged
                    _ => {
                        // Changed or removed
                        if let Some(sym) = Symbol::from_cache_key(key) {
                            changed.insert(sym);
                        }
                    }
                }
            }

            // Check for new symbols
            for (key, _) in &current_keyed {
                if !upstream_keyed.contains_key(key) {
                    if let Some(sym) = Symbol::from_cache_key(key) {
                        changed.insert(sym);
                    }
                }
            }

            if !changed.is_empty() {
                // Propagate through intra-module dependencies: if B changed and
                // A calls B, then A is effectively changed too.
                let intra_deps = symbols::extract_intra_module_deps(&current_source);
                let propagated = symbols::propagate_intra_module_changes(&changed, &intra_deps);
                symbol_changes.insert(path.clone(), propagated);
            }
            // If changed is empty, the file had semantic changes (passed filter_semantically_changed)
            // but no individual symbol changed — this shouldn't happen, but if it does, skip the file
        }

        (symbol_changes, fallback_files)
    }

    /// Merge entries from another cache into this one (used to collect
    /// parallel results).
    pub fn merge(&mut self, other: ImportCache) {
        self.entries.extend(other.entries);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_semantic_hash_ignores_comments() {
        let base = "import foo\nx = 1\n";
        let with_comment = "import foo\n# this is a comment\nx = 1\n";
        assert_eq!(semantic_hash(base), semantic_hash(with_comment));
    }

    #[test]
    fn test_semantic_hash_ignores_inline_comment() {
        let base = "x = 1\n";
        let with_comment = "x = 1  # inline comment\n";
        assert_eq!(semantic_hash(base), semantic_hash(with_comment));
    }

    #[test]
    fn test_semantic_hash_detects_code_change() {
        let before = "x = 1\n";
        let after = "x = 2\n";
        assert_ne!(semantic_hash(before), semantic_hash(after));
    }

    #[test]
    fn test_semantic_hash_detects_import_change() {
        let before = "import foo\n";
        let after = "import bar\n";
        assert_ne!(semantic_hash(before), semantic_hash(after));
    }

    #[test]
    fn test_semantic_hash_ignores_blank_lines() {
        let base = "import foo\nx = 1\n";
        let with_blanks = "import foo\n\n\nx = 1\n";
        assert_eq!(semantic_hash(base), semantic_hash(with_blanks));
    }
}
