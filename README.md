# shorts

Like `pants dependees`, but not as long.

`shorts` analyzes Python import graphs to find which modules depend on your
changed files. It tracks dependencies at the **symbol level**: if you change
function `foo` in module A, only modules that actually import and use `foo`
are flagged — not every module that imports A.

This is particularly helpful in dev and CI: if you run `pylint`/`mypy`/etc.
only on the files you have changed, you will not catch errors in other modules
which depend on these files.

Tools such as `pants` can calculate the dependees of a python module, but they
are quite heavyweight and slow.

## Installation

### From source
```sh
cargo install --git https://github.com/nicois/shorts
```

### From release binary (Linux)
Download from [Releases](https://github.com/nicois/shorts/releases):
- `shorts-x86_64-unknown-linux-gnu.tar.gz` (x86_64)
- `shorts-aarch64-unknown-linux-gnu.tar.gz` (aarch64)

## Usage

```sh
shorts [flags] [<dependency>...]
```

### Flags

| Flag | Description |
|------|-------------|
| `-0` | Separate results with `\0` characters (for `xargs -0`) |
| `--quiet` | Only show warning and error messages |
| `--verbose` | Show additional diagnostic messages |
| `--relative` | Show relative paths instead of absolute |
| `--ref <ref>` | Git ref to calculate changes relative to (default: `origin/main`, `origin/master`, or `$GIT_DEFAULT_UPSTREAM`) |
| `--root <path>` | Explicit Python source root (repeatable; bypasses auto-detection) |
| `--root-glob <pattern>` | Glob pattern for Python source root directories (repeatable) |
| `--roots-from-stdin` | Read source roots from stdin (one per line) |
| `--exclude <pattern>` | Glob pattern to exclude from output (repeatable) |
| `--filter <pattern>` | Glob pattern to include in output — only matching paths are shown (repeatable) |
| `--json` | Output results as JSON |
| `--explain` | Show which input file triggered each dependee |
| `--debug` | Show why each file was included (e.g. which symbol triggered it) |
| `--changed-files-only` | Output only the changed files (no dependee analysis) |
| `--dependencies` | Show forward dependencies (what the input files import) instead of reverse dependees |
| `--stdin` | Read additional file paths from stdin to merge into output (deduplicated) |
| `--namespace-packages` | Support PEP 420 namespace packages (root detection without `__init__.py`) |
| `--build-files` | Include the nearest pants BUILD file for each dependee in output |
| `--show-build-files` | Include referencing BUILD files in the dependees list when non-Python files trigger a reverse lookup |
| `--build-file-name <name>` | Base name for BUILD files (default: `BUILD`). Matches exact name and `name.*` variants. |
| `--cache-dir <path>` | Directory for the `.shorts` cache (default: git repo root, or cwd if not in a repo) |
| `--version` | Print version and exit |

### Examples

Print all python files which import at least one of the two files, either directly
or indirectly:

```sh
shorts myapp/utils/common.py myapp/utils/cache.py
```

Automatically calculate which files have changed relative to the default upstream
branch:

```sh
shorts
```

Specify explicit source roots for a monorepo with multiple Python projects:

```sh
shorts --root src/app --root lib/shared src/app/utils.py
```

Exclude virtual environments and test files from results:

```sh
shorts --exclude venv --exclude test_* myapp/models.py
```

Get JSON output for CI pipeline integration:

```sh
shorts --json myapp/models.py
```

Example JSON output:

```json
{
  "dependees": ["myapp/views.py", "myapp/admin.py"],
  "roots": ["/home/user/project"]
}
```

Show why each dependee was included:

```sh
shorts --explain myapp/utils.py
```

Example output:

```
myapp/models.py  (triggered by: myapp/utils.py)
myapp/views.py  (triggered by: myapp/utils.py)
```

Show the specific reason each file was flagged (symbol-level detail):

```sh
shorts --debug myapp/utils.py
```

Example output:

```
myapp/utils.py  (changed)
myapp/models.py  (uses myapp.utils.helper)
myapp/views.py  (star-imports myapp.utils)
myapp/admin.py  (imports myapp.models)
```

Include pants BUILD files in the output:

```sh
shorts --build-files myapp/utils.py
```

For projects using a non-standard BUILD file name (e.g. `.BUILD`):

```sh
shorts --build-files --build-file-name .BUILD myapp/utils.py
```

When non-Python files are passed as input (e.g. SQL, config files), `shorts`
scans BUILD files for `dependencies` or `dependency_globs` entries that
reference those files, and outputs the Python sources owned by matching targets:

```sh
shorts --build-file-name .BUILD aiven/db/funcs/email_queue.sql
```

To also include the referencing BUILD files themselves in the output:

```sh
shorts --show-build-files --build-file-name .BUILD aiven/db/funcs/email_queue.sql
```

Use with `xargs` to run mypy only on affected files:

```sh
shorts -0 | xargs -0 mypy
```

### Monorepo usage

For monorepos with multiple Python projects under a single git root (e.g. a
top-level `aiven/` package plus per-service packages under `py/*/src/`), use
`--root` and `--root-glob` to tell `shorts` about all source roots:

```sh
shorts --root . --root-glob 'py/*/src' --namespace-packages aiven/acorn/api.py
```

Or discover roots dynamically via stdin:

```sh
find py/*/src -maxdepth 0 | shorts --roots-from-stdin --root . --namespace-packages
```

When multiple roots overlap (e.g. `/repo/` and `/repo/py/kafka/src/`), `shorts`
uses longest-prefix matching so that files are resolved against the most specific
root.

**Note:** `--roots-from-stdin` reads source roots from stdin, so it cannot be
combined with piping file paths into `shorts`. Use explicit `--root` / `--root-glob`
flags if you also need to pass files via arguments.

## Details

### Symbol-level tracking

`shorts` uses the [ruff](https://github.com/astral-sh/ruff) Python parser to
analyze imports at the symbol level:

- `from A import foo` — only flagged when `foo` changes in A
- `import A; A.bar()` — only flagged when `bar` changes in A
- `from A import *` — conservatively flagged when anything in A changes
- `getattr(A, ...)` or bare module escaping (`x = A`) — conservatively flagged

Changes to comments, whitespace, or blank lines are ignored entirely (semantic
hashing). Changes to module-level code outside any function or class definition
flag all importers of that module.

After the first hop, transitive propagation is conservative: if B is flagged
because it uses `A.foo`, then all importers of B are flagged regardless of which
symbols they use from B.

### Caching

`shorts` maintains a content-addressable cache of import metadata and per-symbol
hashes in `.shorts/cache/` (under the git root by default). Cache keys are
derived from file content and module position, so the same cache can be safely
shared across branches in CI — entries are never invalidated by branch switches,
only by actual content changes.

Cache entries are stored as individual bincode files in a sharded directory
structure. Unused entries are gradually pruned (~5% per run). Use `--cache-dir`
to store the cache elsewhere (e.g. a shared CI cache directory).

The cache directory is automatically gitignored.

### Root detection

By default, `shorts` automatically determines the Python root by recursively
checking parent directories of the nominated files until a directory does not
contain `__init__.py`.

With `--root`, you can bypass this detection and specify one or more source roots
explicitly. This is essential for monorepos with multiple Python projects.

With `--namespace-packages`, both root detection and the underlying AST scanner
walk directories without requiring `__init__.py`. Root detection walks up from
each file until hitting the git root or a directory containing no `.py` files.

### Performance

File parsing and tree building are parallelized across all available CPU cores.
Execution time is based on the number of python modules located under the
calculated python root; all files need to be checked to be sure of identifying
dependencies. Whether you search for 1 or 100 files, execution time will be
virtually identical.

### Pants BUILD files

With `--build-files`, `shorts` includes the nearest BUILD file for each dependee
in the output. This is useful for CI systems that need to know which pants
targets are affected by a change.

BUILD file discovery walks up from each dependee's directory until it finds a
file matching the base name (default `BUILD`) or `BUILD.*` variants like
`BUILD.pants`. Use `--build-file-name` to change the base name (e.g. `.BUILD`).

For non-Python input files (SQL, JSON, config, etc.), `shorts` performs a reverse
lookup: it scans BUILD files under all roots for `dependencies` or
`dependency_globs` entries that reference the changed file, and outputs the
Python sources owned by those targets. Use `--show-build-files` to also include
the referencing BUILD files themselves in the dependees list.

### Intra-module dependency propagation

When a symbol changes in a module, `shorts` also checks whether other symbols
in the same module reference it. For example, if `_helper()` calls `foo()` and
`foo()` changes, then consumers of `_helper()` are also flagged. This prevents
missed dependencies due to internal call chains.

### Subdir behavior

When running without arguments from a subdirectory of a git repository, `shorts`
detects changed files relative to the git root. A warning is logged if the
current directory differs from the git root.
