use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct GitRepo {
    root: PathBuf,
    default_upstream: String,
}

impl GitRepo {
    /// Discover git repo from the given starting path. Returns None if not in a repo.
    pub fn discover(start: &Path) -> Option<Self> {
        let output = Command::new("git")
            .arg("rev-parse")
            .arg("--show-toplevel")
            .current_dir(start)
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let root = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        let default_upstream = Self::detect_upstream(&root);

        Some(Self {
            root,
            default_upstream,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn default_upstream(&self) -> &str {
        &self.default_upstream
    }

    /// Get files changed relative to the given ref.
    pub fn changed_paths(&self, since_ref: &str) -> HashSet<PathBuf> {
        let mut paths = HashSet::new();

        // 1. git diff <ref>... --name-only (three dots for merge-base diff)
        if !since_ref.is_empty() {
            let diff_ref = format!("{since_ref}...");
            if let Ok(output) = Command::new("git")
                .args(["diff", &diff_ref, "--name-only"])
                .current_dir(&self.root)
                .output()
            {
                if output.status.success() {
                    for line in String::from_utf8_lossy(&output.stdout).lines() {
                        let line = line.trim();
                        if !line.is_empty() {
                            paths.insert(self.root.join(line));
                        }
                    }
                }
            }
        }

        // 2. git ls-files --modified (uncommitted changes)
        if let Ok(output) = Command::new("git")
            .args(["ls-files", "--modified"])
            .current_dir(&self.root)
            .output()
        {
            if output.status.success() {
                for line in String::from_utf8_lossy(&output.stdout).lines() {
                    let line = line.trim();
                    if !line.is_empty() {
                        paths.insert(self.root.join(line));
                    }
                }
            }
        }

        paths
    }

    /// Get the content of a file at the merge-base with the given ref.
    /// Returns None if the file doesn't exist at that ref or on any error.
    pub fn file_content_at_ref(&self, path: &Path, since_ref: &str) -> Option<String> {
        let rel = path.strip_prefix(&self.root).ok()?;
        let rel_str = rel.to_str()?;

        // Find merge-base
        let merge_base = Command::new("git")
            .args(["merge-base", since_ref, "HEAD"])
            .current_dir(&self.root)
            .output()
            .ok()?;
        if !merge_base.status.success() {
            return None;
        }
        let base = String::from_utf8_lossy(&merge_base.stdout).trim().to_string();

        let output = Command::new("git")
            .args(["show", &format!("{base}:{rel_str}")])
            .current_dir(&self.root)
            .output()
            .ok()?;
        if output.status.success() {
            String::from_utf8(output.stdout).ok()
        } else {
            None
        }
    }

    fn detect_upstream(root: &Path) -> String {
        // 1. Check GIT_DEFAULT_UPSTREAM env var
        if let Ok(val) = std::env::var("GIT_DEFAULT_UPSTREAM") {
            if !val.is_empty() {
                return val;
            }
        }

        // 2. Try "origin/main"
        if Self::ref_exists(root, "origin/main") {
            return "origin/main".to_string();
        }

        // 3. Try "origin/master"
        if Self::ref_exists(root, "origin/master") {
            return "origin/master".to_string();
        }

        // 4. Return empty string if none found
        String::new()
    }

    fn ref_exists(root: &Path, reference: &str) -> bool {
        Command::new("git")
            .args(["rev-parse", "--verify", reference])
            .current_dir(root)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}
