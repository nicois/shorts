# Design: `--pants-targets` flag

## Problem

Pants outputs target addresses with suffixes (e.g., `tests/unit/ci/cli/pipeline/test_pants.py:tests`). Shorts outputs bare file paths. For feature parity during the pants-to-shorts migration, shorts needs to optionally append pants-style `:target_name` suffixes to output paths.

## Behavior

When `--pants-targets` is passed, shorts appends `:target_name` to each output path that matches a target definition in its nearest BUILD file. Paths with no matching BUILD target are output bare.

The suffix is applied across all output modes: plain text, `--explain`, `--debug`, `--json`, `--changed-files-only`, and `--dependencies`. It applies only to Python file paths, not to BUILD file paths in the output.

## BUILD file parsing

A simple parser extracts target definitions from BUILD files:

1. Find `python_tests(...)` and `python_sources(...)` blocks using bracket-counting to handle nested parentheses.
2. Extract the `name` parameter as a keyword argument (`name="value"` or `name='value'`). Fall back to the directory name if `name` is omitted.
3. Extract the `sources` parameter (a list of string literals). Fall back to defaults if omitted:
   - `python_tests`: `["test_*.py", "*_test.py", "tests.py", "conftest.py"]`
   - `python_sources`: `["*.py", "!test_*.py", "!*_test.py", "!conftest.py"]`
4. Negative patterns (prefixed with `!`) exclude matches. Evaluation order: a file must match at least one positive pattern and not match any negative pattern.

### Parsing rules

Only simple string literals (single- or double-quoted) are extracted. Function calls (e.g., `globs()`), string concatenation, f-strings, and other Python expressions are not parsed. If `sources` contains unparseable expressions, it is treated as missing and defaults are used.

### Source pattern matching

Patterns are matched against the **filename only** (not the relative path from the BUILD file's directory). Recursive glob patterns (`**`) are not supported. This covers the common case where BUILD files define targets for files in their own directory.

### Supported target types

Only `python_tests` and `python_sources` are recognized. Other target types (`shell_sources`, `resources`, `python_test_utils`, etc.) are ignored.

### Target priority

When a file matches multiple targets (e.g., `conftest.py` matching both `python_tests` defaults and `python_sources` defaults), `python_tests` targets are checked before `python_sources` targets, regardless of declaration order in the BUILD file. This matches pants semantics where test targets take precedence.

## File-to-target matching

For each output file path:

1. Walk up from the file's directory looking for a BUILD file (reusing existing `find_build_files` / `collect_build_files` logic, respecting `--build-file-name`).
2. Parse the BUILD file's target definitions.
3. Match the filename against each target's source patterns. Targets are checked in priority order (`python_tests` before `python_sources`); first match wins.
4. If matched, append `:target_name` to the formatted path string.

## Implementation

### New types and functions

All added to `main.rs` alongside existing BUILD file logic:

- `struct BuildTarget` — holds `name: String`, `sources: Vec<String>`, `target_type: String`.
- `fn parse_build_targets(content: &str, default_name: &str) -> Vec<BuildTarget>` — parses a BUILD file's content and returns target definitions.
- `fn build_target_map(files: &[PathBuf], build_file_name: &str) -> HashMap<PathBuf, String>` — takes a list of output file paths (absolute), finds and parses their BUILD files, and returns a map from absolute file path to target name. When multiple BUILD file variants exist in the same directory (e.g., `BUILD` and `BUILD.pants`), all are parsed and their targets concatenated (sorted by filename).
- `fn format_path_with_target(path: &Path, cwd: &Path, relative: bool, target_map: &HashMap<PathBuf, String>) -> String` — wraps `format_path` and appends `:target_name` if the path exists in the target map.

### CLI flag

```rust
/// Append pants-style :target_name suffixes to output paths based on BUILD file target definitions.
#[arg(long)]
pants_targets: bool,
```

### Integration points

The target map is built once before output, after dependee computation. All output formatting calls switch from `format_path` to `format_path_with_target` when `--pants-targets` is enabled. When the flag is off, `format_path_with_target` behaves identically to `format_path` (the map is empty).

### Output examples

```
# Without --pants-targets
tests/unit/ci/cli/pipeline/test_pants.py
aiven/client/connection.py

# With --pants-targets
tests/unit/ci/cli/pipeline/test_pants.py:tests
aiven/client/connection.py:sources
```

JSON output applies the suffix to the string values in `dependees`, `changed_files`, and `explanations` keys.

## Not in scope

- Shell targets, resources, or other non-Python target types.
- Validating that BUILD file declarations are correct or complete.
- Using target suffixes for dependency resolution (this is output formatting only).
- Parsing complex Python expressions in BUILD files (only simple string literals and lists are extracted).
- Caching of parsed BUILD file targets (can be added later if performance requires it).
- Recursive glob patterns (`**`) in source patterns.
