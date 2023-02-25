use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use shorts::cache::ImportCache;
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

/// Build trees with a fresh (empty) cache, returning the trees and the populated cache dir.
fn build_with_fresh_cache(roots: HashSet<PathBuf>, ns: bool) -> (Trees, TempDir) {
    let cache_dir = TempDir::new().unwrap();
    let cache = ImportCache::default();
    let trees = Trees::build(roots, ns, cache, Some(cache_dir.path()));
    (trees, cache_dir)
}

/// Build trees reusing an existing cache from a previous run.
fn build_with_warm_cache(roots: HashSet<PathBuf>, ns: bool, cache_dir: &std::path::Path) -> Trees {
    let cache = ImportCache::load(cache_dir);
    Trees::build(roots, ns, cache, Some(cache_dir))
}

// ── Module-level BFS: cache state should not affect results ──

#[test]
fn test_module_dependees_no_cache_vs_warm_cache_simple() {
    let root = abs_testdata("simple");
    let roots: HashSet<PathBuf> = [root.clone()].into();
    let input: HashSet<PathBuf> = [root.join("myapp/utils.py")].into();

    // First run: no cache
    let (trees1, cache_dir) = build_with_fresh_cache(roots.clone(), false);
    let deps1 = trees1.get_dependees(&input);

    // Second run: warm cache
    let trees2 = build_with_warm_cache(roots.clone(), false, cache_dir.path());
    let deps2 = trees2.get_dependees(&input);

    // Third run: warm cache again
    let trees3 = build_with_warm_cache(roots, false, cache_dir.path());
    let deps3 = trees3.get_dependees(&input);

    assert_eq!(deps1, deps2, "warm cache should match cold cache");
    assert_eq!(deps2, deps3, "repeated warm cache should be stable");
    assert!(deps1.contains(&root.join("myapp/models.py")));
    assert!(deps1.contains(&root.join("myapp/views.py")));
}

#[test]
fn test_module_dependees_no_cache_vs_warm_cache_crossroot() {
    let repo_root = abs_testdata("crossroot/repo");
    let kafka_src = abs_testdata("crossroot/repo/py/kafka/src");
    let metrics_src = abs_testdata("crossroot/repo/py/metrics/src");
    let roots: HashSet<PathBuf> = [repo_root.clone(), kafka_src.clone(), metrics_src.clone()].into();
    let input: HashSet<PathBuf> = [kafka_src.join("avn/kafka/consumer.py")].into();

    let (trees1, cache_dir) = build_with_fresh_cache(roots.clone(), true);
    let deps1 = trees1.get_dependees(&input);

    let trees2 = build_with_warm_cache(roots.clone(), true, cache_dir.path());
    let deps2 = trees2.get_dependees(&input);

    let trees3 = build_with_warm_cache(roots, true, cache_dir.path());
    let deps3 = trees3.get_dependees(&input);

    assert_eq!(deps1, deps2);
    assert_eq!(deps2, deps3);
    assert!(deps1.contains(&repo_root.join("aiven/acorn/api.py")));
    assert!(deps1.contains(&metrics_src.join("avn/metrics/collector.py")));
}

#[test]
fn test_module_dependees_no_cache_vs_warm_cache_namespace() {
    let root = abs_testdata("namespace/src");
    let roots: HashSet<PathBuf> = [root.clone()].into();
    let input: HashSet<PathBuf> = [root.join("avn/kafka/consumer.py")].into();

    let (trees1, cache_dir) = build_with_fresh_cache(roots.clone(), true);
    let deps1 = trees1.get_dependees(&input);

    let trees2 = build_with_warm_cache(roots, true, cache_dir.path());
    let deps2 = trees2.get_dependees(&input);

    assert_eq!(deps1, deps2);
    assert!(deps1.contains(&root.join("avn/kafka/producer.py")));
}

// ── Symbol-aware BFS: cache state should not affect results ──

#[test]
fn test_symbol_aware_no_cache_vs_warm_cache_change_foo() {
    let root = abs_testdata("symbols");
    let roots: HashSet<PathBuf> = [root.clone()].into();

    // Simulate: only foo() changed in base.py
    let mut symbol_changes: HashMap<PathBuf, HashSet<Symbol>> = HashMap::new();
    symbol_changes.insert(
        root.join("myapp/base.py"),
        [Symbol::Function("foo".to_string())].into(),
    );
    let fallback = HashSet::new();

    // First run: no cache
    let (trees1, cache_dir) = build_with_fresh_cache(roots.clone(), false);
    let deps1 = trees1.get_dependees_symbol_aware(&symbol_changes, &fallback);

    // Second run: warm cache
    let trees2 = build_with_warm_cache(roots.clone(), false, cache_dir.path());
    let deps2 = trees2.get_dependees_symbol_aware(&symbol_changes, &fallback);

    // Third run
    let trees3 = build_with_warm_cache(roots, false, cache_dir.path());
    let deps3 = trees3.get_dependees_symbol_aware(&symbol_changes, &fallback);

    assert_eq!(deps1, deps2, "warm cache should match cold cache");
    assert_eq!(deps2, deps3, "repeated warm cache should be stable");

    // uses_foo imports foo → flagged
    assert!(deps1.contains(&root.join("myapp/uses_foo.py")),
        "uses_foo.py imports foo, should be flagged, got: {:?}", deps1);
    // uses_foo_indirect imports uses_foo → transitive
    assert!(deps1.contains(&root.join("myapp/uses_foo_indirect.py")),
        "uses_foo_indirect.py transitively depends on foo, got: {:?}", deps1);
    // uses_star does star import → always flagged
    assert!(deps1.contains(&root.join("myapp/uses_star.py")),
        "uses_star.py does star import, should always be flagged, got: {:?}", deps1);
    // uses_bar imports bar, not foo → NOT flagged
    assert!(!deps1.contains(&root.join("myapp/uses_bar.py")),
        "uses_bar.py imports bar not foo, should NOT be flagged, got: {:?}", deps1);
    // uses_baz imports Baz, not foo → NOT flagged
    assert!(!deps1.contains(&root.join("myapp/uses_baz.py")),
        "uses_baz.py imports Baz not foo, should NOT be flagged, got: {:?}", deps1);
    // uses_constant imports CONSTANT, not foo → NOT flagged
    assert!(!deps1.contains(&root.join("myapp/uses_constant.py")),
        "uses_constant.py imports CONSTANT not foo, should NOT be flagged, got: {:?}", deps1);
}

#[test]
fn test_symbol_aware_no_cache_vs_warm_cache_change_bar() {
    let root = abs_testdata("symbols");
    let roots: HashSet<PathBuf> = [root.clone()].into();

    let mut symbol_changes: HashMap<PathBuf, HashSet<Symbol>> = HashMap::new();
    symbol_changes.insert(
        root.join("myapp/base.py"),
        [Symbol::Function("bar".to_string())].into(),
    );
    let fallback = HashSet::new();

    let (trees1, cache_dir) = build_with_fresh_cache(roots.clone(), false);
    let deps1 = trees1.get_dependees_symbol_aware(&symbol_changes, &fallback);

    let trees2 = build_with_warm_cache(roots, false, cache_dir.path());
    let deps2 = trees2.get_dependees_symbol_aware(&symbol_changes, &fallback);

    assert_eq!(deps1, deps2);

    assert!(deps1.contains(&root.join("myapp/uses_bar.py")));
    assert!(deps1.contains(&root.join("myapp/uses_star.py")));
    assert!(!deps1.contains(&root.join("myapp/uses_foo.py")));
    assert!(!deps1.contains(&root.join("myapp/uses_baz.py")));
    assert!(!deps1.contains(&root.join("myapp/uses_constant.py")));
}

#[test]
fn test_symbol_aware_no_cache_vs_warm_cache_change_class() {
    let root = abs_testdata("symbols");
    let roots: HashSet<PathBuf> = [root.clone()].into();

    let mut symbol_changes: HashMap<PathBuf, HashSet<Symbol>> = HashMap::new();
    symbol_changes.insert(
        root.join("myapp/base.py"),
        [Symbol::Class("Baz".to_string())].into(),
    );
    let fallback = HashSet::new();

    let (trees1, cache_dir) = build_with_fresh_cache(roots.clone(), false);
    let deps1 = trees1.get_dependees_symbol_aware(&symbol_changes, &fallback);

    let trees2 = build_with_warm_cache(roots, false, cache_dir.path());
    let deps2 = trees2.get_dependees_symbol_aware(&symbol_changes, &fallback);

    assert_eq!(deps1, deps2);

    assert!(deps1.contains(&root.join("myapp/uses_baz.py")));
    assert!(deps1.contains(&root.join("myapp/uses_star.py")));
    assert!(!deps1.contains(&root.join("myapp/uses_foo.py")));
    assert!(!deps1.contains(&root.join("myapp/uses_bar.py")));
}

#[test]
fn test_symbol_aware_no_cache_vs_warm_cache_change_variable() {
    let root = abs_testdata("symbols");
    let roots: HashSet<PathBuf> = [root.clone()].into();

    let mut symbol_changes: HashMap<PathBuf, HashSet<Symbol>> = HashMap::new();
    symbol_changes.insert(
        root.join("myapp/base.py"),
        [Symbol::Variable("CONSTANT".to_string())].into(),
    );
    let fallback = HashSet::new();

    let (trees1, cache_dir) = build_with_fresh_cache(roots.clone(), false);
    let deps1 = trees1.get_dependees_symbol_aware(&symbol_changes, &fallback);

    let trees2 = build_with_warm_cache(roots, false, cache_dir.path());
    let deps2 = trees2.get_dependees_symbol_aware(&symbol_changes, &fallback);

    assert_eq!(deps1, deps2);

    assert!(deps1.contains(&root.join("myapp/uses_constant.py")));
    assert!(deps1.contains(&root.join("myapp/uses_star.py")));
    assert!(!deps1.contains(&root.join("myapp/uses_foo.py")));
    assert!(!deps1.contains(&root.join("myapp/uses_bar.py")));
}

#[test]
fn test_symbol_aware_no_cache_vs_warm_cache_change_module_body() {
    let root = abs_testdata("symbols");
    let roots: HashSet<PathBuf> = [root.clone()].into();

    // ModuleBody change → all importers flagged
    let mut symbol_changes: HashMap<PathBuf, HashSet<Symbol>> = HashMap::new();
    symbol_changes.insert(
        root.join("myapp/base.py"),
        [Symbol::ModuleBody].into(),
    );
    let fallback = HashSet::new();

    let (trees1, cache_dir) = build_with_fresh_cache(roots.clone(), false);
    let deps1 = trees1.get_dependees_symbol_aware(&symbol_changes, &fallback);

    let trees2 = build_with_warm_cache(roots, false, cache_dir.path());
    let deps2 = trees2.get_dependees_symbol_aware(&symbol_changes, &fallback);

    assert_eq!(deps1, deps2);

    // ModuleBody → all importers
    assert!(deps1.contains(&root.join("myapp/uses_foo.py")));
    assert!(deps1.contains(&root.join("myapp/uses_bar.py")));
    assert!(deps1.contains(&root.join("myapp/uses_baz.py")));
    assert!(deps1.contains(&root.join("myapp/uses_constant.py")));
    assert!(deps1.contains(&root.join("myapp/uses_star.py")));
    assert!(deps1.contains(&root.join("myapp/uses_module.py")));
}

#[test]
fn test_symbol_aware_no_cache_vs_warm_cache_multiple_symbols() {
    let root = abs_testdata("symbols");
    let roots: HashSet<PathBuf> = [root.clone()].into();

    // Both foo and Baz changed
    let mut symbol_changes: HashMap<PathBuf, HashSet<Symbol>> = HashMap::new();
    symbol_changes.insert(
        root.join("myapp/base.py"),
        [
            Symbol::Function("foo".to_string()),
            Symbol::Class("Baz".to_string()),
        ]
        .into(),
    );
    let fallback = HashSet::new();

    let (trees1, cache_dir) = build_with_fresh_cache(roots.clone(), false);
    let deps1 = trees1.get_dependees_symbol_aware(&symbol_changes, &fallback);

    let trees2 = build_with_warm_cache(roots, false, cache_dir.path());
    let deps2 = trees2.get_dependees_symbol_aware(&symbol_changes, &fallback);

    assert_eq!(deps1, deps2);

    assert!(deps1.contains(&root.join("myapp/uses_foo.py")));
    assert!(deps1.contains(&root.join("myapp/uses_baz.py")));
    assert!(deps1.contains(&root.join("myapp/uses_foo_indirect.py")));
    assert!(deps1.contains(&root.join("myapp/uses_star.py")));
    assert!(!deps1.contains(&root.join("myapp/uses_bar.py")));
    assert!(!deps1.contains(&root.join("myapp/uses_constant.py")));
}

// ── Fallback: module-level BFS via fallback_files ──

#[test]
fn test_symbol_aware_fallback_no_cache_vs_warm_cache() {
    let root = abs_testdata("symbols");
    let roots: HashSet<PathBuf> = [root.clone()].into();

    // Fallback = full module-level BFS for this file
    let symbol_changes = HashMap::new();
    let fallback: HashSet<PathBuf> = [root.join("myapp/base.py")].into();

    let (trees1, cache_dir) = build_with_fresh_cache(roots.clone(), false);
    let deps1 = trees1.get_dependees_symbol_aware(&symbol_changes, &fallback);

    let trees2 = build_with_warm_cache(roots, false, cache_dir.path());
    let deps2 = trees2.get_dependees_symbol_aware(&symbol_changes, &fallback);

    assert_eq!(deps1, deps2);

    // Fallback = all importers flagged (same as module-level)
    assert!(deps1.contains(&root.join("myapp/uses_foo.py")));
    assert!(deps1.contains(&root.join("myapp/uses_bar.py")));
    assert!(deps1.contains(&root.join("myapp/uses_baz.py")));
    assert!(deps1.contains(&root.join("myapp/uses_constant.py")));
    assert!(deps1.contains(&root.join("myapp/uses_star.py")));
    assert!(deps1.contains(&root.join("myapp/uses_module.py")));
}

// ── Verify symbol-aware is strictly more precise than module-level ──

#[test]
fn test_symbol_aware_subset_of_module_level() {
    let root = abs_testdata("symbols");
    let roots: HashSet<PathBuf> = [root.clone()].into();

    let (trees, _cache_dir) = build_with_fresh_cache(roots, false);

    let input: HashSet<PathBuf> = [root.join("myapp/base.py")].into();
    let module_deps = trees.get_dependees(&input);

    // Symbol-aware with a single symbol should be a subset of module-level
    for sym_name in &["foo", "bar"] {
        let mut symbol_changes: HashMap<PathBuf, HashSet<Symbol>> = HashMap::new();
        symbol_changes.insert(
            root.join("myapp/base.py"),
            [Symbol::Function(sym_name.to_string())].into(),
        );
        let symbol_deps = trees.get_dependees_symbol_aware(&symbol_changes, &HashSet::new());

        assert!(
            symbol_deps.is_subset(&module_deps),
            "symbol-aware deps for {} should be subset of module-level deps.\n\
             symbol-aware: {:?}\n\
             module-level: {:?}",
            sym_name,
            symbol_deps,
            module_deps
        );
    }
}

// ── uses_module.py: `import myapp.base` + attribute access ──

#[test]
fn test_symbol_aware_module_import_attribute_access() {
    let root = abs_testdata("symbols");
    let roots: HashSet<PathBuf> = [root.clone()].into();

    let mut symbol_changes: HashMap<PathBuf, HashSet<Symbol>> = HashMap::new();
    symbol_changes.insert(
        root.join("myapp/base.py"),
        [Symbol::Function("foo".to_string())].into(),
    );
    let fallback = HashSet::new();

    let (trees, _) = build_with_fresh_cache(roots, false);
    let deps = trees.get_dependees_symbol_aware(&symbol_changes, &fallback);

    // uses_module.py does `import myapp.base` then `myapp.base.foo()`
    // The symbol usage extractor should detect attribute access on the module
    // and record usage of "foo". If it falls back to All, it will also be flagged.
    // Either way, it should be flagged when foo changes.
    assert!(deps.contains(&root.join("myapp/uses_module.py")),
        "uses_module.py accesses myapp.base.foo(), should be flagged when foo changes, got: {:?}", deps);
}

// ── Transitive propagation consistency ──

#[test]
fn test_transitive_propagation_no_cache_vs_warm_cache() {
    let root = abs_testdata("symbols");
    let roots: HashSet<PathBuf> = [root.clone()].into();

    // Change foo → uses_foo flagged → uses_foo_indirect flagged (transitive)
    let mut symbol_changes: HashMap<PathBuf, HashSet<Symbol>> = HashMap::new();
    symbol_changes.insert(
        root.join("myapp/base.py"),
        [Symbol::Function("foo".to_string())].into(),
    );
    let fallback = HashSet::new();

    let (trees1, cache_dir) = build_with_fresh_cache(roots.clone(), false);
    let deps1 = trees1.get_dependees_symbol_aware(&symbol_changes, &fallback);

    let trees2 = build_with_warm_cache(roots.clone(), false, cache_dir.path());
    let deps2 = trees2.get_dependees_symbol_aware(&symbol_changes, &fallback);

    let trees3 = build_with_warm_cache(roots, false, cache_dir.path());
    let deps3 = trees3.get_dependees_symbol_aware(&symbol_changes, &fallback);

    assert_eq!(deps1, deps2);
    assert_eq!(deps2, deps3);

    // Verify the transitive chain works
    assert!(deps1.contains(&root.join("myapp/uses_foo.py")), "direct dep");
    assert!(deps1.contains(&root.join("myapp/uses_foo_indirect.py")), "transitive dep");
}

// ── Cross-root symbol-aware consistency ──

#[test]
fn test_crossroot_symbol_aware_no_cache_vs_warm_cache() {
    let repo_root = abs_testdata("crossroot/repo");
    let kafka_src = abs_testdata("crossroot/repo/py/kafka/src");
    let metrics_src = abs_testdata("crossroot/repo/py/metrics/src");
    let roots: HashSet<PathBuf> = [repo_root.clone(), kafka_src.clone(), metrics_src.clone()].into();

    // Change KafkaConsumer class in consumer.py
    let mut symbol_changes: HashMap<PathBuf, HashSet<Symbol>> = HashMap::new();
    symbol_changes.insert(
        kafka_src.join("avn/kafka/consumer.py"),
        [Symbol::Class("KafkaConsumer".to_string())].into(),
    );
    let fallback = HashSet::new();

    let (trees1, cache_dir) = build_with_fresh_cache(roots.clone(), true);
    let deps1 = trees1.get_dependees_symbol_aware(&symbol_changes, &fallback);

    let trees2 = build_with_warm_cache(roots, true, cache_dir.path());
    let deps2 = trees2.get_dependees_symbol_aware(&symbol_changes, &fallback);

    assert_eq!(deps1, deps2);

    assert!(deps1.contains(&repo_root.join("aiven/acorn/api.py")),
        "api.py imports KafkaConsumer, should be flagged");
    assert!(deps1.contains(&metrics_src.join("avn/metrics/collector.py")),
        "collector.py imports KafkaConsumer, should be flagged");
}

// ── Debug reasons: consistent across cache states ──

#[test]
fn test_debug_reasons_no_cache_vs_warm_cache() {
    let root = abs_testdata("symbols");
    let roots: HashSet<PathBuf> = [root.clone()].into();

    let mut symbol_changes: HashMap<PathBuf, HashSet<Symbol>> = HashMap::new();
    symbol_changes.insert(
        root.join("myapp/base.py"),
        [Symbol::Function("foo".to_string())].into(),
    );
    let fallback = HashSet::new();

    let (trees1, cache_dir) = build_with_fresh_cache(roots.clone(), false);
    let reasons1 = trees1.get_dependees_symbol_aware_with_reasons(&symbol_changes, &fallback);

    let trees2 = build_with_warm_cache(roots, false, cache_dir.path());
    let reasons2 = trees2.get_dependees_symbol_aware_with_reasons(&symbol_changes, &fallback);

    // Same set of files
    let keys1: HashSet<_> = reasons1.keys().collect();
    let keys2: HashSet<_> = reasons2.keys().collect();
    assert_eq!(keys1, keys2, "same files should be flagged");

    // Same reasons
    for (path, reason1) in &reasons1 {
        let reason2 = &reasons2[path];
        assert_eq!(reason1, reason2, "reason for {:?} should be stable", path);
    }

    // Verify specific reasons
    assert_eq!(reasons1[&root.join("myapp/base.py")], "foo() changed");
    assert_eq!(reasons1[&root.join("myapp/uses_foo.py")], "uses myapp.base.foo");
    assert_eq!(reasons1[&root.join("myapp/uses_star.py")], "star-imports myapp.base");
    assert_eq!(reasons1[&root.join("myapp/uses_foo_indirect.py")], "imports myapp.uses_foo");
}