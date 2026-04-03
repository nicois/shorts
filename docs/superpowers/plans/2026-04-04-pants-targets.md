# `--pants-targets` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `--pants-targets` CLI flag that appends pants-style `:target_name` suffixes to output paths by parsing BUILD file target definitions.

**Architecture:** New BUILD file target parser (`parse_build_targets`) extracts `python_tests` and `python_sources` definitions. A `build_target_map` function maps output file paths to target names. A `format_path_with_target` wrapper appends the suffix during output formatting.

**Tech Stack:** Rust, `glob` crate (already a dependency), `clap` for CLI

**Spec:** `docs/superpowers/specs/2026-04-04-pants-targets-design.md`

---

### Task 1: BUILD file target parser — unit tests and implementation

**Files:**
- Modify: `src/main.rs` (add `BuildTarget` struct, `parse_build_targets` fn, and `#[cfg(test)]` unit tests at bottom of file)

This task adds the core parsing logic with no integration — purely a function that takes BUILD file content and returns parsed targets.

- [ ] **Step 1: Add the `BuildTarget` struct**

Add after the existing `JsonOutputDebug` struct (line ~122):

```rust
struct BuildTarget {
    name: String,
    sources: Vec<String>,
    target_type: String,
}
```

- [ ] **Step 2: Write failing tests for `parse_build_targets`**

Add a `#[cfg(test)] mod tests` block at the end of `main.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_python_tests_with_name() {
        let content = r#"
python_tests(
    name="tests",
    timeout=120,
)
"#;
        let targets = parse_build_targets(content, "mydir");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].name, "tests");
        assert_eq!(targets[0].target_type, "python_tests");
        assert_eq!(targets[0].sources, vec![
            "test_*.py", "*_test.py", "tests.py", "conftest.py",
        ]);
    }

    #[test]
    fn test_parse_python_sources_with_custom_sources() {
        let content = r#"
python_sources(
    name="lib",
    sources=["app.py", "util.py"],
)
"#;
        let targets = parse_build_targets(content, "mydir");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].name, "lib");
        assert_eq!(targets[0].sources, vec!["app.py", "util.py"]);
    }

    #[test]
    fn test_parse_defaults_name_to_dir() {
        let content = r#"
python_tests()
"#;
        let targets = parse_build_targets(content, "pipeline");
        assert_eq!(targets[0].name, "pipeline");
    }

    #[test]
    fn test_parse_multiple_targets() {
        let content = r#"
python_tests(
    name="tests",
)

python_sources(
    name="sources",
)
"#;
        let targets = parse_build_targets(content, "mydir");
        assert_eq!(targets.len(), 2);
        // python_tests should come first (priority sorting)
        assert_eq!(targets[0].target_type, "python_tests");
        assert_eq!(targets[1].target_type, "python_sources");
    }

    #[test]
    fn test_parse_ignores_unknown_target_types() {
        let content = r#"
shell_sources(name="sh")
python_tests(name="tests")
resources(name="data")
"#;
        let targets = parse_build_targets(content, "mydir");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].target_type, "python_tests");
    }

    #[test]
    fn test_parse_single_quoted_name() {
        let content = r#"
python_tests(
    name='tests',
)
"#;
        let targets = parse_build_targets(content, "mydir");
        assert_eq!(targets[0].name, "tests");
    }

    #[test]
    fn test_parse_python_sources_default_sources() {
        let content = r#"
python_sources(
    name="lib",
)
"#;
        let targets = parse_build_targets(content, "mydir");
        assert_eq!(targets[0].sources, vec!["*.py", "!test_*.py", "!*_test.py", "!conftest.py"]);
    }

    #[test]
    fn test_parse_name_not_confused_with_rename() {
        let content = r#"
python_tests(
    rename="wrong",
    name="correct",
)
"#;
        let targets = parse_build_targets(content, "mydir");
        assert_eq!(targets[0].name, "correct");
    }

    #[test]
    fn test_parse_sources_with_negation() {
        let content = r#"
python_sources(
    sources=["*.py", "!test_*.py"],
)
"#;
        let targets = parse_build_targets(content, "mydir");
        assert_eq!(targets[0].sources, vec!["*.py", "!test_*.py"]);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib -- tests::test_parse 2>&1 | head -30`
Expected: compilation error — `parse_build_targets` not defined

- [ ] **Step 4: Implement `parse_build_targets`**

Add after the `BuildTarget` struct:

```rust
fn parse_build_targets(content: &str, default_name: &str) -> Vec<BuildTarget> {
    let known_types = ["python_tests", "python_sources"];
    let mut targets = Vec::new();
    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut pos = 0;

    while pos < len {
        // Find next '(' that might be a target call
        let paren_pos = match content[pos..].find('(') {
            Some(p) => pos + p,
            None => break,
        };

        // Extract identifier before '('
        let before = content[pos..paren_pos].trim_end();
        let ident_start = before
            .rfind(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|p| p + 1 + pos)
            .unwrap_or(pos);
        let target_type = content[ident_start..paren_pos].trim();

        // Find matching ')' with bracket counting
        let mut depth = 1u32;
        let mut j = paren_pos + 1;
        while j < len && depth > 0 {
            match bytes[j] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            j += 1;
        }
        let block = &content[paren_pos + 1..j.saturating_sub(1)];

        if known_types.contains(&target_type) {
            let name = extract_string_param(block, "name")
                .unwrap_or_else(|| default_name.to_string());
            let sources = extract_list_param(block, "sources")
                .unwrap_or_else(|| default_sources(target_type));

            targets.push(BuildTarget {
                name,
                sources,
                target_type: target_type.to_string(),
            });
        }

        pos = j;
    }

    // Sort: python_tests before python_sources for priority
    targets.sort_by(|a, b| {
        let priority = |t: &str| -> u8 {
            match t {
                "python_tests" => 0,
                "python_sources" => 1,
                _ => 2,
            }
        };
        priority(&a.target_type).cmp(&priority(&b.target_type))
    });

    targets
}

/// Find `param=` in block, ensuring it's not a substring of a longer identifier
/// (e.g., searching for "name=" must not match "rename=").
fn find_param(block: &str, param: &str) -> Option<usize> {
    let pattern = format!("{}=", param);
    let mut search_from = 0;
    loop {
        let idx = block[search_from..].find(&pattern)?;
        let abs_idx = search_from + idx;
        // Ensure preceding char is not alphanumeric or underscore
        if abs_idx > 0 {
            let prev = block.as_bytes()[abs_idx - 1];
            if prev.is_ascii_alphanumeric() || prev == b'_' {
                search_from = abs_idx + 1;
                continue;
            }
        }
        return Some(abs_idx);
    }
}

fn extract_string_param(block: &str, param: &str) -> Option<String> {
    let pattern = format!("{}=", param);
    let idx = find_param(block, param)?;
    let after = &block[idx + pattern.len()..];
    let after = after.trim_start();
    let quote = after.as_bytes().first()?;
    if *quote != b'"' && *quote != b'\'' {
        return None;
    }
    let end = after[1..].find(*quote as char)?;
    Some(after[1..1 + end].to_string())
}

fn extract_list_param(block: &str, param: &str) -> Option<Vec<String>> {
    let pattern = format!("{}=", param);
    let idx = find_param(block, param)?;
    let after = &block[idx + pattern.len()..];
    let after = after.trim_start();
    if !after.starts_with('[') {
        return None; // Not a simple list literal — fall back to defaults
    }
    let end = after.find(']')?;
    let list_content = &after[1..end];
    let mut items = Vec::new();
    for item in list_content.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        // Extract quoted string
        let quote = item.as_bytes().first()?;
        if *quote != b'"' && *quote != b'\'' {
            return None; // Non-literal expression — fall back to defaults
        }
        let rest = &item[1..];
        let end = rest.find(*quote as char)?;
        items.push(rest[..end].to_string());
    }
    if items.is_empty() {
        None
    } else {
        Some(items)
    }
}

fn default_sources(target_type: &str) -> Vec<String> {
    match target_type {
        "python_tests" => vec![
            "test_*.py".to_string(),
            "*_test.py".to_string(),
            "tests.py".to_string(),
            "conftest.py".to_string(),
        ],
        "python_sources" => vec![
            "*.py".to_string(),
            "!test_*.py".to_string(),
            "!*_test.py".to_string(),
            "!conftest.py".to_string(),
        ],
        _ => vec![],
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib -- tests::test_parse -v`
Expected: all 9 tests pass

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "feat: add BUILD file target parser for --pants-targets"
```

---

### Task 2: File-to-target matching — `build_target_map` and `format_path_with_target`

**Files:**
- Modify: `src/main.rs` (add `build_target_map`, `matches_source_patterns`, `format_path_with_target` fns + unit tests)

- [ ] **Step 1: Write failing tests for `matches_source_patterns` and `build_target_map`**

Add to the existing `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn test_matches_source_patterns_positive() {
        assert!(matches_source_patterns("test_foo.py", &[
            "test_*.py".to_string(),
        ]));
    }

    #[test]
    fn test_matches_source_patterns_negative() {
        // *.py matches, but !test_*.py excludes
        assert!(!matches_source_patterns("test_foo.py", &[
            "*.py".to_string(),
            "!test_*.py".to_string(),
        ]));
    }

    #[test]
    fn test_matches_source_patterns_no_positive_match() {
        assert!(!matches_source_patterns("setup.cfg", &[
            "*.py".to_string(),
        ]));
    }

    #[test]
    fn test_format_path_with_target_has_suffix() {
        let cwd = PathBuf::from("/repo");
        let path = PathBuf::from("/repo/tests/test_foo.py");
        let mut target_map = HashMap::new();
        target_map.insert(PathBuf::from("/repo/tests/test_foo.py"), "tests".to_string());
        let result = format_path_with_target(&path, &cwd, true, &target_map);
        assert_eq!(result, "tests/test_foo.py:tests");
    }

    #[test]
    fn test_format_path_with_target_no_suffix() {
        let cwd = PathBuf::from("/repo");
        let path = PathBuf::from("/repo/src/app.py");
        let target_map = HashMap::new();
        let result = format_path_with_target(&path, &cwd, true, &target_map);
        assert_eq!(result, "src/app.py");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib -- tests::test_matches tests::test_format_path_with 2>&1 | head -20`
Expected: compilation errors

- [ ] **Step 3: Implement `matches_source_patterns`**

```rust
fn matches_source_patterns(filename: &str, patterns: &[String]) -> bool {
    let mut matched = false;
    for pattern in patterns {
        if let Some(neg) = pattern.strip_prefix('!') {
            if glob::Pattern::new(neg).map_or(false, |p| p.matches(filename)) {
                return false;
            }
        } else if glob::Pattern::new(pattern).map_or(false, |p| p.matches(filename)) {
            matched = true;
        }
    }
    matched
}
```

- [ ] **Step 4: Implement `build_target_map`**

```rust
fn build_target_map(files: &[PathBuf], build_file_name: &str) -> HashMap<PathBuf, String> {
    let mut result = HashMap::new();
    // Cache parsed BUILD files by directory to avoid re-parsing
    let mut dir_cache: HashMap<PathBuf, Vec<BuildTarget>> = HashMap::new();

    for file in files {
        let filename = match file.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };

        let build_files = find_build_files(file, build_file_name);
        if build_files.is_empty() {
            continue;
        }

        let dir = match build_files[0].parent() {
            Some(d) => d.to_path_buf(),
            None => continue,
        };

        let targets = dir_cache.entry(dir.clone()).or_insert_with(|| {
            let dir_name = dir.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");
            let mut all_targets = Vec::new();
            for build_path in &build_files {
                if let Ok(content) = std::fs::read_to_string(build_path) {
                    all_targets.extend(parse_build_targets(&content, dir_name));
                }
            }
            // Re-sort after merging multiple BUILD files
            all_targets.sort_by(|a, b| {
                let priority = |t: &str| -> u8 {
                    match t { "python_tests" => 0, "python_sources" => 1, _ => 2 }
                };
                priority(&a.target_type).cmp(&priority(&b.target_type))
            });
            all_targets
        });

        for target in targets {
            if matches_source_patterns(filename, &target.sources) {
                result.insert(file.clone(), target.name.clone());
                break;
            }
        }
    }

    result
}
```

- [ ] **Step 5: Implement `format_path_with_target`**

```rust
fn format_path_with_target(
    path: &Path,
    cwd: &Path,
    relative: bool,
    target_map: &HashMap<PathBuf, String>,
) -> String {
    let base = format_path(path, cwd, relative);
    match target_map.get(path) {
        Some(target_name) => format!("{}:{}", base, target_name),
        None => base,
    }
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib -- tests::test_matches tests::test_format_path_with -v`
Expected: all 5 tests pass

- [ ] **Step 7: Commit**

```bash
git add src/main.rs
git commit -m "feat: add build_target_map and format_path_with_target"
```

---

### Task 3: CLI flag and integration into output paths

**Files:**
- Modify: `src/main.rs` (add CLI flag, build target map before output, replace `format_path` calls for dependee/file paths)

This task wires the target map into all output paths. The key rule: use `format_path_with_target` for dependee paths, changed file paths, and forward dependency paths. Keep using plain `format_path` for root paths and BUILD file paths.

- [ ] **Step 1: Add the CLI flag**

Add to the `Cli` struct after the `build_file_name` field (line ~96):

```rust
    /// Append pants-style :target_name suffixes to output paths based on BUILD file target definitions.
    #[arg(long)]
    pants_targets: bool,
```

- [ ] **Step 2: Build the target map before output**

In `main()`, after the `--changed-files-only` early return (around line 561) and before the non-Python file separation (line 563), the target map cannot be built yet because we don't know all output files. Instead, build it lazily at each output point.

A cleaner approach: define a helper closure after the CLI is parsed. Add this near the top of `main()` (after `let cwd = ...` around line 420):

```rust
    let fmt = |path: &Path| -> String {
        format_path(path, &cwd, cli.relative)
    };
```

This won't work cleanly because we need the target map. Instead, build the target map at each output section, right before formatting. Since there are 4 output sections (`--changed-files-only`, `--dependencies`, and the main output with its `explain`/`debug`/plain branches), the simplest correct approach is:

**In the `--changed-files-only` section** (line ~537): build target map from `input_files`, then use `format_path_with_target`:

Replace:
```rust
    if cli.changed_files_only {
        let mut file_strs: Vec<String> = input_files
            .iter()
            .filter(|f| !should_exclude(f, &cwd, &cli.excludes))
            .filter(|f| should_include(f, &cwd, &cli.filters))
            .map(|f| format_path(f, &cwd, cli.relative))
            .collect();
```

With:
```rust
    if cli.changed_files_only {
        let filtered: Vec<PathBuf> = input_files
            .iter()
            .filter(|f| !should_exclude(f, &cwd, &cli.excludes))
            .filter(|f| should_include(f, &cwd, &cli.filters))
            .cloned()
            .collect();
        let target_map = if cli.pants_targets {
            build_target_map(&filtered, &cli.build_file_name)
        } else {
            HashMap::new()
        };
        let mut file_strs: Vec<String> = filtered
            .iter()
            .map(|f| format_path_with_target(f, &cwd, cli.relative, &target_map))
            .collect();
```

- [ ] **Step 3: Wire into `--dependencies` output** (line ~664)

After `dep_paths.sort();` (line ~678), add:

```rust
        let target_map = if cli.pants_targets {
            build_target_map(&dep_paths, &cli.build_file_name)
        } else {
            HashMap::new()
        };
```

Then replace `format_path` with `format_path_with_target` at these specific call sites:

- `dep_paths` → dependees (line ~683): change to `format_path_with_target`
- `all_input` → changed_strs (line ~692): change to `format_path_with_target`
- `dep` → dep_str in text output (line ~709): change to `format_path_with_target`

**Do NOT change** line ~687 (`root_strs`) — root paths never get target suffixes.

- [ ] **Step 4: Wire into main output section**

The main output section (line ~716 onward) has three branches: `explain`, `debug`, and plain. For each branch, after the final `dep_paths` / `entries` is computed and sorted, build the target map once:

```rust
        let target_map = if cli.pants_targets {
            let paths: Vec<PathBuf> = entries.iter().map(|(p, _)| p.clone()).collect();
            build_target_map(&paths, &cli.build_file_name)
        } else {
            HashMap::new()
        };
```

Then replace `format_path` with `format_path_with_target` for dependee paths, changed file paths, and trigger paths. Keep plain `format_path` for root paths and BUILD file paths.

The specific `format_path` call sites to change (by line number at time of writing — these will shift after earlier edits):

**Explain branch** (~line 716-817):
- Line 753: dependees in JSON → use `format_path_with_target`
- Line 765: dep_str in text → use `format_path_with_target`
- Line 769: trigger_strs → use `format_path_with_target`
- Line 784: changed_files in JSON → use `format_path_with_target`
- Line 802: dep_str in text explain → use `format_path_with_target`
- Line 805: trigger_strs in text → use `format_path_with_target`

**Debug branch** (~line 818-902):
- Line 852: dependees in JSON → use `format_path_with_target`
- Line 864: dep_str in JSON → use `format_path_with_target`
- Line 878: changed_files in JSON → use `format_path_with_target`
- Line 896: dep_str in text → use `format_path_with_target`

**Plain branch** (~line 903-976):
- Line 940: dependees in JSON → use `format_path_with_target`
- Line 951: changed_files in JSON → use `format_path_with_target`
- Line 969: dep_str in text → use `format_path_with_target`

**Do NOT change** (keep as `format_path`):
- Root path formatting (lines 778, 872, 945)
- BUILD file path formatting (lines 753/build_strs, 852/build_strs, 929/build_strs)

- [ ] **Step 5: Verify it compiles**

Run: `cargo build 2>&1 | tail -5`
Expected: successful compilation

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "feat: add --pants-targets CLI flag and wire into all output paths"
```

---

### Task 4: CLI integration tests

**Files:**
- Create: `tests/testdata/pantstargets/BUILD` (test BUILD file)
- Create: `tests/testdata/pantstargets/__init__.py`
- Create: `tests/testdata/pantstargets/test_app.py`
- Create: `tests/testdata/pantstargets/app.py`
- Create: `tests/testdata/pantstargets/conftest.py`
- Modify: `tests/cli.rs` (add integration tests)

- [ ] **Step 1: Create test fixture files**

`tests/testdata/pantstargets/BUILD`:
```
python_tests(
    name="tests",
    timeout=120,
)

python_sources(
    name="sources",
)
```

`tests/testdata/pantstargets/__init__.py`:
```python
```

`tests/testdata/pantstargets/app.py`:
```python
def hello():
    return "hello"
```

`tests/testdata/pantstargets/test_app.py`:
```python
from pantstargets.app import hello

def test_hello():
    assert hello() == "hello"
```

`tests/testdata/pantstargets/conftest.py`:
```python
import pytest
```

- [ ] **Step 2: Write integration tests**

Add to `tests/cli.rs`:

```rust
#[test]
fn test_pants_targets_text_output() {
    let root = testdata("pantstargets");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--pants-targets", "--relative"])
        .arg(root.join("app.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("test_app.py:tests"), "expected :tests suffix in: {stdout}");
}

#[test]
fn test_pants_targets_source_suffix() {
    let root = testdata("pantstargets");
    // test_app.py imports app.py, so app.py's dependee is test_app.py
    // If we query dependees of conftest.py (imported by nothing),
    // we get no output — test with --changed-files-only instead
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--pants-targets",
            "--changed-files-only",
        ])
        .arg(root.join("app.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("app.py:sources"), "expected :sources suffix in: {stdout}");
}

#[test]
fn test_pants_targets_json_output() {
    let root = testdata("pantstargets");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap(), "--pants-targets", "--relative", "--json"])
        .arg(root.join("app.py"))
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
    let root = testdata("pantstargets");
    let output = shorts_bin()
        .args([
            "--root", root.to_str().unwrap(),
            "--pants-targets",
            "--changed-files-only",
        ])
        .arg(root.join("conftest.py"))
        .output()
        .unwrap();
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    // conftest.py should match python_tests (priority) not python_sources
    assert!(stdout.contains("conftest.py:tests"), "expected :tests suffix for conftest.py in: {stdout}");
}

#[test]
fn test_without_pants_targets_no_suffix() {
    let root = testdata("pantstargets");
    let output = shorts_bin()
        .args(["--root", root.to_str().unwrap()])
        .arg(root.join("app.py"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains(":tests"), "should not have suffix without --pants-targets: {stdout}");
    assert!(!stdout.contains(":sources"), "should not have suffix without --pants-targets: {stdout}");
}
```

- [ ] **Step 3: Run integration tests**

Run: `cargo test --test cli -- test_pants_targets test_without_pants -v`
Expected: all 5 tests pass

- [ ] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: all tests pass

- [ ] **Step 5: Commit**

```bash
git add tests/testdata/pantstargets/ tests/cli.rs
git commit -m "test: add integration tests for --pants-targets flag"
```
