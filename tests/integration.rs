use shorts::graph::Trees;
use std::collections::HashSet;
use std::path::PathBuf;

fn abs_testdata(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/testdata")
        .join(rel)
        .canonicalize()
        .unwrap()
}

#[test]
fn test_simple_dependees() {
    let root = abs_testdata("simple");
    let trees = Trees::build([root.clone()].into(), false, Default::default(), None);

    let input: HashSet<PathBuf> = [root.join("myapp/utils.py")].into();
    let deps = trees.get_dependees(&input);

    assert!(
        deps.contains(&root.join("myapp/models.py")),
        "models.py should depend on utils.py, got: {:?}",
        deps
    );
}

#[test]
fn test_transitive_simple() {
    let root = abs_testdata("simple");
    let trees = Trees::build([root.clone()].into(), false, Default::default(), None);

    let input: HashSet<PathBuf> = [root.join("myapp/utils.py")].into();
    let deps = trees.get_dependees(&input);

    // views.py imports models.py which imports utils.py — transitive
    assert!(
        deps.contains(&root.join("myapp/views.py")),
        "views.py should transitively depend on utils.py, got: {:?}",
        deps
    );
}

#[test]
fn test_namespace_packages() {
    let root = abs_testdata("namespace/src");
    let trees = Trees::build([root.clone()].into(), true, Default::default(), None);

    let input: HashSet<PathBuf> = [root.join("avn/kafka/consumer.py")].into();
    let deps = trees.get_dependees(&input);

    assert!(
        deps.contains(&root.join("avn/kafka/producer.py")),
        "producer should depend on consumer, got: {:?}",
        deps
    );
}

#[test]
fn test_namespace_packages_disabled() {
    let root = abs_testdata("namespace/src");
    let trees = Trees::build([root.clone()].into(), false, Default::default(), None);

    let input: HashSet<PathBuf> = [root.join("avn/kafka/consumer.py")].into();
    let deps = trees.get_dependees(&input);

    // Without namespace packages, avn/ (no __init__.py at src/ level) should be skipped
    assert!(
        !deps.contains(&root.join("avn/kafka/producer.py")),
        "without namespace packages, producer should NOT be detected, got: {:?}",
        deps
    );
}

#[test]
fn test_cross_root_dependees() {
    let repo_root = abs_testdata("crossroot/repo");
    let kafka_src = abs_testdata("crossroot/repo/py/kafka/src");

    let trees = Trees::build([repo_root.clone(), kafka_src.clone()].into(), true, Default::default(), None);

    let input: HashSet<PathBuf> = [kafka_src.join("avn/kafka/consumer.py")].into();
    let deps = trees.get_dependees(&input);

    assert!(
        deps.contains(&repo_root.join("aiven/acorn/api.py")),
        "api.py should depend on consumer.py via cross-root import, got: {:?}",
        deps
    );
}

#[test]
fn test_cross_project_avn_imports() {
    let repo_root = abs_testdata("crossroot/repo");
    let kafka_src = abs_testdata("crossroot/repo/py/kafka/src");
    let metrics_src = abs_testdata("crossroot/repo/py/metrics/src");

    let trees = Trees::build(
        [repo_root.clone(), kafka_src.clone(), metrics_src.clone()].into(),
        true,
        Default::default(),
        None,
    );

    let input: HashSet<PathBuf> = [kafka_src.join("avn/kafka/consumer.py")].into();
    let deps = trees.get_dependees(&input);

    assert!(
        deps.contains(&metrics_src.join("avn/metrics/collector.py")),
        "collector.py should depend on consumer.py, got: {:?}",
        deps
    );
}

#[test]
fn test_transitive_cross_root() {
    let repo_root = abs_testdata("crossroot/repo");
    let kafka_src = abs_testdata("crossroot/repo/py/kafka/src");
    let metrics_src = abs_testdata("crossroot/repo/py/metrics/src");

    let trees = Trees::build(
        [repo_root.clone(), kafka_src.clone(), metrics_src.clone()].into(),
        true,
        Default::default(),
        None,
    );

    let input: HashSet<PathBuf> = [kafka_src.join("avn/kafka/consumer.py")].into();
    let deps = trees.get_dependees(&input);

    assert!(
        deps.contains(&repo_root.join("aiven/acorn/api.py")),
        "api.py should be in dependees, got: {:?}",
        deps
    );
    assert!(
        deps.contains(&metrics_src.join("avn/metrics/collector.py")),
        "collector.py should be in dependees, got: {:?}",
        deps
    );
}
