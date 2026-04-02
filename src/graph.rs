use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rayon::prelude::*;
use walkdir::WalkDir;

use crate::cache::{self, ImportCache};
use crate::imports::extract_imports;
use crate::symbols::{self, Symbol, SymbolUsage};

/// Format a set of symbols into a human-readable string like "foo(), Bar, CONST".
fn format_symbol_list(symbols: &HashSet<Symbol>) -> String {
    let mut parts: Vec<String> = symbols
        .iter()
        .map(|s| match s {
            Symbol::Function(n) => format!("{n}()"),
            Symbol::Class(n) => n.clone(),
            Symbol::Variable(n) => n.clone(),
            Symbol::ModuleBody => "module body".to_string(),
        })
        .collect();
    parts.sort();
    parts.join(", ")
}

/// A single-root dependency tree.
/// Maps class names to the set of classes that import them (reverse edges).
pub struct Tree {
    pub root: PathBuf,
    /// Module-level reverse edges (existing)
    pub importers: HashMap<String, HashSet<String>>,
    /// Module-level forward edges: module -> set of modules it imports
    pub dependencies: HashMap<String, HashSet<String>>,
    /// Symbol-level reverse edges:
    /// (imported_module, symbol_name) -> set of importer module names
    pub symbol_importers: HashMap<(String, String), HashSet<String>>,
    /// Modules that depend on ALL symbols of a given module
    /// (star imports, getattr, module escaping)
    pub all_importers: HashMap<String, HashSet<String>>,
}

/// Collection of trees for multi-root support.
pub struct Trees {
    pub trees: Vec<Tree>,
}

/// Convert file path to Python dotted class name relative to root.
///
/// `/project/myapp/utils.py` with root `/project` -> `"myapp.utils"`
/// `/project/myapp/__init__.py` with root `/project` -> `"myapp.__init__"`
pub fn path_to_class(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let s = rel.to_str()?;
    if !s.ends_with(".py") {
        return None;
    }
    let trimmed = &s[..s.len() - 3]; // strip ".py"
    let dotted = trimmed.replace('/', ".");
    if dotted.is_empty() {
        return None;
    }
    Some(dotted)
}

/// Reverse of path_to_class. Convert a dotted class name to a file path under root.
///
/// Checks if class is a package (directory with `__init__.py`) first, then module (`.py` file).
/// Returns None if neither exists on disk.
pub fn class_to_path(root: &Path, class: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = class.split('.').collect();
    let rel_path = parts.join("/");

    // Try package: root/a/b/__init__.py
    let package_path = root.join(&rel_path).join("__init__.py");
    if package_path.exists() {
        return Some(package_path);
    }

    // Try module: root/a/b.py
    let module_path = root.join(format!("{}.py", rel_path));
    if module_path.exists() {
        return Some(module_path);
    }

    None
}

impl Tree {
    /// Scan all Python files under `root` and build the reverse import graph.
    /// Uses the provided cache to skip re-parsing unchanged files.
    pub fn build(root: PathBuf, namespace_packages: bool, cache: &ImportCache, new_entries: &Mutex<ImportCache>) -> Self {
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
                // Without namespace packages, skip dirs without __init__.py
                e.path() == root.as_path() || e.path().join("__init__.py").exists()
            })
            .flatten()
            .filter(|e| {
                e.path().is_file() && e.path().extension().and_then(|ext| ext.to_str()) == Some("py")
            })
            .map(|e| e.into_path())
            .collect();

        /// Per-file parsing result for parallel collection
        struct FileResult {
            module_class: String,
            deps: HashSet<String>,
            usage: symbols::ModuleSymbolUsage,
        }

        // Parse files in parallel, using cache for unchanged files
        let file_results: Vec<FileResult> = paths
            .par_iter()
            .filter_map(|path| {
                let module_class = path_to_class(&root, path)?;

                // Check cache first for imports
                let (deps, source_opt) = if let Some(cached_imports) = cache.get(path) {
                    (cached_imports.iter().cloned().collect::<HashSet<_>>(), None)
                } else {
                    let source = fs::read_to_string(path).ok()?;
                    let deps = extract_imports(&module_class, &source);
                    let hash = cache::semantic_hash(&source);
                    let sym_hashes = symbols::extract_symbol_hashes(&source);
                    let sym_hashes_keyed: HashMap<String, u64> = sym_hashes
                        .iter()
                        .map(|(sym, h)| (sym.cache_key(), *h))
                        .collect();
                    if let Ok(mut entries) = new_entries.lock() {
                        entries.insert(
                            path.clone(),
                            deps.iter().cloned().collect(),
                            hash,
                            sym_hashes_keyed,
                        );
                    }
                    (deps, Some(source))
                };

                // Extract symbol usage (need source for this)
                let usage = if let Some(source) = source_opt {
                    symbols::extract_symbol_usage(&module_class, &source)
                } else {
                    // Re-read for symbol usage if we used cached imports
                    match fs::read_to_string(path) {
                        Ok(source) => symbols::extract_symbol_usage(&module_class, &source),
                        Err(_) => symbols::ModuleSymbolUsage::default(),
                    }
                };

                Some(FileResult {
                    module_class,
                    deps,
                    usage,
                })
            })
            .collect();

        let mut importers: HashMap<String, HashSet<String>> = HashMap::new();
        let mut dependencies: HashMap<String, HashSet<String>> = HashMap::new();
        let mut symbol_importers: HashMap<(String, String), HashSet<String>> = HashMap::new();
        let mut all_importers: HashMap<String, HashSet<String>> = HashMap::new();

        for fr in file_results {
            // Module-level edges (existing reverse + new forward)
            for dep in &fr.deps {
                importers
                    .entry(dep.clone())
                    .or_default()
                    .insert(fr.module_class.clone());
                dependencies
                    .entry(fr.module_class.clone())
                    .or_default()
                    .insert(dep.clone());
            }

            // Symbol-level edges
            for (imported_module, usage) in &fr.usage.usage {
                match usage {
                    SymbolUsage::All => {
                        all_importers
                            .entry(imported_module.clone())
                            .or_default()
                            .insert(fr.module_class.clone());
                    }
                    SymbolUsage::Specific(symbols) => {
                        for sym in symbols {
                            symbol_importers
                                .entry((imported_module.clone(), sym.clone()))
                                .or_default()
                                .insert(fr.module_class.clone());
                        }
                    }
                }
            }
        }

        Tree {
            root,
            importers,
            dependencies,
            symbol_importers,
            all_importers,
        }
    }
}

impl Trees {
    pub fn build(roots: HashSet<PathBuf>, namespace_packages: bool, cache: ImportCache, cache_dir: Option<&Path>) -> Self {
        let new_entries = Mutex::new(ImportCache::default());

        let roots_vec: Vec<PathBuf> = roots.into_iter().collect();
        let trees: Vec<Tree> = roots_vec
            .into_par_iter()
            .map(|root| {
                let root = root.canonicalize().unwrap_or(root);
                Tree::build(root, namespace_packages, &cache, &new_entries)
            })
            .collect();

        // Save updated cache
        if let Some(dir) = cache_dir {
            let mut final_cache = cache;
            final_cache.merge(new_entries.into_inner().unwrap_or_default());
            final_cache.save(dir);
        }

        Trees { trees }
    }

    /// Find the tree with the longest root that is a prefix of path,
    /// then use that root to convert path to a class name.
    pub fn path_to_class_across_trees(&self, path: &Path) -> Option<String> {
        self.trees
            .iter()
            .filter(|t| path.starts_with(&t.root))
            .max_by_key(|t| t.root.as_os_str().len())
            .and_then(|t| path_to_class(&t.root, path))
    }

    /// Union of importers from ALL trees for a given class.
    pub fn get_importers_across_trees(&self, class: &str) -> HashSet<String> {
        let mut result = HashSet::new();
        for tree in &self.trees {
            if let Some(imps) = tree.importers.get(class) {
                result.extend(imps.iter().cloned());
            }
        }
        result
    }

    /// Try class_to_path against each tree's root, return first match.
    pub fn class_to_path_across_trees(&self, class: &str) -> Option<PathBuf> {
        for tree in &self.trees {
            if let Some(p) = class_to_path(&tree.root, class) {
                return Some(p);
            }
        }
        None
    }

    /// Union of forward dependencies from ALL trees for a given class.
    pub fn get_dependencies_across_trees(&self, class: &str) -> HashSet<String> {
        let mut result = HashSet::new();
        for tree in &self.trees {
            if let Some(deps) = tree.dependencies.get(class) {
                result.extend(deps.iter().cloned());
            }
        }
        result
    }

    /// BFS to find all transitive forward dependencies of the given input files.
    ///
    /// Returns the set of file paths that the input files transitively import.
    pub fn get_dependencies(&self, input_files: &HashSet<PathBuf>) -> HashSet<PathBuf> {
        let input_classes: HashSet<String> = input_files
            .iter()
            .filter_map(|p| self.path_to_class_across_trees(p))
            .collect();

        let mut seen: HashSet<String> = HashSet::new();
        let mut pending = input_classes;

        while !pending.is_empty() {
            let mut next = HashSet::new();
            for class in &pending {
                if !seen.insert(class.clone()) {
                    continue;
                }
                for dep in self.get_dependencies_across_trees(class) {
                    if !seen.contains(&dep) {
                        next.insert(dep);
                    }
                }
            }
            pending = next;
        }

        seen.iter()
            .filter_map(|c| self.class_to_path_across_trees(c))
            .collect()
    }

    /// BFS to find all transitive dependees of the given input files.
    ///
    /// Returns the set of file paths (including input files) that transitively
    /// depend on the input files.
    pub fn get_dependees(&self, input_files: &HashSet<PathBuf>) -> HashSet<PathBuf> {
        // Convert input file paths to class names
        let input_classes: HashSet<String> = input_files
            .iter()
            .filter_map(|p| self.path_to_class_across_trees(p))
            .collect();

        // BFS
        let mut seen: HashSet<String> = HashSet::new();
        let mut pending = input_classes;

        while !pending.is_empty() {
            let mut next = HashSet::new();
            for class in &pending {
                if !seen.insert(class.clone()) {
                    continue;
                }
                for imp in self.get_importers_across_trees(class) {
                    if !seen.contains(&imp) {
                        next.insert(imp);
                    }
                }
            }
            pending = next;
        }

        // Convert all seen classes to file paths
        seen.iter()
            .filter_map(|c| self.class_to_path_across_trees(c))
            .collect()
    }

    /// Call get_dependees once per input file to track which input triggered each dependee.
    ///
    /// Returns map from dependee path to list of triggering input files.
    pub fn get_dependees_explained(
        &self,
        input_files: &HashSet<PathBuf>,
    ) -> HashMap<PathBuf, Vec<PathBuf>> {
        let input_vec: Vec<PathBuf> = input_files.iter().cloned().collect();

        // Run BFS for each input file in parallel
        let per_file_results: Vec<(PathBuf, HashSet<PathBuf>)> = input_vec
            .into_par_iter()
            .map(|input_file| {
                let single: HashSet<PathBuf> = [input_file.clone()].into();
                let dependees = self.get_dependees(&single);
                (input_file, dependees)
            })
            .collect();

        let mut result: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        for (input_file, dependees) in per_file_results {
            for dep in dependees {
                result.entry(dep).or_default().push(input_file.clone());
            }
        }

        result
    }

    /// Get symbol-level importers across all trees for a (module, symbol) pair.
    fn get_symbol_importers_across_trees(&self, module: &str, symbol: &str) -> HashSet<String> {
        let mut result = HashSet::new();
        let key = (module.to_string(), symbol.to_string());
        for tree in &self.trees {
            if let Some(imps) = tree.symbol_importers.get(&key) {
                result.extend(imps.iter().cloned());
            }
        }
        result
    }

    /// Get all-importers (star imports, getattr, module escaping) across all trees.
    fn get_all_importers_across_trees(&self, module: &str) -> HashSet<String> {
        let mut result = HashSet::new();
        for tree in &self.trees {
            if let Some(imps) = tree.all_importers.get(module) {
                result.extend(imps.iter().cloned());
            }
        }
        result
    }

    /// Symbol-aware BFS to find transitive dependees.
    ///
    /// For files with known changed symbols: only flag importers that use those
    /// specific symbols (plus star/getattr importers). ModuleBody changes flag
    /// all importers.
    ///
    /// For fallback files (no symbol info): use full module-level BFS.
    ///
    /// After the first hop, transitive propagation is conservative (full-module).
    pub fn get_dependees_symbol_aware(
        &self,
        symbol_changes: &HashMap<PathBuf, HashSet<Symbol>>,
        fallback_files: &HashSet<PathBuf>,
    ) -> HashSet<PathBuf> {
        // Convert to class-based inputs
        let mut class_symbol_changes: HashMap<String, HashSet<Symbol>> = HashMap::new();
        for (path, syms) in symbol_changes {
            if let Some(module) = self.path_to_class_across_trees(path) {
                class_symbol_changes.insert(module, syms.clone());
            }
        }
        let mut fallback_classes: HashSet<String> = HashSet::new();
        for path in fallback_files {
            if let Some(module) = self.path_to_class_across_trees(path) {
                fallback_classes.insert(module);
            }
        }

        let seen = self.get_dependee_classes_symbol_aware(&class_symbol_changes, &fallback_classes);

        // Convert to file paths
        seen.iter()
            .filter_map(|c| self.class_to_path_across_trees(c))
            .collect()
    }

    /// Class-based symbol-aware BFS. Returns the set of affected class names.
    fn get_dependee_classes_symbol_aware(
        &self,
        symbol_changes: &HashMap<String, HashSet<Symbol>>,
        fallback_classes: &HashSet<String>,
    ) -> HashSet<String> {
        let mut first_hop_classes: HashSet<String> = HashSet::new();
        let mut seed_classes: HashSet<String> = HashSet::new();

        // Symbol-aware first hop
        for (module, changed_symbols) in symbol_changes {
            seed_classes.insert(module.clone());

            let mut needs_full_module = false;
            for sym in changed_symbols {
                match sym {
                    Symbol::ModuleBody => {
                        needs_full_module = true;
                        break;
                    }
                    Symbol::Function(name) | Symbol::Class(name) | Symbol::Variable(name) => {
                        let importers = self.get_symbol_importers_across_trees(module, name);
                        first_hop_classes.extend(importers);
                    }
                }
            }

            // Always include star/getattr importers
            let all_imps = self.get_all_importers_across_trees(module);
            first_hop_classes.extend(all_imps);

            if needs_full_module {
                let module_imps = self.get_importers_across_trees(module);
                first_hop_classes.extend(module_imps);
            }
        }

        // Fallback classes: full module-level first hop
        for module in fallback_classes {
            seed_classes.insert(module.clone());
            let module_imps = self.get_importers_across_trees(module);
            first_hop_classes.extend(module_imps);
        }

        // Conservative transitive propagation from first-hop results
        let mut seen: HashSet<String> = HashSet::new();
        seen.extend(seed_classes);

        let mut pending = first_hop_classes;

        while !pending.is_empty() {
            let mut next = HashSet::new();
            for class in &pending {
                if !seen.insert(class.clone()) {
                    continue;
                }
                for imp in self.get_importers_across_trees(class) {
                    if !seen.contains(&imp) {
                        next.insert(imp);
                    }
                }
            }
            pending = next;
        }

        seen
    }

    /// Symbol-aware BFS that also returns one reason per dependee explaining
    /// why it was included.
    pub fn get_dependees_symbol_aware_with_reasons(
        &self,
        symbol_changes: &HashMap<PathBuf, HashSet<Symbol>>,
        fallback_files: &HashSet<PathBuf>,
    ) -> HashMap<PathBuf, String> {
        // Convert to class-based inputs
        let mut class_symbol_changes: HashMap<String, HashSet<Symbol>> = HashMap::new();
        for (path, syms) in symbol_changes {
            if let Some(module) = self.path_to_class_across_trees(path) {
                class_symbol_changes.insert(module, syms.clone());
            }
        }
        let mut fallback_classes: HashSet<String> = HashSet::new();
        for path in fallback_files {
            if let Some(module) = self.path_to_class_across_trees(path) {
                fallback_classes.insert(module);
            }
        }

        let reasons = self.get_dependee_classes_with_reasons(&class_symbol_changes, &fallback_classes);

        // Convert class names to file paths
        reasons
            .into_iter()
            .filter_map(|(class, reason)| {
                self.class_to_path_across_trees(&class).map(|p| (p, reason))
            })
            .collect()
    }

    /// Class-based symbol-aware BFS returning (class -> reason) map.
    fn get_dependee_classes_with_reasons(
        &self,
        symbol_changes: &HashMap<String, HashSet<Symbol>>,
        fallback_classes: &HashSet<String>,
    ) -> HashMap<String, String> {
        // reason for each class
        let mut reasons: HashMap<String, String> = HashMap::new();
        // first-hop classes with their reasons
        let mut first_hop: HashMap<String, String> = HashMap::new();

        // Symbol-aware first hop
        for (module, changed_symbols) in symbol_changes {
            let sym_desc = format_symbol_list(changed_symbols);
            reasons.insert(module.clone(), format!("{sym_desc} changed"));

            let mut needs_full_module = false;
            for sym in changed_symbols {
                match sym {
                    Symbol::ModuleBody => {
                        needs_full_module = true;
                        break;
                    }
                    Symbol::Function(name) | Symbol::Class(name) | Symbol::Variable(name) => {
                        for imp in self.get_symbol_importers_across_trees(module, name) {
                            first_hop.entry(imp).or_insert_with(|| {
                                format!("uses {module}.{name}")
                            });
                        }
                    }
                }
            }

            for imp in self.get_all_importers_across_trees(module) {
                first_hop.entry(imp).or_insert_with(|| {
                    format!("star-imports {module}")
                });
            }

            if needs_full_module {
                for imp in self.get_importers_across_trees(module) {
                    first_hop.entry(imp).or_insert_with(|| {
                        format!("imports {module} (module body changed)")
                    });
                }
            }
        }

        // Fallback classes: full module-level first hop
        for module in fallback_classes {
            reasons.insert(module.clone(), "changed".to_string());
            for imp in self.get_importers_across_trees(module) {
                first_hop.entry(imp).or_insert_with(|| {
                    format!("imports {module}")
                });
            }
        }

        // Conservative transitive propagation
        let mut pending = first_hop;

        while !pending.is_empty() {
            let mut next: HashMap<String, String> = HashMap::new();
            for (class, reason) in pending {
                if reasons.contains_key(&class) {
                    continue;
                }
                reasons.insert(class.clone(), reason);
                for imp in self.get_importers_across_trees(&class) {
                    if !reasons.contains_key(&imp) {
                        next.entry(imp).or_insert_with(|| {
                            format!("imports {class}")
                        });
                    }
                }
            }
            pending = next;
        }

        reasons
    }

    /// Symbol-aware version of get_dependees_explained.
    pub fn get_dependees_symbol_aware_explained(
        &self,
        symbol_changes: &HashMap<PathBuf, HashSet<Symbol>>,
        fallback_files: &HashSet<PathBuf>,
    ) -> HashMap<PathBuf, Vec<PathBuf>> {
        // Run per input file
        let all_input_files: Vec<PathBuf> = symbol_changes
            .keys()
            .chain(fallback_files.iter())
            .cloned()
            .collect();

        let per_file_results: Vec<(PathBuf, HashSet<PathBuf>)> = all_input_files
            .into_par_iter()
            .map(|input_file| {
                let (sc, fb) = if let Some(syms) = symbol_changes.get(&input_file) {
                    let mut sc = HashMap::new();
                    sc.insert(input_file.clone(), syms.clone());
                    (sc, HashSet::new())
                } else {
                    let mut fb = HashSet::new();
                    fb.insert(input_file.clone());
                    (HashMap::new(), fb)
                };
                let dependees = self.get_dependees_symbol_aware(&sc, &fb);
                (input_file, dependees)
            })
            .collect();

        let mut result: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        for (input_file, dependees) in per_file_results {
            for dep in dependees {
                result.entry(dep).or_default().push(input_file.clone());
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tree(root: &str, edges: &[(&str, &str)]) -> Tree {
        let mut importers: HashMap<String, HashSet<String>> = HashMap::new();
        let mut dependencies: HashMap<String, HashSet<String>> = HashMap::new();
        for (imported, importer) in edges {
            importers
                .entry(imported.to_string())
                .or_default()
                .insert(importer.to_string());
            dependencies
                .entry(importer.to_string())
                .or_default()
                .insert(imported.to_string());
        }
        Tree {
            root: PathBuf::from(root),
            importers,
            dependencies,
            symbol_importers: HashMap::new(),
            all_importers: HashMap::new(),
        }
    }

    #[test]
    fn test_path_to_class() {
        assert_eq!(
            path_to_class(Path::new("/project"), Path::new("/project/myapp/utils.py")),
            Some("myapp.utils".to_string())
        );
        assert_eq!(
            path_to_class(
                Path::new("/project"),
                Path::new("/project/myapp/__init__.py")
            ),
            Some("myapp.__init__".to_string())
        );
    }

    #[test]
    fn test_path_to_class_not_python() {
        assert_eq!(
            path_to_class(Path::new("/project"), Path::new("/project/myapp/utils.rs")),
            None
        );
    }

    #[test]
    fn test_path_to_class_wrong_root() {
        assert_eq!(
            path_to_class(
                Path::new("/other"),
                Path::new("/project/myapp/utils.py")
            ),
            None
        );
    }

    #[test]
    fn test_path_to_class_across_trees_longest_prefix() {
        let tree1 = make_tree("/repo", &[]);
        let tree2 = make_tree("/repo/py/kafka/src", &[]);
        let trees = Trees {
            trees: vec![tree1, tree2],
        };

        let class = trees
            .path_to_class_across_trees(Path::new("/repo/py/kafka/src/avn/kafka/consumer.py"));
        assert_eq!(class, Some("avn.kafka.consumer".to_string()));
    }

    #[test]
    fn test_get_importers_across_trees() {
        let tree1 = make_tree("/repo", &[("avn.kafka.consumer", "aiven.acorn.api")]);
        let tree2 = make_tree("/repo/py/kafka/src", &[("avn.kafka.consumer", "avn.kafka.producer")]);
        let trees = Trees {
            trees: vec![tree1, tree2],
        };

        let importers = trees.get_importers_across_trees("avn.kafka.consumer");
        assert!(importers.contains("aiven.acorn.api"));
        assert!(importers.contains("avn.kafka.producer"));
        assert_eq!(importers.len(), 2);
    }

    #[test]
    fn test_bfs_transitive() {
        // a imports b, b imports c -> changing a should flag b and c
        let tree = make_tree("/project", &[("myapp.a", "myapp.b"), ("myapp.b", "myapp.c")]);
        let trees = Trees {
            trees: vec![tree],
        };

        // Test the BFS logic directly by checking seen classes
        let input_classes: HashSet<String> = ["myapp.a".to_string()].into();
        let mut seen: HashSet<String> = HashSet::new();
        let mut pending = input_classes.clone();

        while !pending.is_empty() {
            let mut next = HashSet::new();
            for class in &pending {
                if !seen.insert(class.clone()) {
                    continue;
                }
                for imp in trees.get_importers_across_trees(class) {
                    if !seen.contains(&imp) {
                        next.insert(imp);
                    }
                }
            }
            pending = next;
        }

        assert!(seen.contains("myapp.a"));
        assert!(seen.contains("myapp.b"));
        assert!(seen.contains("myapp.c"));
    }

    #[test]
    fn test_bfs_no_importers() {
        let tree = make_tree("/project", &[("myapp.a", "myapp.b")]);
        let trees = Trees {
            trees: vec![tree],
        };

        let input_classes: HashSet<String> = ["myapp.b".to_string()].into();
        let mut seen: HashSet<String> = HashSet::new();
        let mut pending = input_classes.clone();

        while !pending.is_empty() {
            let mut next = HashSet::new();
            for class in &pending {
                if !seen.insert(class.clone()) {
                    continue;
                }
                for imp in trees.get_importers_across_trees(class) {
                    if !seen.contains(&imp) {
                        next.insert(imp);
                    }
                }
            }
            pending = next;
        }

        // myapp.b has no importers, so only itself
        assert!(seen.contains("myapp.b"));
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn test_bfs_cycle() {
        // a -> b -> c -> a (cycle)
        let tree = make_tree(
            "/project",
            &[
                ("myapp.a", "myapp.b"),
                ("myapp.b", "myapp.c"),
                ("myapp.c", "myapp.a"),
            ],
        );
        let trees = Trees {
            trees: vec![tree],
        };

        let input_classes: HashSet<String> = ["myapp.a".to_string()].into();
        let mut seen: HashSet<String> = HashSet::new();
        let mut pending = input_classes.clone();

        while !pending.is_empty() {
            let mut next = HashSet::new();
            for class in &pending {
                if !seen.insert(class.clone()) {
                    continue;
                }
                for imp in trees.get_importers_across_trees(class) {
                    if !seen.contains(&imp) {
                        next.insert(imp);
                    }
                }
            }
            pending = next;
        }

        assert!(seen.contains("myapp.a"));
        assert!(seen.contains("myapp.b"));
        assert!(seen.contains("myapp.c"));
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn test_get_importers_across_trees_empty() {
        let tree = make_tree("/project", &[("myapp.a", "myapp.b")]);
        let trees = Trees {
            trees: vec![tree],
        };

        let importers = trees.get_importers_across_trees("myapp.nonexistent");
        assert!(importers.is_empty());
    }

    /// Build a tree with symbol-level edges for testing.
    /// `symbol_edges`: (imported_module, symbol_name, importer_module)
    /// `all_edges`: (imported_module, importer_module) — star/getattr importers
    fn make_tree_with_symbols(
        root: &str,
        module_edges: &[(&str, &str)],
        symbol_edges: &[(&str, &str, &str)],
        all_edges: &[(&str, &str)],
    ) -> Tree {
        let mut importers: HashMap<String, HashSet<String>> = HashMap::new();
        let mut dependencies: HashMap<String, HashSet<String>> = HashMap::new();
        for (imported, importer) in module_edges {
            importers
                .entry(imported.to_string())
                .or_default()
                .insert(importer.to_string());
            dependencies
                .entry(importer.to_string())
                .or_default()
                .insert(imported.to_string());
        }
        let mut symbol_importers: HashMap<(String, String), HashSet<String>> = HashMap::new();
        for (module, symbol, importer) in symbol_edges {
            symbol_importers
                .entry((module.to_string(), symbol.to_string()))
                .or_default()
                .insert(importer.to_string());
        }
        let mut all_importers: HashMap<String, HashSet<String>> = HashMap::new();
        for (module, importer) in all_edges {
            all_importers
                .entry(module.to_string())
                .or_default()
                .insert(importer.to_string());
        }
        Tree {
            root: PathBuf::from(root),
            importers,
            dependencies,
            symbol_importers,
            all_importers,
        }
    }

    #[test]
    fn test_symbol_aware_only_affected_importers() {
        // Module A has symbols foo, bar
        // Module B imports A.foo, Module C imports A.bar
        // If only A.foo changes, only B should be flagged
        let tree = make_tree_with_symbols(
            "/project",
            &[("myapp.a", "myapp.b"), ("myapp.a", "myapp.c")],
            &[
                ("myapp.a", "foo", "myapp.b"),
                ("myapp.a", "bar", "myapp.c"),
            ],
            &[],
        );
        let trees = Trees { trees: vec![tree] };

        let mut changes = HashMap::new();
        changes.insert(
            "myapp.a".to_string(),
            [Symbol::Function("foo".to_string())].into(),
        );

        let result = trees.get_dependee_classes_symbol_aware(&changes, &HashSet::new());
        assert!(result.contains("myapp.a"));
        assert!(result.contains("myapp.b"));
        assert!(!result.contains("myapp.c"));
    }

    #[test]
    fn test_symbol_aware_star_import_always_flagged() {
        let tree = make_tree_with_symbols(
            "/project",
            &[("myapp.a", "myapp.b"), ("myapp.a", "myapp.c")],
            &[("myapp.a", "foo", "myapp.b")],
            &[("myapp.a", "myapp.c")], // C has star import
        );
        let trees = Trees { trees: vec![tree] };

        let mut changes = HashMap::new();
        changes.insert(
            "myapp.a".to_string(),
            [Symbol::Function("bar".to_string())].into(), // bar changed, B uses foo
        );

        let result = trees.get_dependee_classes_symbol_aware(&changes, &HashSet::new());
        assert!(result.contains("myapp.a"));
        assert!(!result.contains("myapp.b"), "B uses foo, not bar");
        assert!(result.contains("myapp.c"), "C has star import, always flagged");
    }

    #[test]
    fn test_symbol_aware_module_body_flags_all() {
        let tree = make_tree_with_symbols(
            "/project",
            &[("myapp.a", "myapp.b"), ("myapp.a", "myapp.c")],
            &[("myapp.a", "foo", "myapp.b")],
            &[],
        );
        let trees = Trees { trees: vec![tree] };

        let mut changes = HashMap::new();
        changes.insert("myapp.a".to_string(), [Symbol::ModuleBody].into());

        let result = trees.get_dependee_classes_symbol_aware(&changes, &HashSet::new());
        assert!(result.contains("myapp.b"));
        assert!(result.contains("myapp.c"));
    }

    #[test]
    fn test_symbol_aware_transitive_propagation() {
        // A.foo changes → B (uses foo) flagged → C (imports B) flagged transitively
        let tree = make_tree_with_symbols(
            "/project",
            &[
                ("myapp.a", "myapp.b"),
                ("myapp.a", "myapp.d"),
                ("myapp.b", "myapp.c"),
            ],
            &[("myapp.a", "foo", "myapp.b")],
            &[],
        );
        let trees = Trees { trees: vec![tree] };

        let mut changes = HashMap::new();
        changes.insert(
            "myapp.a".to_string(),
            [Symbol::Function("foo".to_string())].into(),
        );

        let result = trees.get_dependee_classes_symbol_aware(&changes, &HashSet::new());
        assert!(result.contains("myapp.a"));
        assert!(result.contains("myapp.b"), "uses foo directly");
        assert!(result.contains("myapp.c"), "imports B transitively");
        assert!(!result.contains("myapp.d"), "uses A but not foo");
    }

    #[test]
    fn test_forward_dependencies() {
        // a imports b, b imports c -> forward deps of c = {c, b, a} (c depends on nothing it imports)
        // Actually: edges are (imported, importer), so ("myapp.a", "myapp.b") means b imports a
        // Forward deps of myapp.b: b imports a, so deps = {b, a}
        let tree = make_tree("/project", &[("myapp.a", "myapp.b"), ("myapp.b", "myapp.c")]);
        let trees = Trees {
            trees: vec![tree],
        };

        let deps = trees.get_dependencies_across_trees("myapp.b");
        assert!(deps.contains("myapp.a"), "b imports a");
        assert!(!deps.contains("myapp.c"), "b does not import c");
    }

    #[test]
    fn test_forward_dependencies_transitive() {
        // c imports b, b imports a -> forward deps of c should include b and a
        let tree = make_tree("/project", &[("myapp.a", "myapp.b"), ("myapp.b", "myapp.c")]);
        let trees = Trees {
            trees: vec![tree],
        };

        // We need to test the BFS version via class names
        let input_classes: HashSet<String> = ["myapp.c".to_string()].into();
        let mut seen: HashSet<String> = HashSet::new();
        let mut pending = input_classes;
        while !pending.is_empty() {
            let mut next = HashSet::new();
            for class in &pending {
                if !seen.insert(class.clone()) {
                    continue;
                }
                for dep in trees.get_dependencies_across_trees(class) {
                    if !seen.contains(&dep) {
                        next.insert(dep);
                    }
                }
            }
            pending = next;
        }

        assert!(seen.contains("myapp.c"), "includes self");
        assert!(seen.contains("myapp.b"), "c imports b");
        assert!(seen.contains("myapp.a"), "b imports a (transitive)");
    }

    #[test]
    fn test_symbol_aware_fallback_classes() {
        // Fallback classes use full module-level BFS
        let tree = make_tree_with_symbols(
            "/project",
            &[("myapp.a", "myapp.b"), ("myapp.a", "myapp.c")],
            &[("myapp.a", "foo", "myapp.b")],
            &[],
        );
        let trees = Trees { trees: vec![tree] };

        let fallback: HashSet<String> = ["myapp.a".to_string()].into();
        let result = trees.get_dependee_classes_symbol_aware(&HashMap::new(), &fallback);
        assert!(result.contains("myapp.b"));
        assert!(result.contains("myapp.c"));
    }
}
