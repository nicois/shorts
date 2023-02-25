# aiven-core Monorepo Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable `shorts` to analyze the aiven-core monorepo, which has multiple Python source roots, implicit namespace packages, and cross-root imports.

**Architecture:** Changes span two local sibling repositories owned by the same author. First, modify `pyast` (`/home/claude-aiven/git/pyast`) to support namespace package scanning and cross-root import resolution. Then, modify `shorts` (`/home/claude-aiven/git/shorts`) to add convenience flags and wire through the new pyast options. Use `go.mod` `replace` directives during development to test against local pyast changes before pushing.

**Tech Stack:** Go 1.25, `github.com/nicois/pyast`, `github.com/nicois/file`, `path/filepath`

---

## Repository Layout

All repositories are local clones owned by the same author (Nick Farrell):

```
/home/claude-aiven/git/pyast/   # github.com/nicois/pyast  - Python AST import graph builder
/home/claude-aiven/git/file/    # github.com/nicois/file   - file path utilities
/home/claude-aiven/git/git/     # github.com/nicois/git    - git integration
/home/claude-aiven/git/shorts/  # github.com/nicois/shorts - this CLI tool
```

During development, `shorts/go.mod` will use a `replace` directive to point
at the local pyast checkout:

```
replace github.com/nicois/pyast => ../pyast
```

This is removed before the final commit once pyast changes are pushed and tagged.

---

## Problem Analysis

The aiven-core repo has this layout:

```
<repo>/aiven/               # source root: <repo>/,       namespace: aiven.*
<repo>/py/kafka/src/avn/    # source root: py/kafka/src/,  namespace: avn.*
<repo>/py/metrics/src/avn/  # source root: py/metrics/src/, namespace: avn.*
... (~291 py/*/src/ projects)
```

Three technical problems prevent `shorts` from working here:

### Problem 1: Namespace package scanning (pyast)

`pyast`'s `buildDependencies` (`/home/claude-aiven/git/pyast/pyast.go:248-253`)
skips any subdirectory that lacks `__init__.py`:

```go
if path != pythonRoot && !file.FileExists(filepath.Join(path, "__init__.py")) {
    return fs.SkipDir
}
```

The `avn/` directories are implicit namespace packages with no `__init__.py`.
When `py/kafka/src/` is a root, pyast skips `src/avn/` entirely and scans
zero files. This is a **hard blocker**.

**Fix:** Add an option to `BuildTrees`/`BuildTree` that disables the
`__init__.py` check during directory walking.

### Problem 2: Cross-root import resolution (pyast)

Each tree is built and queried independently. Consider:

- Tree 1 (root `<repo>/`) scans `aiven/acorn/api.py` which does
  `from avn.kafka import consumer`. This creates a node:
  `avn.kafka.consumer` -> importers: {`aiven.acorn.api`}
- Tree 2 (root `py/kafka/src/`) contains `avn/kafka/consumer.py`

When we ask "what depends on `py/kafka/src/avn/kafka/consumer.py`?":
- Tree 2 converts the path to class `avn.kafka.consumer` (correct) and finds
  importers within Tree 2 only
- Tree 1 receives the same file path, strips its root prefix, gets
  `py/kafka/src/avn/kafka/consumer.py` -> class `py.kafka.src.avn.kafka.consumer`
  (wrong), finds nothing

Tree 1 has the node `avn.kafka.consumer` with the right importers, but
`GetDependees` maps the input path to the wrong class name because the file
isn't under Tree 1's root in the right way.

**CRITICAL: Overlapping root prefixes.** In aiven-core, the repo root `<repo>/`
is a prefix of `<repo>/py/kafka/src/`. When resolving file paths to class names,
we must use **longest-prefix matching** (most specific root wins), not first-match.
Otherwise `<repo>/py/kafka/src/avn/kafka/consumer.py` would match the repo root
and produce the wrong class `py.kafka.src.avn.kafka.consumer`.

**Fix:** Replace `trees.GetDependees` with a cross-root-aware version that uses
longest-prefix matching to resolve paths, then looks up class names across ALL
trees' node maps.

### Problem 3: Root discovery convenience (shorts)

The requirements doc proposes `--root-glob` and `--roots-from-stdin`. We already
have `--root` (repeatable), but specifying 291 roots individually isn't practical.

**Fix:** Add `--root-glob` and `--roots-from-stdin` to shorts.

---

## File Structure

### pyast repository (`/home/claude-aiven/git/pyast/`)

- **Modify:** `pyast.go` - Add `BuildTreesOptions` struct, modify `buildDependencies` for namespace packages, replace `trees.GetDependees` with cross-root-aware version
- **Create:** `pyast_test.go` - Tests for namespace scanning, cross-root resolution, overlapping roots
- **Create:** `testdata/` - Test fixtures mimicking aiven-core layout

### shorts repository (`/home/claude-aiven/git/shorts/`)

- **Modify:** `main.go` - Add `--root-glob` and `--roots-from-stdin` flags, pass options to pyast
- **Modify:** `main_test.go` - Tests for glob expansion and stdin reading
- **Modify:** `go.mod` - `replace` directive during dev, then update pyast version
- **Modify:** `README.md` - Document new flags and monorepo usage

---

## Tasks

### Task 1: Add namespace package support to pyast's directory walker

**Repo:** `/home/claude-aiven/git/pyast/`

**Files:**
- Modify: `pyast.go:135` (`BuildTrees` function)
- Modify: `pyast.go:169` (`BuildTree` function)
- Modify: `pyast.go:231-260` (`buildDependencies` function)
- Create: `pyast_test.go`
- Create: `testdata/namespace/src/avn/kafka/__init__.py`
- Create: `testdata/namespace/src/avn/kafka/consumer.py`
- Create: `testdata/namespace/src/avn/kafka/producer.py`

The `avn/` directory has no `__init__.py`. Currently pyast skips it.

- [ ] **Step 1: Create test fixtures for namespace packages**

```
testdata/namespace/src/avn/kafka/__init__.py    (empty)
testdata/namespace/src/avn/kafka/consumer.py    (contains: class KafkaConsumer: pass)
testdata/namespace/src/avn/kafka/producer.py    (contains: from avn.kafka.consumer import KafkaConsumer)
```

Note: `testdata/namespace/src/avn/` has NO `__init__.py` (namespace package).

- [ ] **Step 2: Write failing test**

```go
func TestBuildTreesNamespacePackages(t *testing.T) {
    root, _ := filepath.Abs("testdata/namespace/src")
    roots := file.CreatePaths(root)
    ctx := context.Background()

    opts := BuildTreesOptions{NamespacePackages: true}
    trees := BuildTreesWithOptions(ctx, roots, opts)

    consumerPath := filepath.Join(root, "avn/kafka/consumer.py")
    deps, err := trees.GetDependees(file.CreatePaths(consumerPath))
    if err != nil {
        t.Fatal(err)
    }

    producerPath := filepath.Join(root, "avn/kafka/producer.py")
    if _, ok := deps[producerPath]; !ok {
        t.Errorf("expected producer.py to depend on consumer.py, got: %v", deps)
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cd /home/claude-aiven/git/pyast && go test -run TestBuildTreesNamespacePackages -v`
Expected: FAIL (function doesn't exist yet)

- [ ] **Step 4: Add `BuildTreesOptions` struct and `BuildTreesWithOptions` function**

```go
// BuildTreesOptions controls tree-building behavior.
type BuildTreesOptions struct {
    // NamespacePackages disables the __init__.py requirement when walking
    // directories. Required for implicit namespace packages (PEP 420).
    NamespacePackages bool
}

func BuildTreesWithOptions(ctx context.Context, pythonRoots file.Paths, opts BuildTreesOptions) *trees {
    var wg sync.WaitGroup
    c := make(chan tree)
    for pythonRoot := range pythonRoots {
        wg.Add(1)
        go buildTreeWithOptions(ctx, &wg, c, pythonRoot, opts)
    }
    go func() {
        wg.Wait()
        close(c)
    }()
    result := make(trees, 0)
    for t := range c {
        result = append(result, t)
    }
    // (keep existing PYAST_DUMP_LOCATION logic)
    return &result
}
```

Make `BuildTrees` delegate to `BuildTreesWithOptions` with zero-value options:

```go
func BuildTrees(ctx context.Context, pythonRoots file.Paths) *trees {
    return BuildTreesWithOptions(ctx, pythonRoots, BuildTreesOptions{})
}
```

- [ ] **Step 5: Add `buildTreeWithOptions` and modify `buildDependencies`**

The key change is in the `WalkDir` callback -- make the `__init__.py` check
conditional on `namespacePackages`:

```go
func buildTreeWithOptions(ctx context.Context, pwg *sync.WaitGroup, c chan tree, pythonRoot string, opts BuildTreesOptions) {
    // Same as BuildTree but passes opts through
    defer pwg.Done()
    // ... existing setup ...
    wg.Add(1)
    go buildDependencies(ctx, &wg, pythonRoot, depPairs, opts.NamespacePackages)
    // ... rest unchanged ...
}

func buildDependencies(ctx context.Context, wg *sync.WaitGroup, pythonRoot string, depPairs chan depPair, namespacePackages bool) {
    // ... existing setup ...
    filepath.WalkDir(pythonRoot, func(path string, d fs.DirEntry, err error) error {
        if d.IsDir() {
            if path != pythonRoot && !namespacePackages && !file.FileExists(filepath.Join(path, "__init__.py")) {
                return fs.SkipDir
            }
            return nil
        }
        // ... rest unchanged ...
    })
}
```

Thread `namespacePackages` through: `BuildTreesWithOptions` -> `buildTreeWithOptions` -> `buildDependencies`.

- [ ] **Step 6: Run test to verify it passes**

Run: `cd /home/claude-aiven/git/pyast && go test -run TestBuildTreesNamespacePackages -v`
Expected: PASS

- [ ] **Step 7: Verify existing `BuildTrees` still works (backward compat)**

Run: `cd /home/claude-aiven/git/pyast && go test -v ./...`
Expected: All existing tests still pass

- [ ] **Step 8: Commit**

```bash
cd /home/claude-aiven/git/pyast
git add pyast.go pyast_test.go testdata/
git commit -m "feat: add namespace package support via BuildTreesOptions"
```

---

### Task 2: Add cross-root import resolution to pyast

**Repo:** `/home/claude-aiven/git/pyast/`

**Files:**
- Modify: `pyast.go:81-127` (`trees.GetDependees` and `tree.GetDependees`)
- Modify: `pyast_test.go`
- Create: cross-root test fixtures

This is the hardest change. The current `trees.GetDependees` calls each
`tree.GetDependees` independently. Each tree maps the input FILE PATH to a
class name relative to its own root. A file from one root produces the WRONG
class name in another tree, so cross-root imports are never found.

**The algorithm:**

1. For each input path, find which tree contains it using **longest-prefix matching**
2. Convert the path to a class name using THAT tree's root
3. Collect all such class names
4. For each class, look up its importers across ALL trees' node maps
5. Repeat from step 3 with newly discovered classes until no new importers found
6. Convert all seen classes back to file paths

- [ ] **Step 1: Create cross-root test fixtures**

```
testdata/crossroot/repo/aiven/__init__.py                       (empty)
testdata/crossroot/repo/aiven/acorn/__init__.py                 (empty)
testdata/crossroot/repo/aiven/acorn/api.py                      (from avn.kafka.consumer import KafkaConsumer)
testdata/crossroot/repo/py/kafka/src/avn/kafka/__init__.py      (empty)
testdata/crossroot/repo/py/kafka/src/avn/kafka/consumer.py      (class KafkaConsumer: pass)
testdata/crossroot/repo/py/metrics/src/avn/metrics/__init__.py  (empty)
testdata/crossroot/repo/py/metrics/src/avn/metrics/collector.py (from avn.kafka.consumer import KafkaConsumer)
```

Note: `testdata/crossroot/repo/py/kafka/src/avn/` and
`testdata/crossroot/repo/py/metrics/src/avn/` have NO `__init__.py`.

- [ ] **Step 2: Write failing tests**

```go
func TestCrossRootDependees(t *testing.T) {
    repoRoot, _ := filepath.Abs("testdata/crossroot/repo")
    kafkaSrc, _ := filepath.Abs("testdata/crossroot/repo/py/kafka/src")

    roots := file.CreatePaths(repoRoot, kafkaSrc)
    opts := BuildTreesOptions{NamespacePackages: true}
    trees := BuildTreesWithOptions(context.Background(), roots, opts)

    consumerPath := filepath.Join(kafkaSrc, "avn/kafka/consumer.py")
    deps, err := trees.GetDependees(file.CreatePaths(consumerPath))
    if err != nil {
        t.Fatal(err)
    }

    apiPath := filepath.Join(repoRoot, "aiven/acorn/api.py")
    if _, ok := deps[apiPath]; !ok {
        t.Errorf("expected aiven/acorn/api.py to depend on consumer.py via cross-root import\ngot: %v", deps)
    }
}

func TestOverlappingRootPrefixes(t *testing.T) {
    // The repo root IS a prefix of the kafka src root.
    // Verifies longest-prefix matching in pathToClassAcrossTrees.
    repoRoot, _ := filepath.Abs("testdata/crossroot/repo")
    kafkaSrc, _ := filepath.Abs("testdata/crossroot/repo/py/kafka/src")

    roots := file.CreatePaths(repoRoot, kafkaSrc)
    opts := BuildTreesOptions{NamespacePackages: true}
    trees := BuildTreesWithOptions(context.Background(), roots, opts)

    consumerPath := filepath.Join(kafkaSrc, "avn/kafka/consumer.py")
    class, ok := trees.pathToClassAcrossTrees(consumerPath)
    if !ok {
        t.Fatal("expected to find class for consumer.py")
    }
    if class != "avn.kafka.consumer" {
        t.Errorf("expected class avn.kafka.consumer, got %s (longest-prefix matching failed)", class)
    }
}

func TestCrossProjectAvnImports(t *testing.T) {
    // avn.metrics imports from avn.kafka (different roots, same namespace)
    repoRoot, _ := filepath.Abs("testdata/crossroot/repo")
    kafkaSrc, _ := filepath.Abs("testdata/crossroot/repo/py/kafka/src")
    metricsSrc, _ := filepath.Abs("testdata/crossroot/repo/py/metrics/src")

    roots := file.CreatePaths(repoRoot, kafkaSrc, metricsSrc)
    opts := BuildTreesOptions{NamespacePackages: true}
    trees := BuildTreesWithOptions(context.Background(), roots, opts)

    consumerPath := filepath.Join(kafkaSrc, "avn/kafka/consumer.py")
    deps, err := trees.GetDependees(file.CreatePaths(consumerPath))
    if err != nil {
        t.Fatal(err)
    }

    collectorPath := filepath.Join(metricsSrc, "avn/metrics/collector.py")
    if _, ok := deps[collectorPath]; !ok {
        t.Errorf("expected avn/metrics/collector.py to depend on avn/kafka/consumer.py\ngot: %v", deps)
    }
}

func TestTransitiveCrossRootDeps(t *testing.T) {
    // Changing consumer.py should find BOTH api.py (aiven.*) and collector.py (avn.metrics.*)
    repoRoot, _ := filepath.Abs("testdata/crossroot/repo")
    kafkaSrc, _ := filepath.Abs("testdata/crossroot/repo/py/kafka/src")
    metricsSrc, _ := filepath.Abs("testdata/crossroot/repo/py/metrics/src")

    roots := file.CreatePaths(repoRoot, kafkaSrc, metricsSrc)
    opts := BuildTreesOptions{NamespacePackages: true}
    trees := BuildTreesWithOptions(context.Background(), roots, opts)

    consumerPath := filepath.Join(kafkaSrc, "avn/kafka/consumer.py")
    deps, err := trees.GetDependees(file.CreatePaths(consumerPath))
    if err != nil {
        t.Fatal(err)
    }

    apiPath := filepath.Join(repoRoot, "aiven/acorn/api.py")
    collectorPath := filepath.Join(metricsSrc, "avn/metrics/collector.py")

    if _, ok := deps[apiPath]; !ok {
        t.Errorf("expected aiven/acorn/api.py in dependees (cross-root), got: %v", deps)
    }
    if _, ok := deps[collectorPath]; !ok {
        t.Errorf("expected avn/metrics/collector.py in dependees (cross-project), got: %v", deps)
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cd /home/claude-aiven/git/pyast && go test -run "TestCrossRoot|TestOverlapping|TestCrossProject|TestTransitive" -v`
Expected: FAIL

- [ ] **Step 4: Add helper methods for cross-root resolution**

```go
// pathToClassAcrossTrees finds the correct class name for a file path.
// Uses longest-prefix matching to handle overlapping roots
// (e.g., /repo/ vs /repo/py/kafka/src/).
func (t *trees) pathToClassAcrossTrees(path string) (string, bool) {
    var bestRoot string
    for _, tree := range *t {
        if strings.HasPrefix(path, tree.root+"/") {
            if len(tree.root) > len(bestRoot) {
                bestRoot = tree.root
            }
        }
    }
    if bestRoot == "" {
        return "", false
    }
    relative := path[len(bestRoot)+1:]
    if class, err := PathToClass(relative); err == nil {
        return class, true
    }
    return "", false
}

// getImportersAcrossTrees finds all classes that import the given class,
// searching the node maps of all trees.
func (t *trees) getImportersAcrossTrees(class string) Classes {
    result := CreateClasses()
    for _, tree := range *t {
        if node, ok := tree.nodes[class]; ok {
            result.Union(node.importers)
        }
    }
    return result
}

// classToPathAcrossTrees converts a class name to a file path by checking
// which tree root actually contains the file.
func (t *trees) classToPathAcrossTrees(class string) (string, bool) {
    for _, tree := range *t {
        path := ClassToPath(tree.root, class)
        if file.FileExists(path) {
            return path, true
        }
    }
    return "", false
}
```

- [ ] **Step 5: Replace `trees.GetDependees` with cross-root-aware version**

```go
func (t *trees) GetDependees(paths file.Paths) (file.Paths, error) {
    result := file.CreatePaths()
    seen := CreateClasses()

    // Seed: convert input paths to class names using the correct tree
    pending := CreateClasses()
    for path := range paths {
        if class, ok := t.pathToClassAcrossTrees(path); ok {
            pending.Add(class)
        }
    }

    // Iteratively resolve importers across all trees until stable
    for len(pending) > 0 {
        nextPending := CreateClasses()
        for class := range pending {
            if _, already := seen[class]; already {
                continue
            }
            seen.Add(class)

            importers := t.getImportersAcrossTrees(class)
            for importer := range importers {
                if _, already := seen[importer]; !already {
                    nextPending.Add(importer)
                }
            }
        }
        pending = nextPending
    }

    // Convert seen classes back to file paths
    for class := range seen {
        if path, ok := t.classToPathAcrossTrees(class); ok {
            result.Add(path)
        }
    }
    return result, nil
}
```

The individual `tree.GetDependees` method is preserved but is no longer called
by `trees.GetDependees`.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cd /home/claude-aiven/git/pyast && go test -v ./...`
Expected: All pass (including new cross-root tests and existing tests)

- [ ] **Step 7: Commit**

```bash
cd /home/claude-aiven/git/pyast
git add pyast.go pyast_test.go testdata/
git commit -m "feat: cross-root import resolution with longest-prefix matching"
```

- [ ] **Step 8: Push pyast**

```bash
cd /home/claude-aiven/git/pyast
git push origin main
```

---

### Task 3: Add `replace` directive to shorts and wire pyast options

**Repo:** `/home/claude-aiven/git/shorts/`

**Files:**
- Modify: `go.mod` (add `replace` directive, later update pyast version)
- Modify: `main.go:138-139` (use `BuildTreesWithOptions`)

- [ ] **Step 1: Add `replace` directive to test against local pyast**

```bash
cd /home/claude-aiven/git/shorts
go mod edit -replace github.com/nicois/pyast=../pyast
go mod tidy
```

- [ ] **Step 2: Replace `BuildTrees` call with `BuildTreesWithOptions`**

In `main.go`, change:

```go
trees := pyast.BuildTrees(ctx, pythonRoots)
```

to:

```go
opts := pyast.BuildTreesOptions{
    NamespacePackages: *namespacePackages,
}
trees := pyast.BuildTreesWithOptions(ctx, pythonRoots, opts)
```

- [ ] **Step 3: Verify build and tests pass against local pyast**

Run: `cd /home/claude-aiven/git/shorts && go build ./... && go test -v ./...`
Expected: All pass

- [ ] **Step 4: Commit (with replace directive still in place)**

```bash
git add go.mod go.sum main.go
git commit -m "feat: wire namespace-packages option through to pyast BuildTreesWithOptions"
```

---

### Task 4: Add `--root-glob` flag to shorts

**Repo:** `/home/claude-aiven/git/shorts/`

**Files:**
- Modify: `main.go` (add rootGlobFlags type, register flag, expand globs)
- Modify: `main_test.go`

- [ ] **Step 1: Write failing test for glob expansion**

```go
func TestRootGlobExpansion(t *testing.T) {
    dirs := []string{
        "testdata/monorepo/py/kafka/src",
        "testdata/monorepo/py/metrics/src",
        "testdata/monorepo/py/schemas/src",
    }
    for _, d := range dirs {
        os.MkdirAll(d, 0o755)
    }
    defer os.RemoveAll("testdata/monorepo")

    pattern := "testdata/monorepo/py/*/src"
    expanded, err := expandRootGlob(pattern)
    if err != nil {
        t.Fatal(err)
    }
    if len(expanded) != 3 {
        t.Fatalf("expected 3 roots, got %d: %v", len(expanded), expanded)
    }
}

func TestRootGlobEmptyResult(t *testing.T) {
    expanded, err := expandRootGlob("testdata/nonexistent/*/src")
    if err != nil {
        t.Fatal(err)
    }
    if len(expanded) != 0 {
        t.Fatalf("expected 0 roots for non-matching glob, got %d", len(expanded))
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `go test -run TestRootGlob -v`
Expected: FAIL - `expandRootGlob` undefined

- [ ] **Step 3: Implement `expandRootGlob` and `rootGlobFlags` type**

```go
type rootGlobFlags []string

func (r *rootGlobFlags) String() string { return strings.Join(*r, ",") }
func (r *rootGlobFlags) Set(value string) error {
    *r = append(*r, value)
    return nil
}

func expandRootGlob(pattern string) ([]string, error) {
    matches, err := filepath.Glob(pattern)
    if err != nil {
        return nil, fmt.Errorf("invalid glob pattern %q: %w", pattern, err)
    }
    var roots []string
    for _, m := range matches {
        abs, err := filepath.Abs(m)
        if err != nil {
            return nil, err
        }
        info, err := os.Stat(abs)
        if err != nil || !info.IsDir() {
            continue
        }
        roots = append(roots, abs)
    }
    return roots, nil
}
```

Register flag and wire into root resolution:

```go
var rootGlobs rootGlobFlags
flag.Var(&rootGlobs, "root-glob", "Glob pattern for Python source root directories (can be specified multiple times)")
```

In the root resolution section, expand globs before checking `len(roots)`:

```go
for _, pattern := range rootGlobs {
    expanded, err := expandRootGlob(pattern)
    if err != nil {
        slog.Error("failed to expand root glob", "pattern", pattern, "error", err)
        return 1
    }
    roots = append(roots, expanded...)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `go test -v ./...`
Expected: All pass

- [ ] **Step 5: Commit**

```bash
git add main.go main_test.go
git commit -m "feat: add --root-glob flag for glob-based root discovery"
```

---

### Task 5: Add `--roots-from-stdin` flag to shorts

**Repo:** `/home/claude-aiven/git/shorts/`

**Files:**
- Modify: `main.go` (add flag and stdin reading)
- Modify: `main_test.go`

- [ ] **Step 1: Write failing test**

```go
func TestReadRootsFromReader(t *testing.T) {
    input := "/tmp/root1\n/tmp/root2\n\n/tmp/root3\n"
    reader := strings.NewReader(input)
    roots, err := readRootsFromReader(reader)
    if err != nil {
        t.Fatal(err)
    }
    if len(roots) != 3 {
        t.Fatalf("expected 3 roots, got %d: %v", len(roots), roots)
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `go test -run TestReadRootsFromReader -v`
Expected: FAIL

- [ ] **Step 3: Implement stdin root reading**

```go
func readRootsFromReader(r io.Reader) ([]string, error) {
    scanner := bufio.NewScanner(r)
    var roots []string
    for scanner.Scan() {
        line := strings.TrimSpace(scanner.Text())
        if line == "" {
            continue
        }
        abs, err := filepath.Abs(line)
        if err != nil {
            return nil, fmt.Errorf("invalid root path %q: %w", line, err)
        }
        roots = append(roots, abs)
    }
    return roots, scanner.Err()
}
```

Register flag and wire in:

```go
rootsFromStdin := flag.Bool("roots-from-stdin", false, "Read source roots from stdin (one per line)")
```

```go
if *rootsFromStdin {
    stdinRoots, err := readRootsFromReader(os.Stdin)
    if err != nil {
        slog.Error("failed to read roots from stdin", "error", err)
        return 1
    }
    roots = append(roots, stdinRoots...)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `go test -v ./...`
Expected: All pass

- [ ] **Step 5: Commit**

```bash
git add main.go main_test.go
git commit -m "feat: add --roots-from-stdin flag"
```

---

### Task 6: Update README, finalize go.mod, integration test

**Repo:** `/home/claude-aiven/git/shorts/`

**Files:**
- Modify: `README.md`
- Modify: `go.mod` (remove `replace`, update pyast version)
- Modify: `main_test.go`

- [ ] **Step 1: Update go.mod to point at pushed pyast commit**

```bash
cd /home/claude-aiven/git/shorts
go mod edit -dropreplace github.com/nicois/pyast
go get github.com/nicois/pyast@<commit-hash-from-task-2>
go mod tidy
```

- [ ] **Step 2: Add monorepo usage section to README**

Add after the existing "Examples" section:

```markdown
### Monorepo usage (e.g., aiven-core)

For repositories with multiple Python source roots and namespace packages:

    # Explicit roots + glob
    shorts -root . -root-glob 'py/*/src' -namespace-packages aiven/acorn/api.py

    # Roots from stdin
    find py/*/src -maxdepth 0 | shorts -roots-from-stdin -root . -namespace-packages

Cross-root imports are resolved automatically: a change in `py/kafka/src/avn/kafka/`
will correctly identify dependees in `aiven/` that import from `avn.kafka`.

**Note:** `--roots-from-stdin` consumes stdin for root paths. Changed files
must come from positional arguments or git auto-detection; you cannot pipe
both roots and file paths via stdin simultaneously.
```

Update the flags table to include `-root-glob` and `-roots-from-stdin`.

- [ ] **Step 3: Verify build and tests pass**

Run: `go build ./... && go test -v ./...`
Expected: All pass

- [ ] **Step 4: Commit**

```bash
git add go.mod go.sum README.md main_test.go
git commit -m "docs: add monorepo usage, finalize pyast dependency"
```

---

## Execution Order

```
                    ┌─────────────────────────────────────┐
                    │  /home/claude-aiven/git/pyast/      │
                    │                                     │
                    │  Task 1: namespace packages         │
                    │         ↓                           │
                    │  Task 2: cross-root resolution      │
                    │         ↓                           │
                    │  git push                           │
                    └─────────┬───────────────────────────┘
                              │
                    ┌─────────▼───────────────────────────┐
                    │  /home/claude-aiven/git/shorts/     │
                    │                                     │
                    │  Task 3: replace directive + wire    │
                    │         ↓                           │
                    │  Task 4 ──┬── Task 5  (parallel)    │
                    │           ↓                         │
                    │  Task 6: finalize go.mod + README   │
                    └─────────────────────────────────────┘
```

Tasks 1-2 are in pyast, must complete first. Task 3 adds a `replace` directive
so shorts can build against local pyast during development. Tasks 4 and 5 are
independent shorts CLI additions that can run in parallel. Task 6 removes the
`replace` directive and finalizes everything.

## Risks

1. **Performance** - The cross-root `GetDependees` iterates until stable.
   With ~291 roots and large import graphs, this could be slow. The current
   per-tree approach is O(nodes). The cross-root approach is O(nodes * roots)
   worst case but practically converges in 2-3 iterations.
2. **Broad scanning with `-namespace-packages`** - pyast will scan ALL
   subdirectories under a root, including virtualenvs or build artifacts.
   The `-exclude` flag mitigates this for output, but pyast still spends
   time scanning. A future optimization could add exclude patterns to pyast's
   walker directly.
3. **File descriptor pressure** - pyast creates a per-root semaphore of 10
   concurrent file reads. With 291 roots, that's up to 2910 concurrent reads.
   Users may need to increase `ulimit -n`. A future pyast improvement could
   use a shared global semaphore.
4. **Stdin exclusivity** - `--roots-from-stdin` consumes stdin for root paths.
   Changed files must come from positional args or git auto-detection; you
   cannot pipe both roots and file paths via stdin simultaneously.
