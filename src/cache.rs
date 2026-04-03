use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ruff_python_parser::{parse_unchecked, Mode, ParseOptions};
use ruff_text_size::Ranged;
use serde::{Deserialize, Serialize};

use crate::git::GitRepo;
use crate::symbols::{self, ModuleSymbolUsage, Symbol};

#[derive(Serialize, Deserialize, Clone)]
pub struct CacheEntry {
    pub imports: Vec<String>,
    /// Hash of non-trivia tokens (ignores comments, whitespace, blank lines).
    /// Used to detect whether a file has semantically changed.
    pub semantic_hash: u64,
    /// Per-symbol hashes for fine-grained change detection.
    /// Keys are symbol cache keys (e.g. "fn:foo", "cls:Bar", "__body__").
    /// None = old cache format, triggers re-parse.
    pub symbol_hashes: Option<HashMap<String, u64>>,
    /// What symbols this module uses from each imported module.
    pub symbol_usage: ModuleSymbolUsage,
}

/// Content-addressable, directory-backed import cache.
///
/// Individual entries are stored as JSON files under `.shorts/cache/{xx}/{full_hex}.json`,
/// where `xx` is the first two hex digits of the hash and `full_hex` is the full 16-char
/// hex representation of the cache key.
pub struct ImportCache {
    cache_dir: Option<PathBuf>,
    accessed: Mutex<HashSet<u64>>,
}

/// Compute a cache key from file content and module class name.
///
/// Module class is needed because relative imports resolve differently depending
/// on file location, so the same source content in different packages should produce
/// different cache entries.
pub fn cache_key(content: &[u8], module_class: &str) -> u64 {
    let content_hash = xxhash_rust::xxh3::xxh3_64(content);
    let module_hash = xxhash_rust::xxh3::xxh3_64(module_class.as_bytes());
    content_hash ^ module_hash.rotate_left(32)
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

/// Compute the on-disk path for a cache entry with the given hash.
fn entry_path(cache_base: &Path, hash: u64) -> PathBuf {
    let hex = format!("{:016x}", hash);
    cache_base
        .join(".shorts")
        .join("cache")
        .join(&hex[..2])
        .join(format!("{}.json", hex))
}

/// Ensure `.shorts/` is listed in `.gitignore` at the repo root.
pub fn ensure_gitignored(repo_root: &Path) {
    let gitignore = repo_root.join(".gitignore");
    let content = fs::read_to_string(&gitignore).unwrap_or_default();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == ".shorts/" || trimmed == ".shorts" {
            return;
        }
    }
    // Append the entry
    let mut new_content = content;
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push_str(".shorts/\n");
    if let Err(e) = fs::write(&gitignore, new_content) {
        log::warn!("failed to update .gitignore: {}", e);
    }
}

/// Remove the legacy single-file cache if it exists.
pub fn remove_legacy_cache(base: &Path) {
    let legacy = base.join(".shorts").join("cache.json");
    if legacy.exists() {
        if let Err(e) = fs::remove_file(&legacy) {
            log::warn!("failed to remove legacy cache: {}", e);
        }
    }
}

impl ImportCache {
    /// Create a new cache handle. No I/O is performed.
    pub fn new(cache_dir: Option<&Path>) -> Self {
        ImportCache {
            cache_dir: cache_dir.map(|p| p.to_path_buf()),
            accessed: Mutex::new(HashSet::new()),
        }
    }

    /// Look up a cached entry by its content hash key.
    ///
    /// Returns `None` if the cache is disabled, the file doesn't exist,
    /// deserialization fails, or the entry has no `symbol_hashes` (old format).
    /// Thread-safe: tracks accessed hashes via internal Mutex.
    pub fn get(&self, hash: u64) -> Option<CacheEntry> {
        let base = self.cache_dir.as_ref()?;
        let path = entry_path(base, hash);
        let data = fs::read_to_string(&path).ok()?;
        let entry: CacheEntry = serde_json::from_str(&data).ok()?;
        // Reject old-format entries that lack symbol hashes
        if entry.symbol_hashes.is_none() {
            return None;
        }
        self.accessed.lock().unwrap().insert(hash);
        Some(entry)
    }

    /// Write cache entries to disk as individual JSON files.
    ///
    /// Uses an atomic write pattern: writes to a `.tmp` file then renames.
    pub fn write_entries(&self, entries: Vec<(u64, CacheEntry)>) {
        let base = match self.cache_dir.as_ref() {
            Some(b) => b,
            None => return,
        };
        for (hash, entry) in entries {
            let path = entry_path(base, hash);
            if let Some(parent) = path.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    log::warn!("failed to create cache dir {}: {}", parent.display(), e);
                    continue;
                }
            }
            let tmp_path = path.with_extension("tmp");
            match serde_json::to_string(&entry) {
                Ok(json) => {
                    if let Err(e) = fs::write(&tmp_path, &json) {
                        log::warn!("failed to write cache tmp file: {}", e);
                        continue;
                    }
                    if let Err(e) = fs::rename(&tmp_path, &path) {
                        log::warn!("failed to rename cache file: {}", e);
                    }
                }
                Err(e) => log::warn!("failed to serialize cache entry: {}", e),
            }
        }
    }

    /// Write entries, mark them as accessed, then prune stale entries.
    pub fn write_entries_and_prune(&self, entries: Vec<(u64, CacheEntry)>) {
        {
            let mut accessed = self.accessed.lock().unwrap();
            for (hash, _) in &entries {
                accessed.insert(*hash);
            }
        }
        self.write_entries(entries);
        self.prune();
    }

    /// Probabilistic pruning: walk the cache directory and remove ~5% of
    /// entries that were not accessed in this session.
    fn prune(&self) {
        let base = match self.cache_dir.as_ref() {
            Some(b) => b,
            None => return,
        };
        let cache_root = base.join(".shorts").join("cache");
        if !cache_root.exists() {
            return;
        }
        let accessed = self.accessed.lock().unwrap();
        let mut unaccessed_count: u64 = 0;
        for entry in walkdir::WalkDir::new(&cache_root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            // Parse hash from filename stem
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s,
                None => continue,
            };
            let hash = match u64::from_str_radix(stem, 16) {
                Ok(h) => h,
                Err(_) => continue,
            };
            if accessed.contains(&hash) {
                continue;
            }
            unaccessed_count += 1;
            // Remove every 20th unaccessed entry (~5%)
            if unaccessed_count % 20 == 0 {
                let _ = fs::remove_file(path);
            }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn count_cache_files(dir: &Path) -> usize {
        let cache_root = dir.join(".shorts").join("cache");
        if !cache_root.exists() {
            return 0;
        }
        walkdir::WalkDir::new(&cache_root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    == Some("json")
            })
            .count()
    }

    fn make_entry(imports: Vec<&str>, hash: u64) -> CacheEntry {
        CacheEntry {
            imports: imports.into_iter().map(String::from).collect(),
            semantic_hash: hash,
            symbol_hashes: Some(HashMap::new()),
            symbol_usage: ModuleSymbolUsage::default(),
        }
    }

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

    #[test]
    fn test_cache_key_deterministic() {
        let content = b"import foo\nx = 1\n";
        let module = "my.module";
        assert_eq!(cache_key(content, module), cache_key(content, module));
    }

    #[test]
    fn test_cache_key_differs_for_different_content() {
        let module = "my.module";
        assert_ne!(
            cache_key(b"import foo\n", module),
            cache_key(b"import bar\n", module)
        );
    }

    #[test]
    fn test_cache_key_differs_for_different_module_class() {
        let content = b"import foo\n";
        assert_ne!(
            cache_key(content, "my.module"),
            cache_key(content, "other.module")
        );
    }

    #[test]
    fn test_cache_miss_on_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let cache = ImportCache::new(Some(tmp.path()));
        assert!(cache.get(12345).is_none());
    }

    #[test]
    fn test_cache_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let cache = ImportCache::new(Some(tmp.path()));

        let hash = cache_key(b"import foo\n", "my.module");
        let entry = make_entry(vec!["foo", "bar"], 42);

        cache.write_entries(vec![(hash, entry.clone())]);

        let retrieved = cache.get(hash).expect("entry should be present");
        assert_eq!(retrieved.imports, vec!["foo", "bar"]);
        assert_eq!(retrieved.semantic_hash, 42);
    }

    #[test]
    fn test_prune_removes_subset_of_unused() {
        let tmp = TempDir::new().unwrap();
        let cache = ImportCache::new(Some(tmp.path()));

        // Write 100 entries
        let entries: Vec<(u64, CacheEntry)> = (0..100u64)
            .map(|i| {
                let hash = cache_key(format!("content_{}", i).as_bytes(), "mod");
                (hash, make_entry(vec!["foo"], i))
            })
            .collect();

        let all_hashes: Vec<u64> = entries.iter().map(|(h, _)| *h).collect();
        cache.write_entries(entries);
        assert_eq!(count_cache_files(tmp.path()), 100);

        // Access 20 of them via get
        for &hash in &all_hashes[..20] {
            cache.get(hash);
        }

        // Prune should remove ~5% of the 80 unaccessed (~4 entries)
        cache.prune();

        let remaining = count_cache_files(tmp.path());
        // 20 accessed + ~76 unaccessed that survived = ~96
        assert!(
            remaining >= 93 && remaining <= 100,
            "expected ~96 remaining, got {}",
            remaining
        );
        assert!(remaining < 100, "prune should have removed at least one entry");
    }
}
