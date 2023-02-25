use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// For each `.py` file, walk up from its directory until finding a directory
/// WITHOUT `__init__.py`. That directory is a Python root.
pub fn calculate_python_roots(files: &HashSet<PathBuf>) -> HashSet<PathBuf> {
    let mut roots = HashSet::new();
    for file in files {
        if file.extension().and_then(|e| e.to_str()) != Some("py") {
            continue;
        }
        if let Some(dir) = file.parent() {
            let root = walk_up_to_root(dir);
            roots.insert(root);
        }
    }
    roots
}

/// Walk up from `dir` until we find a directory that does NOT contain `__init__.py`.
fn walk_up_to_root(dir: &Path) -> PathBuf {
    let mut current = dir.to_path_buf();
    loop {
        if !current.join("__init__.py").exists() {
            return current;
        }
        match current.parent() {
            Some(parent) if parent != current => {
                current = parent.to_path_buf();
            }
            _ => return current,
        }
    }
}

/// Like `calculate_python_roots` but for PEP 420 namespace packages (no `__init__.py` required).
/// Walk up from each `.py` file until hitting:
/// - The git root (if provided), OR
/// - A directory whose parent contains no `.py` files in its immediate children
pub fn calculate_namespace_roots(
    files: &HashSet<PathBuf>,
    git_root: Option<&Path>,
) -> HashSet<PathBuf> {
    let mut roots = HashSet::new();
    for file in files {
        if file.extension().and_then(|e| e.to_str()) != Some("py") {
            continue;
        }
        if let Some(dir) = file.parent() {
            let root = walk_up_namespace(dir, git_root);
            roots.insert(root);
        }
    }
    roots
}

/// Walk up for namespace package detection.
/// Stop when:
/// - dir == git_root
/// - parent == dir (filesystem root)
/// - parent has no .py files in immediate children
fn walk_up_namespace(start: &Path, git_root: Option<&Path>) -> PathBuf {
    let mut dir = start.to_path_buf();
    while let Some(parent) = dir.parent() {
        if parent == dir {
            break;
        }
        if let Some(gr) = git_root {
            if dir == gr {
                break;
            }
        }
        if !parent_has_py_files(parent) {
            break;
        }
        dir = parent.to_path_buf();
    }
    dir
}

/// Check if a directory has any `.py` files as immediate children.
fn parent_has_py_files(dir: &Path) -> bool {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension() {
                if ext == "py" {
                    return true;
                }
            }
        }
    }
    false
}

/// Expand a glob pattern and return matching directories as absolute paths.
pub fn expand_root_glob(pattern: &str) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let entries = match glob::glob(pattern) {
        Ok(e) => e,
        Err(_) => return results,
    };
    for entry in entries.flatten() {
        if entry.is_dir() {
            match entry.canonicalize() {
                Ok(abs) => results.push(abs),
                Err(_) => results.push(entry),
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_expand_root_glob_no_match() {
        let roots = expand_root_glob("nonexistent_path_xyz/*/src");
        assert!(roots.is_empty());
    }

    #[test]
    fn test_expand_root_glob_finds_dirs() {
        // Create temp dirs
        let tmp = std::env::temp_dir().join("shorts_test_glob");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("py/kafka/src")).unwrap();
        fs::create_dir_all(tmp.join("py/metrics/src")).unwrap();
        // Create a file that should NOT match
        fs::write(tmp.join("py/readme.txt"), "").unwrap();

        let pattern = tmp.join("py/*/src").to_str().unwrap().to_string();
        let roots = expand_root_glob(&pattern);
        assert_eq!(roots.len(), 2);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_calculate_python_roots() {
        let tmp = std::env::temp_dir().join("shorts_test_roots");
        let _ = fs::remove_dir_all(&tmp);
        // Create: tmp/myapp/__init__.py and tmp/myapp/utils.py
        fs::create_dir_all(tmp.join("myapp")).unwrap();
        fs::write(tmp.join("myapp/__init__.py"), "").unwrap();
        fs::write(tmp.join("myapp/utils.py"), "").unwrap();

        let files: HashSet<PathBuf> = [tmp.join("myapp/utils.py")].into();
        let roots = calculate_python_roots(&files);
        // Root should be tmp/ (since tmp/ does NOT have __init__.py)
        assert!(roots.contains(&tmp.canonicalize().unwrap_or(tmp.clone())));

        let _ = fs::remove_dir_all(&tmp);
    }
}
