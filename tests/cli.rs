use std::path::PathBuf;
use std::process::Command;

fn shorts_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_shorts"))
}

fn testdata(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/testdata")
        .join(rel)
}

#[test]
fn test_cli_text_output() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap()])
        .arg(root.join("myapp/utils.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("models.py"), "stdout: {stdout}");
}

#[test]
fn test_cli_json_output() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--json"])
        .arg(root.join("myapp/utils.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .expect(&format!("invalid JSON: {stdout}"));
    assert!(parsed["dependees"].is_array());
    assert!(!parsed["dependees"].as_array().unwrap().is_empty());
}

#[test]
fn test_cli_explain_output() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--explain"])
        .arg(root.join("myapp/utils.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("triggered by:"), "stdout: {stdout}");
}

#[test]
fn test_cli_exclude() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--exclude", "models*",
        ])
        .arg(root.join("myapp/utils.py"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("models.py"), "models.py should be excluded: {stdout}");
}

#[test]
fn test_cli_null_separator() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "-0"])
        .arg(root.join("myapp/utils.py"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = output.stdout;
    assert!(stdout.contains(&b'\0'), "expected null separators in output");
}

#[test]
fn test_cli_version() {
    let output = shorts_bin().arg("--version").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("shorts"), "stdout: {stdout}");
}

#[test]
fn test_cli_namespace_packages() {
    let root = testdata("namespace/src");
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--namespace-packages",
        ])
        .arg(root.join("avn/kafka/consumer.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("producer.py"), "stdout: {stdout}");
}

#[test]
fn test_cli_cross_root() {
    let repo_root = testdata("crossroot/repo");
    let kafka_src = testdata("crossroot/repo/py/kafka/src");
    let output = shorts_bin()
        .args([
            "--root", repo_root.to_str().unwrap(),
            "--root", kafka_src.to_str().unwrap(),
            "--namespace-packages",
        ])
        .arg(kafka_src.join("avn/kafka/consumer.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("api.py"), "stdout: {stdout}");
}

#[test]
fn test_cli_relative_paths() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--relative"])
        .arg(root.join("myapp/utils.py"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // With --relative, paths should not be absolute
    assert!(!stdout.lines().any(|l| l.starts_with('/')),
        "expected relative paths, got: {stdout}");
}

#[test]
fn test_cli_debug_output() {
    let root = testdata("symbols");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--debug", "--relative"])
        .arg(root.join("myapp/base.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Each line should have a reason in parentheses
    for line in stdout.lines() {
        assert!(line.contains('(') && line.contains(')'),
            "expected reason in parens, got: {line}");
    }
    // The changed file itself should show "changed"
    assert!(stdout.contains("base.py") && stdout.contains("(changed)"),
        "base.py should show (changed), got: {stdout}");
    // A direct importer should show "imports myapp.base"
    assert!(stdout.contains("uses_foo.py") && stdout.contains("imports myapp.base"),
        "uses_foo.py should show reason, got: {stdout}");
    // Transitive should show "imports myapp.uses_foo"
    assert!(stdout.contains("uses_foo_indirect.py") && stdout.contains("imports myapp.uses_foo"),
        "uses_foo_indirect.py should show transitive reason, got: {stdout}");
}

#[test]
fn test_cli_debug_json_output() {
    let root = testdata("symbols");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--debug", "--json", "--relative"])
        .arg(root.join("myapp/base.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let reasons = json["reasons"].as_object().expect("reasons should be object");
    assert!(!reasons.is_empty(), "reasons should not be empty");
    // Every dependee should have a reason
    let dependees = json["dependees"].as_array().expect("dependees array");
    for dep in dependees {
        let dep_str = dep.as_str().unwrap();
        assert!(reasons.contains_key(dep_str),
            "dependee {dep_str} should have a reason");
    }
}

// ── --stdin merge tests ──

#[test]
fn test_stdin_merge() {
    use std::io::Write;
    let root = testdata("simple");
    let extra_file = root.join("myapp/__init__.py");
    let mut child = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--stdin", "--relative"])
        .arg(root.join("myapp/utils.py"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Write extra path to stdin
    child.stdin.take().unwrap().write_all(
        format!("{}\n", extra_file.display()).as_bytes()
    ).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should contain both the dependees of utils.py AND the stdin file
    assert!(stdout.contains("__init__.py"), "stdin path should be merged: {stdout}");
    assert!(stdout.contains("models.py"), "dependees should still be present: {stdout}");
}

#[test]
fn test_stdin_dedup() {
    use std::io::Write;
    let root = testdata("simple");
    // models.py is already a dependee of utils.py; passing it via stdin should not duplicate
    let extra_file = root.join("myapp/models.py");
    let mut child = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--stdin", "--relative"])
        .arg(root.join("myapp/utils.py"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(
        format!("{}\n", extra_file.display()).as_bytes()
    ).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let models_count = stdout.lines().filter(|l| l.contains("models.py")).count();
    assert_eq!(models_count, 1, "models.py should appear exactly once (dedup): {stdout}");
}

// ── --dependencies tests ──

#[test]
fn test_dependencies_forward() {
    let root = testdata("simple");
    // views.py imports models.py which imports utils.py
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--dependencies", "--relative"])
        .arg(root.join("myapp/views.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("models.py"), "views imports models: {stdout}");
    assert!(stdout.contains("utils.py"), "models imports utils (transitive): {stdout}");
}

#[test]
fn test_dependencies_no_reverse() {
    let root = testdata("simple");
    // utils.py is imported by models.py, but forward deps of utils should NOT include models
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--dependencies", "--relative"])
        .arg(root.join("myapp/utils.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    // utils.py has no imports, so only itself
    assert!(!stdout.contains("models.py"), "should NOT include reverse deps: {stdout}");
}

#[test]
fn test_dependencies_json() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--dependencies", "--json", "--relative"])
        .arg(root.join("myapp/views.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let deps = json["dependees"].as_array().expect("dependees array");
    let dep_strs: Vec<&str> = deps.iter().map(|d| d.as_str().unwrap()).collect();
    assert!(dep_strs.iter().any(|d| d.contains("models.py")),
        "should include models.py: {:?}", dep_strs);
}

#[test]
fn test_dependencies_with_filter() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--dependencies",
            "--filter", "*utils*",
            "--relative",
        ])
        .arg(root.join("myapp/views.py"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("utils.py"), "utils should match filter: {stdout}");
    assert!(!stdout.contains("models.py"), "models should not match filter: {stdout}");
}

// ── Directory input expansion tests ──

#[test]
fn test_directory_input_expansion() {
    let root = testdata("simple");
    // Pass a directory as input — should expand to all .py files
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--changed-files-only", "--relative"])
        .arg(root.join("myapp"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("utils.py"), "should expand to utils.py: {stdout}");
    assert!(stdout.contains("models.py"), "should expand to models.py: {stdout}");
    assert!(stdout.contains("views.py"), "should expand to views.py: {stdout}");
}

#[test]
fn test_directory_input_with_double_colon() {
    let root = testdata("simple");
    // pants-style :: suffix
    let dir_spec = format!("{}::", root.join("myapp").display());
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--changed-files-only", "--relative"])
        .arg(&dir_spec)
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("utils.py"), "should expand :: to utils.py: {stdout}");
    assert!(stdout.contains("models.py"), "should expand :: to models.py: {stdout}");
}

#[test]
fn test_directory_expansion_dependees() {
    let root = testdata("simple");
    // Pass directory as input for dependee analysis (not --changed-files-only)
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--relative"])
        .arg(root.join("myapp"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should have dependees since we expanded the directory
    assert!(!stdout.trim().is_empty(), "should have dependees: {stdout}");
}

// ── --filter tests ──

#[test]
fn test_filter_keeps_matching() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--filter", "test_*",
            "--relative",
        ])
        .arg(root.join("myapp/utils.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Only test files should pass through filter
    for line in stdout.lines() {
        assert!(line.contains("test_"), "non-test file in output: {line}");
    }
}

#[test]
fn test_filter_path_prefix() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--filter", "*/models*",
            "--relative",
        ])
        .arg(root.join("myapp/utils.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("models.py"), "models.py should match filter: {stdout}");
}

#[test]
fn test_filter_with_changed_files_only() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--changed-files-only",
            "--filter", "*.py",
            "--relative",
        ])
        .arg(root.join("myapp/utils.py"))
        .arg(root.join("myapp/models.py"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("utils.py"), "utils.py should pass filter: {stdout}");
    assert!(stdout.contains("models.py"), "models.py should pass filter: {stdout}");
}

#[test]
fn test_filter_empty_result() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--filter", "nonexistent_pattern_xyz*",
        ])
        .arg(root.join("myapp/utils.py"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.trim().is_empty(), "should have no output with non-matching filter: {stdout}");
}

// ── --changed-files-only tests ──

#[test]
fn test_changed_files_only_text() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--changed-files-only", "--relative"])
        .arg(root.join("myapp/utils.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should only contain the input file, not dependees
    assert!(stdout.contains("utils.py"), "should contain the input file: {stdout}");
    assert!(!stdout.contains("models.py"), "should NOT contain dependees: {stdout}");
}

#[test]
fn test_changed_files_only_json() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--changed-files-only", "--json", "--relative"])
        .arg(root.join("myapp/utils.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let changed = json["changed_files"].as_array().expect("changed_files array");
    assert!(!changed.is_empty(), "changed_files should not be empty");
    assert!(json.get("dependees").is_none(), "should not have dependees field");
}

#[test]
fn test_json_always_includes_changed_files() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--json"])
        .arg(root.join("myapp/utils.py"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    // changed_files should always be present now (not skipped when empty)
    assert!(json.get("changed_files").is_some(),
        "changed_files should always be in JSON output: {stdout}");
}

// ── BUILD file tests ──

#[test]
fn test_cli_no_build_files_by_default() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap()])
        .arg(root.join("myapp/utils.py"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("BUILD"), "should not include BUILD file by default: {stdout}");
}

/// Helper: run shorts with --build-files on a buildfiles fixture and return JSON output.
fn run_build_files_json(root: &std::path::Path, input_file: &std::path::Path) -> serde_json::Value {
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--build-files", "--json"])
        .arg(input_file)
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).expect(&format!("invalid JSON: {stdout}"))
}

/// Helper: run shorts with --build-files on a buildfiles fixture and return text output.
fn run_build_files_text(root: &std::path::Path, input_file: &std::path::Path) -> String {
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--build-files"])
        .arg(input_file)
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn buildfiles_root() -> PathBuf {
    // The root must be the parent of `buildfiles/` so that `buildfiles.core` resolves
    testdata("")
}

fn buildfiles_dir() -> PathBuf {
    testdata("buildfiles")
}

// ── BUILD with python_sources() ──

#[test]
fn test_build_files_python_sources() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let json = run_build_files_json(&root, &dir.join("core.py"));
    let build_files = json["build_files"].as_array().expect("build_files array");
    // Dependees include files in pkg_sources, pkg_tests, etc. — at minimum
    // we should find BUILD files from those directories
    assert!(!build_files.is_empty(), "should find BUILD files for dependees");
    // The pkg_sources/BUILD contains python_sources()
    assert!(build_files.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_sources") && s.ends_with("BUILD")
    }), "should find pkg_sources/BUILD: {:?}", build_files);
}

// ── BUILD with python_tests() ──

#[test]
fn test_build_files_python_tests() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let json = run_build_files_json(&root, &dir.join("core.py"));
    let build_files = json["build_files"].as_array().expect("build_files array");
    assert!(build_files.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_tests") && s.ends_with("BUILD")
    }), "should find pkg_tests/BUILD (python_tests): {:?}", build_files);
}

// ── BUILD with python_library() ──

#[test]
fn test_build_files_python_library() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let json = run_build_files_json(&root, &dir.join("core.py"));
    let build_files = json["build_files"].as_array().expect("build_files array");
    assert!(build_files.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_library") && s.ends_with("BUILD")
    }), "should find pkg_library/BUILD (python_library): {:?}", build_files);
}

// ── BUILD with pex_binary() ──

#[test]
fn test_build_files_pex_binary() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let json = run_build_files_json(&root, &dir.join("core.py"));
    let build_files = json["build_files"].as_array().expect("build_files array");
    assert!(build_files.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_binary") && s.ends_with("BUILD")
    }), "should find pkg_binary/BUILD (pex_binary): {:?}", build_files);
}

// ── BUILD with python_distribution() ──

#[test]
fn test_build_files_python_distribution() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let json = run_build_files_json(&root, &dir.join("core.py"));
    let build_files = json["build_files"].as_array().expect("build_files array");
    assert!(build_files.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_distribution") && s.ends_with("BUILD")
    }), "should find pkg_distribution/BUILD (python_distribution): {:?}", build_files);
}

// ── BUILD with python_requirements() ──

#[test]
fn test_build_files_python_requirements() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let json = run_build_files_json(&root, &dir.join("core.py"));
    let build_files = json["build_files"].as_array().expect("build_files array");
    assert!(build_files.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_requirement") && s.ends_with("BUILD")
    }), "should find pkg_requirement/BUILD (python_requirements): {:?}", build_files);
}

// ── BUILD with resources() + python_sources() ──

#[test]
fn test_build_files_resources_and_sources() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let json = run_build_files_json(&root, &dir.join("core.py"));
    let build_files = json["build_files"].as_array().expect("build_files array");
    assert!(build_files.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_resources") && s.ends_with("BUILD")
    }), "should find pkg_resources/BUILD (resources + python_sources): {:?}", build_files);
}

// ── BUILD with multiple targets (python_sources + python_tests + pex_binary) ──

#[test]
fn test_build_files_multiple_targets() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let json = run_build_files_json(&root, &dir.join("core.py"));
    let build_files = json["build_files"].as_array().expect("build_files array");
    assert!(build_files.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_multiple") && s.ends_with("BUILD")
    }), "should find pkg_multiple/BUILD (multiple targets): {:?}", build_files);
}

// ── BUILD.pants extension variant ──

#[test]
fn test_build_files_build_dot_pants() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let json = run_build_files_json(&root, &dir.join("core.py"));
    let build_files = json["build_files"].as_array().expect("build_files array");
    assert!(build_files.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_build_ext") && s.contains("BUILD.pants")
    }), "should find pkg_build_ext/BUILD.pants: {:?}", build_files);
}

// ── No BUILD file in directory ──

#[test]
fn test_build_files_no_build_walks_up() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let stdout = run_build_files_text(&root, &dir.join("core.py"));
    // The orphan.py file is a dependee, and since pkg_no_build has no BUILD,
    // it should walk up to find a BUILD in an ancestor (or not find one)
    // The key test is that shorts doesn't crash
    assert!(stdout.contains("orphan.py"), "orphan.py should be a dependee: {stdout}");
}

// ── Nested directory inherits ancestor BUILD ──

#[test]
fn test_build_files_nested_inherits_ancestor() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let json = run_build_files_json(&root, &dir.join("core.py"));
    let build_files = json["build_files"].as_array().expect("build_files array");
    // subdir/nested/deep.py has no BUILD, but subdir/BUILD exists
    assert!(build_files.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("subdir") && s.ends_with("BUILD") && !s.contains("nested")
    }), "nested file should find ancestor subdir/BUILD: {:?}", build_files);
}

// ── BUILD files are deduplicated ──

#[test]
fn test_build_files_deduplicated() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let json = run_build_files_json(&root, &dir.join("core.py"));
    let build_files: Vec<&str> = json["build_files"].as_array().expect("build_files array")
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    // No duplicates
    let unique: std::collections::HashSet<&&str> = build_files.iter().collect();
    assert_eq!(build_files.len(), unique.len(),
        "BUILD files should be deduplicated: {:?}", build_files);
}

// ── --build-files with --json omits build_files when flag not set ──

#[test]
fn test_build_files_json_omitted_without_flag() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--json"])
        .arg(root.join("myapp/utils.py"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(json.get("build_files").is_none(),
        "build_files should not appear in JSON without --build-files flag: {stdout}");
}

// ── --build-files with text output ──

#[test]
fn test_build_files_text_output() {
    let root = testdata("simple");
    let stdout = run_build_files_text(&root, &root.join("myapp/utils.py"));
    assert!(stdout.contains("BUILD"), "text output should include BUILD: {stdout}");
    // BUILD file should appear after the Python dependees
    let lines: Vec<&str> = stdout.lines().collect();
    let build_idx = lines.iter().position(|l| l.contains("BUILD")).unwrap();
    let py_indices: Vec<usize> = lines.iter().enumerate()
        .filter(|(_, l)| l.ends_with(".py"))
        .map(|(i, _)| i)
        .collect();
    assert!(py_indices.iter().all(|&i| i < build_idx),
        "BUILD files should appear after Python files: {:?}", lines);
}

// ── --build-files with --explain ──

#[test]
fn test_build_files_with_explain() {
    let root = testdata("simple");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--build-files", "--explain"])
        .arg(root.join("myapp/utils.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("BUILD"), "--explain + --build-files should include BUILD: {stdout}");
}

// ── --build-files with --debug ──

#[test]
fn test_build_files_with_debug() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--build-files", "--debug", "--json"])
        .arg(dir.join("core.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let build_files = json["build_files"].as_array().expect("build_files array");
    assert!(!build_files.is_empty(), "--debug --build-files should include BUILD files");
}

// ── --build-file-name with custom name ──

#[test]
fn test_build_file_name_custom() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--build-files",
            "--build-file-name", "METADATA",
            "--json",
        ])
        .arg(dir.join("core.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let build_files = json["build_files"].as_array().expect("build_files array");
    // Should find METADATA in pkg_custom, but NOT any BUILD files
    assert!(build_files.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_custom") && s.ends_with("METADATA")
    }), "should find pkg_custom/METADATA: {:?}", build_files);
    assert!(!build_files.iter().any(|f| f.as_str().unwrap().contains("BUILD")),
        "should not find BUILD files when using custom name: {:?}", build_files);
}

#[test]
fn test_build_file_name_default_is_build() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    // Without --build-file-name, default is BUILD
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--build-files",
            "--json",
        ])
        .arg(dir.join("core.py"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let build_files = json["build_files"].as_array().expect("build_files array");
    // Should find BUILD files but NOT METADATA
    assert!(!build_files.is_empty(), "should find BUILD files by default");
    assert!(!build_files.iter().any(|f| f.as_str().unwrap().contains("METADATA")),
        "should not find METADATA with default name: {:?}", build_files);
}

// ── Non-Python files: reverse BUILD lookup ──

#[test]
fn test_build_files_non_python_reverse_lookup() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    // Pass a SQL file as input — should find BUILD targets that reference it.
    // The BUILD file and Python files in its directory appear as dependees.
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--build-file-name", "BUILD",
            "--json",
        ])
        .arg(dir.join("pkg_sources/schema.sql"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let dependees = json["dependees"].as_array().expect("dependees array");
    // Without --build-files, the BUILD file should NOT appear in dependees
    assert!(!dependees.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_sources") && s.ends_with("BUILD")
    }), "pkg_sources/BUILD should NOT be a dependee without --build-files: {:?}", dependees);
    // Python files in the same directory should still be dependees
    assert!(dependees.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_sources") && s.ends_with(".py")
    }), "Python files in pkg_sources should be dependees: {:?}", dependees);
}

#[test]
fn test_build_files_non_python_reverse_lookup_with_flag() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    // With --show-build-files, the BUILD file SHOULD appear in dependees
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--build-file-name", "BUILD",
            "--show-build-files",
            "--json",
        ])
        .arg(dir.join("pkg_sources/schema.sql"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let dependees = json["dependees"].as_array().expect("dependees array");
    assert!(dependees.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_sources") && s.ends_with("BUILD")
    }), "pkg_sources/BUILD should be a dependee with --show-build-files: {:?}", dependees);
    assert!(dependees.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_sources") && s.ends_with(".py")
    }), "Python files in pkg_sources should be dependees: {:?}", dependees);
}

#[test]
fn test_build_files_non_python_no_match() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    // Pass a non-existent-in-BUILD file — no BUILD file should reference it
    // Create a dummy file that no BUILD references
    let dummy = dir.join("pkg_no_build/data.json");
    std::fs::write(&dummy, "{}").unwrap();
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--build-files",
            "--json",
        ])
        .arg(&dummy)
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    // No BUILD file references data.json, so build_files should be absent or empty
    assert!(json.get("build_files").is_none()
        || json["build_files"].as_array().unwrap().is_empty(),
        "no BUILD file should reference data.json: {stdout}");
    std::fs::remove_file(&dummy).ok();
}

#[test]
fn test_build_files_non_python_text_output() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--show-build-files",
        ])
        .arg(dir.join("pkg_sources/schema.sql"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("BUILD"), "should output BUILD file for SQL input: {stdout}");
}

#[test]
fn test_build_files_mixed_python_and_non_python() {
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    // Pass both a Python file and a SQL file
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--show-build-files",
            "--build-files",
            "--json",
        ])
        .arg(dir.join("core.py"))
        .arg(dir.join("pkg_sources/schema.sql"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    // Should have Python dependees from core.py AND pkg_sources BUILD reverse lookup
    let dependees = json["dependees"].as_array().expect("dependees array");
    assert!(!dependees.is_empty(), "should have dependees");
    // pkg_sources/BUILD should be in dependees (from reverse lookup of schema.sql)
    assert!(dependees.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_sources") && s.ends_with("BUILD")
    }), "pkg_sources/BUILD should be a dependee: {:?}", dependees);
    // Python files from core.py import graph should also be present
    assert!(dependees.iter().any(|f| {
        f.as_str().unwrap().ends_with(".py")
    }), "should have Python dependees from core.py: {:?}", dependees);
}

// ── BUILD file visibility matrix tests ──

#[test]
fn test_reverse_lookup_no_flags_no_build_in_dependees() {
    // No --build-files, no --show-build-files: BUILD files should NOT appear anywhere
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--build-file-name", "BUILD",
            "--json",
        ])
        .arg(dir.join("pkg_sources/schema.sql"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let dependees = json["dependees"].as_array().expect("dependees array");
    // No BUILD in dependees
    assert!(!dependees.iter().any(|f| f.as_str().unwrap().ends_with("BUILD")),
        "BUILD should not be in dependees without --show-build-files: {:?}", dependees);
    // build_files field should be absent or empty
    assert!(json.get("build_files").is_none()
        || json["build_files"].as_array().unwrap().is_empty(),
        "build_files field should be empty without --build-files: {stdout}");
    // But Python files should still be present
    assert!(dependees.iter().any(|f| f.as_str().unwrap().ends_with(".py")),
        "Python dependees should still appear: {:?}", dependees);
}

#[test]
fn test_reverse_lookup_build_files_flag_no_build_in_dependees() {
    // --build-files but NOT --show-build-files: referencing BUILD should NOT appear
    // in dependees, and should NOT appear in build_files field either
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--build-file-name", "BUILD",
            "--build-files",
            "--json",
        ])
        .arg(dir.join("pkg_sources/schema.sql"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let dependees = json["dependees"].as_array().expect("dependees array");
    // BUILD should NOT be in dependees
    assert!(!dependees.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_sources") && s.ends_with("BUILD")
    }), "referencing BUILD should not be in dependees without --show-build-files: {:?}", dependees);
    // The referencing BUILD should NOT be in the build_files field
    let build_files = json.get("build_files").and_then(|v| v.as_array());
    if let Some(bfs) = build_files {
        assert!(!bfs.iter().any(|f| {
            let s = f.as_str().unwrap();
            s.contains("pkg_sources") && s.ends_with("BUILD")
        }), "referencing BUILD should not be in build_files without --show-build-files: {:?}", bfs);
    }
    // Python files should still be present
    assert!(dependees.iter().any(|f| f.as_str().unwrap().ends_with(".py")),
        "Python dependees should still appear: {:?}", dependees);
}

#[test]
fn test_reverse_lookup_show_build_files_flag_build_in_dependees() {
    // --show-build-files without --build-files: BUILD appears in dependees
    // but no build_files field
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--build-file-name", "BUILD",
            "--show-build-files",
            "--json",
        ])
        .arg(dir.join("pkg_sources/schema.sql"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let dependees = json["dependees"].as_array().expect("dependees array");
    // BUILD SHOULD be in dependees
    assert!(dependees.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_sources") && s.ends_with("BUILD")
    }), "BUILD should be in dependees with --show-build-files: {:?}", dependees);
    // build_files field should be absent (no --build-files flag)
    assert!(json.get("build_files").is_none()
        || json["build_files"].as_array().unwrap().is_empty(),
        "build_files field should be empty without --build-files: {stdout}");
}

#[test]
fn test_reverse_lookup_both_flags_build_everywhere() {
    // --build-files AND --show-build-files: BUILD in dependees AND build_files
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--build-file-name", "BUILD",
            "--build-files",
            "--show-build-files",
            "--json",
        ])
        .arg(dir.join("pkg_sources/schema.sql"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let dependees = json["dependees"].as_array().expect("dependees array");
    // BUILD SHOULD be in dependees
    assert!(dependees.iter().any(|f| {
        let s = f.as_str().unwrap();
        s.contains("pkg_sources") && s.ends_with("BUILD")
    }), "BUILD should be in dependees with --show-build-files: {:?}", dependees);
    // Python files present too
    assert!(dependees.iter().any(|f| f.as_str().unwrap().ends_with(".py")),
        "Python dependees should appear: {:?}", dependees);
}

#[test]
fn test_forward_lookup_unaffected_by_show_build_files() {
    // --build-files on a Python file should always include BUILD in build_files field
    // regardless of --show-build-files
    let root = buildfiles_root();
    let dir = buildfiles_dir();
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--build-files",
            "--json",
        ])
        .arg(dir.join("core.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let build_files = json["build_files"].as_array().expect("build_files array");
    assert!(!build_files.is_empty(),
        "forward lookup should include BUILD files for Python dependees: {stdout}");
}

// ── --pants-targets tests ──

#[test]
fn test_pants_targets_text_output() {
    let root = testdata("");
    let dir = testdata("pantstargets");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--pants-targets", "--relative"])
        .arg(dir.join("app.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("test_app.py:tests"), "expected :tests suffix in: {stdout}");
}

#[test]
fn test_pants_targets_source_suffix() {
    let root = testdata("");
    let dir = testdata("pantstargets");
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--pants-targets", "--relative",
            "--changed-files-only",
        ])
        .arg(dir.join("app.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("app.py:sources"), "expected :sources suffix in: {stdout}");
}

#[test]
fn test_pants_targets_json_output() {
    let root = testdata("");
    let dir = testdata("pantstargets");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--pants-targets", "--relative", "--json"])
        .arg(dir.join("app.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .expect(&format!("invalid JSON: {stdout}"));
    let deps = parsed["dependees"].as_array().unwrap();
    let has_suffix = deps.iter().any(|d| d.as_str().unwrap().contains(":tests"));
    assert!(has_suffix, "expected :tests suffix in JSON dependees: {stdout}");
}

#[test]
fn test_pants_targets_conftest_gets_tests_suffix() {
    let root = testdata("");
    let dir = testdata("pantstargets");
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--pants-targets", "--relative",
            "--changed-files-only",
        ])
        .arg(dir.join("conftest.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("conftest.py:tests"), "expected :tests suffix for conftest.py in: {stdout}");
}

#[test]
fn test_without_pants_targets_no_suffix() {
    let root = testdata("");
    let dir = testdata("pantstargets");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap()])
        .arg(dir.join("app.py"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains(":tests"), "should not have suffix without --pants-targets: {stdout}");
    assert!(!stdout.contains(":sources"), "should not have suffix without --pants-targets: {stdout}");
}
