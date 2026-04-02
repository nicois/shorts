# shorts

Like `pants dependees`, but not as long.

`shorts` lets you very quickly calculate the python dependencies used by one
or more modules.

This is particularly helpful in dev and CI: if you run `pylint`/`mypy`/etc.
only on the files you have changed, you will not catch errors in other modules
which depend on these files.

Tools such as `pants` can calculate the dependees of a python module, but they
are quite heavyweight and slow.

## Installation

```sh
go install github.com/nicois/shorts@latest
```

## Usage

```sh
shorts [flags] [<dependency>...]
```

### Flags

| Flag | Description |
|------|-------------|
| `-0` | Separate results with `\0` characters (for `xargs -0`) |
| `-quiet` | Only show warning and error messages |
| `-verbose` | Show additional diagnostic messages |
| `-relative` | Show relative paths instead of absolute |
| `-ref <ref>` | Git ref to calculate changes relative to (default: auto-detected upstream) |
| `-root <path>` | Explicit Python source root (repeatable; bypasses auto-detection) |
| `-exclude <pattern>` | Glob pattern to exclude from output (repeatable) |
| `-json` | Output results as JSON |
| `-explain` | Show which input file triggered each dependee |
| `-namespace-packages` | Support PEP 420 namespace packages (root detection without `__init__.py`) |
| `-version` | Print version and exit |

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
shorts -root src/app -root lib/shared src/app/utils.py
```

Exclude virtual environments and test files from results:

```sh
shorts -exclude venv -exclude test_* myapp/models.py
```

Get JSON output for CI pipeline integration:

```sh
shorts -json myapp/models.py
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
shorts -explain myapp/utils.py
```

Example output:

```
myapp/models.py  (triggered by: myapp/utils.py)
myapp/views.py  (triggered by: myapp/utils.py)
```

Use with `xargs` to run mypy only on affected files:

```sh
shorts -0 | xargs -0 mypy
```

### Building with version info

The version is embedded at build time via `-ldflags`:

```sh
go build -ldflags "-X main.version=1.0.0" -o shorts .
```

Or for releases using `go install`:

```sh
go install -ldflags "-X main.version=1.0.0" github.com/nicois/shorts@latest
```

## Details

### Root detection

By default, `shorts` automatically determines the Python root by recursively
checking parent directories of the nominated files until a directory does not
contain `__init__.py`.

With `-root`, you can bypass this detection and specify one or more source roots
explicitly. This is essential for monorepos with multiple Python projects.

With `-namespace-packages`, root detection walks up from each file until hitting
the git root or a directory containing no `.py` files, rather than relying on
`__init__.py`.

**Note:** The underlying Python AST scanner still skips subdirectories that lack
`__init__.py`. The `-namespace-packages` flag only affects root detection, not
the tree-building phase. For full namespace package support, use `-root` to
specify roots explicitly and ensure your namespace packages have `__init__.py`
in subdirectories.

### Performance

Execution time is based on the number of python modules located under the
calculated python root; all files need to be checked to be sure of identifying
dependencies. Whether you search for 1 or 100 files, execution time will be
virtually identical.

### Subdir behavior

When running without arguments from a subdirectory of a git repository, `shorts`
detects changed files relative to the git root. A warning is logged if the
current directory differs from the git root.
