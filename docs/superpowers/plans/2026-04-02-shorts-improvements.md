# Shorts Utility Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement 10 improvements to the shorts CLI tool plus switch from logrus to slog for structured logging.

**Architecture:** Rewrite main.go with slog, add new CLI flags (--root, --exclude, --json, --explain), fix existing bugs (build breakage, panic, subdir behavior), add tests, modernize release process, update README.

**Tech Stack:** Go 1.25, log/slog (stdlib), github.com/nicois/pyast, github.com/nicois/git, github.com/nicois/file

---

## Key Constraints

- `pyast` is an upstream dependency we cannot modify. Its `buildDependencies` function skips subdirectories without `__init__.py` - this limits namespace package support.
- `BuildTrees` now requires `context.Context` as first parameter (breaking API change).
- `file.Paths` is `map[string]Void` - a set of absolute paths.

## File Structure

- **Modify:** `main.go` - Complete rewrite with slog, new flags, new logic (~250 lines)
- **Create:** `main_test.go` - Integration tests with fixture Python projects
- **Create:** `testdata/` - Test fixture directory with sample Python projects
- **Modify:** `go.mod` - Remove logrus dependency
- **Modify:** `README.md` - Document all new features
- **Delete:** `make_release.py` - Replace with ldflags-based versioning
- **Delete:** `version.py` - No longer needed

---

### Task 1: Fix build and switch to slog

**Files:**
- Modify: `main.go`
- Modify: `go.mod`

- [ ] **Step 1: Rewrite main.go with slog, fix BuildTrees call, fix panic**

Replace logrus with `log/slog`, add `context.Background()` to `BuildTrees`, replace `panic(err)` with `slog.Error` + `os.Exit(1)`. Keep existing functionality identical.

- [ ] **Step 2: Run `go mod tidy` to clean up dependencies**

- [ ] **Step 3: Verify it compiles**

Run: `go build ./...`

- [ ] **Step 4: Commit**

```bash
git commit -m "fix: switch to slog, fix BuildTrees context, replace panic"
```

---

### Task 2: Add --root flag for multiple source roots

**Files:**
- Modify: `main.go`

- [ ] **Step 1: Add a `--root` flag (repeatable via custom flag type)**

When `--root` is specified, bypass `CalculatePythonRoots` and use the provided roots. Support multiple `--root` flags.

- [ ] **Step 2: Verify it compiles**

- [ ] **Step 3: Commit**

---

### Task 3: Add --namespace-packages flag

**Files:**
- Modify: `main.go`

- [ ] **Step 1: Add `--namespace-packages` flag**

When set, implement our own root detection that doesn't require `__init__.py`. Walk up from each file until we hit the git root or a directory without any `.py` files. Note: pyast's tree builder still skips subdirs without `__init__.py` - document this limitation.

- [ ] **Step 2: Commit**

---

### Task 4: Fix subdir behavior

**Files:**
- Modify: `main.go`

- [ ] **Step 1: Fix auto-detection to use git root**

When auto-detecting changed files and running from a subdirectory, resolve all paths relative to the git root rather than cwd. Warn user if running from subdir.

- [ ] **Step 2: Commit**

---

### Task 5: Add --json output mode

**Files:**
- Modify: `main.go`

- [ ] **Step 1: Add `--json` flag**

Output JSON object with `dependees` array, `changed_files` array (when auto-detecting), and `roots` array.

- [ ] **Step 2: Commit**

---

### Task 6: Add --exclude flag

**Files:**
- Modify: `main.go`

- [ ] **Step 1: Add `--exclude` flag (repeatable)**

Filter output dependees matching any exclude glob pattern. Uses `filepath.Match` for glob matching against relative paths.

- [ ] **Step 2: Commit**

---

### Task 7: Add --explain mode

**Files:**
- Modify: `main.go`

- [ ] **Step 1: Add `--explain` flag**

Call `GetDependees` per input file to show which input triggered each dependee. Output format: `dependee.py (triggered by: input1.py, input2.py)`.

- [ ] **Step 2: Commit**

---

### Task 8: Modernize release process

**Files:**
- Modify: `main.go` (add version variable)
- Delete: `make_release.py`
- Delete: `version.py`

- [ ] **Step 1: Add `var version` with ldflags, add `--version` flag**

- [ ] **Step 2: Delete make_release.py and version.py**

- [ ] **Step 3: Commit**

---

### Task 9: Add tests

**Files:**
- Create: `main_test.go`
- Create: `testdata/simple/myapp/__init__.py`
- Create: `testdata/simple/myapp/utils.py`
- Create: `testdata/simple/myapp/models.py`
- Create: `testdata/simple/myapp/views.py`

- [ ] **Step 1: Create test fixtures**

- [ ] **Step 2: Write tests for flag parsing, exclude filtering, JSON output, version flag**

- [ ] **Step 3: Run tests and verify they pass**

- [ ] **Step 4: Commit**

---

### Task 10: Update README

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Rewrite README with all new features documented**

- [ ] **Step 2: Commit**
