# Content-Addressable Multi-Level Cache Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace mtime-based single-file cache with a content-addressable, per-file, multi-level directory cache that supports cross-branch sharing, concurrent CI safety, and eliminates the parallel-blocking Mutex.

**Architecture:** Cache entries are keyed by xxh3 hash of (file content + module class name) and stored as individual JSON files under `.shorts/cache/{first_2_hex}/{full_hex}.json`. The module class is included in the key because `extract_imports` and `extract_symbol_usage` resolve relative imports based on the file's position in the package hierarchy — identical content at different paths produces different results. Each run reads every Python file, computes its key (cheap), looks up the per-file cache entry (one small file read on hit, skip on miss), and writes only new entries. The Mutex for collecting new entries is eliminated by collecting results as part of rayon's parallel output. 5% of unused cache entries are pruned each run.

**Tech Stack:** `xxhash-rust` (xxh3 feature) for content hashing, existing `serde_json` for per-entry serialization.

---

### Task 1: Add xxhash-rust dependency

**Files:**
- Modify: `Cargo.toml:6-13`

- [ ] **Step 1: Add the dependency**

Add `xxhash-rust` with the `xxh3` feature to `[dependencies]` in `Cargo.toml`:

```toml
xxhash-rust = { version = "0.8", features = ["xxh3"] }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check 2>&1 | tail -5`
Expected: compiles successfully (new dep downloaded)

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "deps: add xxhash-rust for content-addressable cache"
```

---

### Task 2: Rewrite CacheEntry and ImportCache for content-addressable per-file storage

This is the core change. The cache becomes a directory of small JSON files keyed by content hash, rather than a single monolithic JSON file.

**Files:**
- Modify: `src/cache.rs:1-244` (near-complete rewrite)
- Reference: `src/symbols.rs:554-567` (ModuleSymbolUsage, SymbolUsage — already Serialize/Deserialize)

- [ ] **Step 1: Write tests for the new cache API**

Add tests at the bottom of `src/cache.rs` (replacing existing `mod tests` block). Keep the existing `semantic_hash` tests and add:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Keep existing semantic_hash tests unchanged

    #[test]
    fn test_cache_key_deterministic() {
        let source = b"import foo\nx = 1\n";
        assert_eq!(cache_key(source, "myapp.utils"), cache_key(source, "myapp.utils"));
    }

    #[test]
    fn test_cache_key_differs_for_different_content() {
        assert_ne!(cache_key(b"x = 1", "mod"), cache_key(b"x = 2", "mod"));
    }

    #[test]
    fn test_cache_key_differs_for_different_module_class() {
        // Same content at different module paths must produce different keys
        // because relative imports resolve differently.
        let source = b"from . import foo\n";
        assert_ne!(cache_key(source, "myapp.utils"), cache_key(source, "other.utils"));
    }

    #[test]
    fn test_cache_miss_on_empty_dir() {
        let dir = TempDir::new().unwrap();
        let cache = ImportCache::new(Some(dir.path()));
        assert!(cache.get(12345).is_none());
    }

    #[test]
    fn test_cache_roundtrip() {
        let dir = TempDir::new().unwrap();
        let key = cache_key(b"import foo", "myapp.utils");
        let entry = CacheEntry {
            imports: vec!["foo".to_string()],
            semantic_hash: 42,
            symbol_hashes: Some([("fn:bar".to_string(), 99)].into()),
            symbol_usage: Default::default(),
        };
        {
            let cache = ImportCache::new(Some(dir.path()));
            cache.write_entries(vec![(key, entry.clone())]);
        }
        {
            let cache = ImportCache::new(Some(dir.path()));
            let got = cache.get(key).unwrap();
            assert_eq!(got.imports, entry.imports);
            assert_eq!(got.semantic_hash, entry.semantic_hash);
        }
    }

    #[test]
    fn test_prune_removes_subset_of_unused() {
        let dir = TempDir::new().unwrap();
        // Write 100 entries
        let cache = ImportCache::new(Some(dir.path()));
        let entries: Vec<(u64, CacheEntry)> = (0..100)
            .map(|i| {
                (i as u64, CacheEntry {
                    imports: vec![],
                    semantic_hash: 0,
                    symbol_hashes: Some(HashMap::new()),
                    symbol_usage: Default::default(),
                })
            })
            .collect();
        cache.write_entries(entries);

        // Access only 20 of them
        let cache2 = ImportCache::new(Some(dir.path()));
        for i in 0..20 {
            cache2.get(i as u64);
        }
        // Write no new entries, trigger prune
        cache2.write_entries_and_prune(vec![]);

        // Count remaining files
        let remaining = count_cache_files(dir.path());
        // 20 accessed + ~96 (80 unused, 5% = 4 removed) = ~96
        assert!(remaining >= 94 && remaining <= 100,
            "expected ~96 remaining, got {}", remaining);
    }

    fn count_cache_files(base: &Path) -> usize {
        let cache_dir = base.join(".shorts").join("cache");
        if !cache_dir.exists() { return 0; }
        walkdir::WalkDir::new(&cache_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|e| e.to_str()) == Some("json"))
            .count()
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib cache::tests 2>&1 | tail -20`
Expected: compilation errors (new types/functions don't exist yet)

- [ ] **Step 3: Implement the new cache module**

Rewrite `src/cache.rs`. The key changes:

1. `CacheEntry` — remove `mtime_secs`/`mtime_nanos`, add `symbol_usage: ModuleSymbolUsage`
2. `ImportCache` — backed by a cache directory, not an in-memory HashMap
3. `cache_key()` — public function using xxh3, hashes content + module class
4. `get()` — reads individual file from `.shorts/cache/ab/abcdef...json` by cache key, tracks accessed keys
5. `write_entries()` — writes new entries as individual files
6. `write_entries_and_prune()` — writes new entries, then prunes 5% of unaccessed files
7. `filter_semantically_changed()` and `detect_changed_symbols()` — remain as-is (they don't use cache entries)
8. Remove `merge()` (no longer needed)
9. Remove `insert()` (entries are written to disk directly)

```rust
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

const CACHE_DIR: &str = ".shorts";
const CACHE_SUBDIR: &str = "cache";

/// Compute a cache key from file content and module class name.
/// The module class is included because relative import resolution depends on
/// the file's position in the package hierarchy — identical content at
/// different paths produces different imports/symbol_usage.
pub fn cache_key(content: &[u8], module_class: &str) -> u64 {
    // Hash content first, then fold in the module class
    let content_hash = xxhash_rust::xxh3::xxh3_64(content);
    let module_hash = xxhash_rust::xxh3::xxh3_64(module_class.as_bytes());
    // Combine with xor + rotation to avoid symmetry
    content_hash ^ module_hash.rotate_left(32)
}

/// A cached analysis result for a single Python file, keyed by content hash.
#[derive(Serialize, Deserialize, Clone)]
pub struct CacheEntry {
    pub imports: Vec<String>,
    /// Semantic hash of non-trivia tokens (for upstream comparison).
    #[serde(default)]
    pub semantic_hash: u64,
    /// Per-symbol hashes for fine-grained change detection.
    #[serde(default)]
    pub symbol_hashes: Option<HashMap<String, u64>>,
    /// Which symbols this file uses from each imported module.
    #[serde(default)]
    pub symbol_usage: ModuleSymbolUsage,
}

/// Content-addressable cache backed by a multi-level directory.
/// Each entry is stored as `.shorts/cache/{hex[0..2]}/{hex}.json`.
pub struct ImportCache {
    cache_dir: Option<PathBuf>,
    accessed: Mutex<HashSet<u64>>,
}

// -- Semantic hashing (unchanged logic) --

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

// -- Path helpers --

fn hex_of(hash: u64) -> String {
    format!("{:016x}", hash)
}

fn entry_path(cache_base: &Path, hash: u64) -> PathBuf {
    let hex = hex_of(hash);
    cache_base
        .join(CACHE_DIR)
        .join(CACHE_SUBDIR)
        .join(&hex[..2])
        .join(format!("{}.json", hex))
}

impl ImportCache {
    /// Create a cache handle for the given base directory.
    /// Pass `None` to disable caching entirely.
    pub fn new(cache_dir: Option<&Path>) -> Self {
        ImportCache {
            cache_dir: cache_dir.map(|p| p.to_path_buf()),
            accessed: Mutex::new(HashSet::new()),
        }
    }

    /// Look up a cache entry by content hash.
    /// Returns `Some(entry)` on hit, `None` on miss.
    /// Thread-safe: multiple threads can call this concurrently.
    pub fn get(&self, hash: u64) -> Option<CacheEntry> {
        let base = self.cache_dir.as_ref()?;
        let path = entry_path(base, hash);
        let data = fs::read_to_string(&path).ok()?;
        let entry: CacheEntry = serde_json::from_str(&data).ok()?;

        // Only count entries that have symbol_usage populated
        // (reject old-format entries that lack it)
        if entry.symbol_hashes.is_none() {
            return None;
        }

        if let Ok(mut accessed) = self.accessed.lock() {
            accessed.insert(hash);
        }
        Some(entry)
    }

    /// Write new cache entries to disk as individual files.
    pub fn write_entries(&self, entries: Vec<(u64, CacheEntry)>) {
        let base = match &self.cache_dir {
            Some(b) => b,
            None => return,
        };
        for (hash, entry) in entries {
            let path = entry_path(base, hash);
            if let Some(parent) = path.parent() {
                if fs::create_dir_all(parent).is_err() {
                    log::warn!("failed to create cache dir: {}", parent.display());
                    continue;
                }
            }
            // Atomic write: write to temp file, then rename
            let tmp = path.with_extension("tmp");
            match serde_json::to_string(&entry) {
                Ok(json) => {
                    if fs::write(&tmp, &json).is_ok() {
                        if fs::rename(&tmp, &path).is_err() {
                            // Rename failed (cross-device?), fall back to direct write
                            let _ = fs::write(&path, &json);
                            let _ = fs::remove_file(&tmp);
                        }
                    }
                }
                Err(e) => log::warn!("failed to serialize cache entry: {}", e),
            }
        }
    }

    /// Write new entries and prune 5% of unaccessed entries.
    pub fn write_entries_and_prune(&self, entries: Vec<(u64, CacheEntry)>) {
        // Mark new entries as accessed
        if let Ok(mut accessed) = self.accessed.lock() {
            for (hash, _) in &entries {
                accessed.insert(*hash);
            }
        }

        self.write_entries(entries);
        self.prune();
    }

    /// Remove ~5% of cache entries that were not accessed this run.
    fn prune(&self) {
        let base = match &self.cache_dir {
            Some(b) => b,
            None => return,
        };
        let cache_root = base.join(CACHE_DIR).join(CACHE_SUBDIR);
        if !cache_root.exists() {
            return;
        }

        let accessed = match self.accessed.lock() {
            Ok(a) => a.clone(),
            Err(_) => return,
        };

        let mut count = 0u64;
        for entry in walkdir::WalkDir::new(&cache_root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.path().extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            // Parse hash from filename
            let stem = match entry.path().file_stem().and_then(|s| s.to_str()) {
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
            // Remove every 20th unaccessed entry (5%)
            count += 1;
            if count % 20 == 0 {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    // -- Methods that don't use cache storage (unchanged signatures) --

    /// Filter a set of changed files down to those that have actually changed
    /// semantically compared to the upstream git version.
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
                    Err(_) => return true,
                };
                let upstream_source = match repo.file_content_at_ref(path, upstream_ref) {
                    Some(s) => s,
                    None => return true,
                };
                semantic_hash(&current_source) != semantic_hash(&upstream_source)
            })
            .cloned()
            .collect()
    }

    /// Determine which specific symbols changed in each file compared to upstream.
    pub fn detect_changed_symbols(
        files: &HashSet<PathBuf>,
        repo: &GitRepo,
        upstream_ref: &str,
    ) -> (HashMap<PathBuf, HashSet<Symbol>>, HashSet<PathBuf>) {
        // Body unchanged from current implementation
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

            for (key, old_hash) in &upstream_keyed {
                match current_keyed.get(key) {
                    Some(new_hash) if new_hash == old_hash => {}
                    _ => {
                        if let Some(sym) = Symbol::from_cache_key(key) {
                            changed.insert(sym);
                        }
                    }
                }
            }

            for (key, _) in &current_keyed {
                if !upstream_keyed.contains_key(key) {
                    if let Some(sym) = Symbol::from_cache_key(key) {
                        changed.insert(sym);
                    }
                }
            }

            if !changed.is_empty() {
                let intra_deps = symbols::extract_intra_module_deps(&current_source);
                let propagated = symbols::propagate_intra_module_changes(&changed, &intra_deps);
                symbol_changes.insert(path.clone(), propagated);
            }
        }

        (symbol_changes, fallback_files)
    }
}

// -- Ensure .shorts/cache is gitignored --

/// Add `.shorts/` to `.gitignore` if not already present.
pub fn ensure_gitignored(repo_root: &Path) {
    let gitignore = repo_root.join(".gitignore");
    let content = fs::read_to_string(&gitignore).unwrap_or_default();
    if !content.lines().any(|l| l.trim() == ".shorts/" || l.trim() == ".shorts") {
        let entry = if content.ends_with('\n') || content.is_empty() {
            ".shorts/\n"
        } else {
            "\n.shorts/\n"
        };
        let _ = fs::write(&gitignore, format!("{}{}", content, entry));
    }
}
```

Note: `filter_semantically_changed` changes from `&self` method to associated function (no longer needs `&self` since it never used the cache entries).

- [ ] **Step 4: Run cache tests**

Run: `cargo test --lib cache::tests 2>&1 | tail -20`
Expected: all pass

- [ ] **Step 5: Commit**

```bash
git add src/cache.rs
git commit -m "feat: rewrite cache as content-addressable multi-level directory"
```

---

### Task 3: Update graph.rs, main.rs, and tests for new cache API

All callers are updated together to avoid broken intermediate commits. The key changes: eliminate the `Mutex<ImportCache>` for collecting new entries (rayon threads return results directly), use `cache_key(content, module_class)` for lookups, and simplify the `Trees::build` signature.

**Files:**
- Modify: `src/graph.rs:1-10` (imports), `src/graph.rs:90-239` (Tree::build and Trees::build)
- Modify: `src/main.rs:605-627`
- Modify: `tests/cache_consistency.rs:1-30` (helper functions)

- [ ] **Step 1: Update graph.rs imports**

Remove `use std::sync::Mutex;` from `src/graph.rs:4`. Keep all other imports.

- [ ] **Step 2: Update Tree::build and Trees::build**

Key changes to `Tree::build` (`src/graph.rs:90-215`):

1. Remove `Mutex<ImportCache>` parameter — new entries are collected via rayon's parallel output
2. Change visibility to `fn` (not `pub`) since it returns module-private `NewCacheEntry`
3. Read file content first, compute `cache_key(content, module_class)`, then check cache
4. On cache hit: use cached `imports` AND `symbol_usage` (no re-read!)
5. On cache miss: parse everything, return new CacheEntry alongside FileResult
6. Return new cache entries alongside the Tree

```rust
/// Per-file result from parallel parsing.
struct FileResult {
    module_class: String,
    deps: HashSet<String>,
    usage: symbols::ModuleSymbolUsage,
}

/// A new cache entry to be written to disk after the parallel phase.
struct NewCacheEntry {
    key: u64,
    entry: cache::CacheEntry,
}

impl Tree {
    /// Scan all Python files under `root` and build the reverse import graph.
    /// Returns the tree and any new cache entries that need to be written.
    fn build(
        root: PathBuf,
        namespace_packages: bool,
        cache: &ImportCache,
    ) -> (Self, Vec<NewCacheEntry>) {
        // Collect file paths first (WalkDir is not thread-safe)
        let paths: Vec<PathBuf> = WalkDir::new(&root)
            .into_iter()
            .filter_entry(|e| {
                if !e.file_type().is_dir() {
                    return true;
                }
                if namespace_packages {
                    return true;
                }
                e.path() == root.as_path() || e.path().join("__init__.py").exists()
            })
            .flatten()
            .filter(|e| {
                e.path().is_file()
                    && e.path().extension().and_then(|ext| ext.to_str()) == Some("py")
            })
            .map(|e| e.into_path())
            .collect();

        // Parse files in parallel — no Mutex needed, results collected via par_iter output
        let results: Vec<(FileResult, Option<NewCacheEntry>)> = paths
            .par_iter()
            .filter_map(|path| {
                let module_class = path_to_class(&root, path)?;
                let source = fs::read_to_string(path).ok()?;
                let key = cache::cache_key(source.as_bytes(), &module_class);

                if let Some(entry) = cache.get(key) {
                    // Cache hit: use cached imports and symbol usage
                    let fr = FileResult {
                        module_class,
                        deps: entry.imports.iter().cloned().collect(),
                        usage: entry.symbol_usage,
                    };
                    Some((fr, None))
                } else {
                    // Cache miss: parse everything
                    let deps = extract_imports(&module_class, &source);
                    let sem_hash = cache::semantic_hash(&source);
                    let sym_hashes = symbols::extract_symbol_hashes(&source);
                    let sym_hashes_keyed: HashMap<String, u64> = sym_hashes
                        .iter()
                        .map(|(sym, h)| (sym.cache_key(), *h))
                        .collect();
                    let usage = symbols::extract_symbol_usage(&module_class, &source);

                    let new_entry = NewCacheEntry {
                        key,
                        entry: cache::CacheEntry {
                            imports: deps.iter().cloned().collect(),
                            semantic_hash: sem_hash,
                            symbol_hashes: Some(sym_hashes_keyed),
                            symbol_usage: usage.clone(),
                        },
                    };
                    let fr = FileResult {
                        module_class,
                        deps,
                        usage,
                    };
                    Some((fr, Some(new_entry)))
                }
            })
            .collect();

        // Separate file results from new cache entries
        let mut new_entries = Vec::new();
        let mut importers: HashMap<String, HashSet<String>> = HashMap::new();
        let mut dependencies: HashMap<String, HashSet<String>> = HashMap::new();
        let mut symbol_importers: HashMap<(String, String), HashSet<String>> = HashMap::new();
        let mut all_importers: HashMap<String, HashSet<String>> = HashMap::new();

        for (fr, new_entry) in results {
            if let Some(ne) = new_entry {
                new_entries.push(ne);
            }
            // Build edges (same as current code)
            for dep in &fr.deps {
                importers.entry(dep.clone()).or_default().insert(fr.module_class.clone());
                dependencies.entry(fr.module_class.clone()).or_default().insert(dep.clone());
            }
            for (imported_module, usage) in &fr.usage.usage {
                match usage {
                    symbols::SymbolUsage::All => {
                        all_importers.entry(imported_module.clone()).or_default()
                            .insert(fr.module_class.clone());
                    }
                    symbols::SymbolUsage::Specific(syms) => {
                        for sym in syms {
                            symbol_importers
                                .entry((imported_module.clone(), sym.clone()))
                                .or_default()
                                .insert(fr.module_class.clone());
                        }
                    }
                }
            }
        }

        let tree = Tree {
            root,
            importers,
            dependencies,
            symbol_importers,
            all_importers,
        };
        (tree, new_entries)
    }
}
```

Update `Trees::build` (`src/graph.rs:218-239`):

```rust
impl Trees {
    pub fn build(
        roots: HashSet<PathBuf>,
        namespace_packages: bool,
        cache_dir: Option<&Path>,
    ) -> Self {
        let cache = ImportCache::new(cache_dir);

        let roots_vec: Vec<PathBuf> = roots.into_iter().collect();
        let results: Vec<(Tree, Vec<NewCacheEntry>)> = roots_vec
            .into_par_iter()
            .map(|root| {
                let root = root.canonicalize().unwrap_or(root);
                Tree::build(root, namespace_packages, &cache)
            })
            .collect();

        // Collect trees and all new cache entries
        let mut trees = Vec::new();
        let mut all_new_entries = Vec::new();
        for (tree, new_entries) in results {
            trees.push(tree);
            all_new_entries.extend(new_entries);
        }

        // Write new entries and prune
        let entries_to_write: Vec<(u64, cache::CacheEntry)> = all_new_entries
            .into_iter()
            .map(|ne| (ne.key, ne.entry))
            .collect();
        cache.write_entries_and_prune(entries_to_write);

        Trees { trees }
    }
    // ... rest of impl unchanged
}
```

- [ ] **Step 3: Update main.rs callers**

Replace lines ~605-627 of `src/main.rs`:

```rust
    // 6. Filter input files to only semantically changed ones
    let cache_dir = repo.as_ref().map(|r| r.root()).unwrap_or(&cwd);
    let input_files = if let (false, Some(r), Some(ref uref)) = (files_from_cli, &repo, &upstream_ref) {
        ImportCache::filter_semantically_changed(&input_files, r, uref)
    } else {
        input_files
    };
    log::debug!("semantically changed files: {:?}", input_files);

    // 6b. Detect which specific symbols changed
    let (symbol_changes, fallback_files) = if let (false, Some(r), Some(ref uref)) = (files_from_cli, &repo, &upstream_ref) {
        ImportCache::detect_changed_symbols(&input_files, r, uref)
    } else {
        (std::collections::HashMap::new(), input_files.clone())
    };
    log::debug!("symbol changes: {} files with symbol info, {} fallback files",
        symbol_changes.len(), fallback_files.len());

    // 7. Build trees
    let trees = Trees::build(python_roots.clone(), cli.namespace_packages, Some(cache_dir));
```

Key changes:
- Remove `let cache = ImportCache::load(cache_dir);`
- `cache.filter_semantically_changed(...)` → `ImportCache::filter_semantically_changed(...)`
- `Trees::build(roots, ns, cache, Some(cache_dir))` → `Trees::build(roots, ns, Some(cache_dir))`

- [ ] **Step 4: Update cache_consistency test helpers**

The test helpers need to match the new API. `ImportCache::load` and the `cache` parameter to `Trees::build` are gone. Replace the helper functions at the top of `tests/cache_consistency.rs`:

```rust
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use shorts::graph::Trees;
use shorts::symbols::Symbol;
use tempfile::TempDir;

fn abs_testdata(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/testdata")
        .join(rel)
        .canonicalize()
        .unwrap()
}

/// Build trees writing cache to the given directory.
fn build_with_cache_dir(roots: HashSet<PathBuf>, ns: bool, cache_dir: &std::path::Path) -> Trees {
    Trees::build(roots, ns, Some(cache_dir))
}

/// Build trees with a fresh (empty) cache, returning the trees and the populated cache dir.
fn build_with_fresh_cache(roots: HashSet<PathBuf>, ns: bool) -> (Trees, TempDir) {
    let cache_dir = TempDir::new().unwrap();
    let trees = build_with_cache_dir(roots, ns, cache_dir.path());
    (trees, cache_dir)
}

/// Build trees reusing an existing cache from a previous run.
fn build_with_warm_cache(roots: HashSet<PathBuf>, ns: bool, cache_dir: &std::path::Path) -> Trees {
    build_with_cache_dir(roots, ns, cache_dir)
}
```

The test bodies remain unchanged — they already test the right behavior (cold vs warm cache produces identical results).

- [ ] **Step 5: Verify all tests pass**

Run: `cargo test 2>&1 | tail -30`
Expected: all tests pass

- [ ] **Step 6: Commit**

```bash
git add src/graph.rs src/main.rs tests/cache_consistency.rs
git commit -m "feat: update graph, main, and tests for content-addressable cache"
```

---

### Task 4: Gitignore and legacy cache cleanup

Ensure `.shorts/` is in `.gitignore` and clean up the old single-file cache format.

**Files:**
- Modify: `src/cache.rs` (add migration in `new()` or a separate function)

- [ ] **Step 1: Wire up ensure_gitignored and add legacy cleanup**

Add to `main.rs` after `cache_dir` is determined:

```rust
cache::ensure_gitignored(cache_dir);
cache::remove_legacy_cache(cache_dir);
```

Add the `remove_legacy_cache` function in `cache.rs`:

```rust
/// Remove legacy single-file cache if present.
pub fn remove_legacy_cache(base: &Path) {
    let legacy = base.join(".shorts").join("cache.json");
    if legacy.exists() {
        log::info!("removing legacy cache file: {}", legacy.display());
        let _ = fs::remove_file(&legacy);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test 2>&1 | tail -10`
Expected: all pass

- [ ] **Step 3: Commit**

```bash
git add src/cache.rs src/main.rs
git commit -m "chore: gitignore .shorts/ and remove legacy cache on upgrade"
```

---

### Task 5: End-to-end verification

- [ ] **Step 1: Build release binary**

Run: `cargo build --release 2>&1 | tail -5`
Expected: compiles successfully

- [ ] **Step 2: Run full test suite**

Run: `cargo test 2>&1`
Expected: all tests pass

- [ ] **Step 3: Manual smoke test with testdata**

Run against the test fixtures to verify cache files are created:

```bash
./target/release/shorts --root tests/testdata/simple tests/testdata/simple/myapp/utils.py --verbose 2>&1
ls -la tests/testdata/simple/.shorts/cache/
```

Expected: cache directory with hex-named subdirectories containing `.json` files

- [ ] **Step 4: Verify warm cache run is faster**

```bash
time ./target/release/shorts --root tests/testdata/simple tests/testdata/simple/myapp/utils.py 2>/dev/null
time ./target/release/shorts --root tests/testdata/simple tests/testdata/simple/myapp/utils.py 2>/dev/null
```

Expected: second run should be noticeably faster (no parsing on cache hit)

- [ ] **Step 5: Clean up test artifacts**

```bash
rm -rf tests/testdata/simple/.shorts
```

- [ ] **Step 6: Final commit if any cleanup needed**
