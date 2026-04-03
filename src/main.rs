use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::path::{Path, PathBuf};

use clap::Parser;

use shorts::cache::ImportCache;
use shorts::git::GitRepo;
use shorts::graph::Trees;
use shorts::roots::{calculate_namespace_roots, calculate_python_roots, expand_root_glob};

#[derive(Parser)]
#[command(name = "shorts", version, about = "Like pants dependees, but not as long")]
struct Cli {
    /// Input files to find dependees for. If omitted, uses git to detect changed files.
    files: Vec<String>,

    /// \0-separated output (for xargs). Default: newline-separated.
    #[arg(short = '0')]
    null_separator: bool,

    /// Only show warning and error messages. Default: info level.
    #[arg(long)]
    quiet: bool,

    /// Show additional diagnostic messages. Default: info level.
    #[arg(long)]
    verbose: bool,

    /// Show relative paths instead of absolute. Default: absolute paths.
    #[arg(long)]
    relative: bool,

    /// Git ref to calculate changes relative to. Default: origin/main, origin/master, or $GIT_DEFAULT_UPSTREAM.
    #[arg(long = "ref")]
    git_ref: Option<String>,

    /// Explicit Python source root (repeatable). Default: auto-detected from input files.
    #[arg(long = "root")]
    roots: Vec<String>,

    /// Glob pattern for source root directories (repeatable)
    #[arg(long = "root-glob")]
    root_globs: Vec<String>,

    /// Read source roots from stdin (one per line)
    #[arg(long)]
    roots_from_stdin: bool,

    /// Glob pattern to exclude from output (repeatable)
    #[arg(long = "exclude")]
    excludes: Vec<String>,

    /// Glob pattern to include in output — only matching paths are shown (repeatable)
    #[arg(long = "filter")]
    filters: Vec<String>,

    /// Output results as JSON. Default: plain text.
    #[arg(long)]
    json: bool,

    /// Show which input file triggered each dependee
    #[arg(long)]
    explain: bool,

    /// Show why each file was included in the output
    #[arg(long)]
    debug: bool,

    /// Output only the changed files (no dependee analysis). Requires git detection or explicit files.
    #[arg(long)]
    changed_files_only: bool,

    /// Show forward dependencies (what the input files import) instead of reverse dependees
    #[arg(long)]
    dependencies: bool,

    /// Read additional file paths from stdin to merge into output (deduplicated)
    #[arg(long, conflicts_with = "roots_from_stdin")]
    stdin: bool,

    /// Support PEP 420 namespace packages. Default: regular packages (requires __init__.py).
    #[arg(long)]
    namespace_packages: bool,

    /// Include pants BUILD files in output. Default: off.
    #[arg(long)]
    build_files: bool,

    /// Show referencing BUILD files in dependees output. Default: off.
    #[arg(long)]
    show_build_files: bool,

    /// Base name for BUILD files. Matches exact name and name.* variants.
    #[arg(long, default_value = "BUILD")]
    build_file_name: String,

    /// Directory for the cache. Default: ~/.cache/shorts.
    #[arg(long)]
    cache_dir: Option<String>,
}

#[derive(serde::Serialize)]
struct JsonOutput {
    dependees: Vec<String>,
    changed_files: Vec<String>,
    roots: Vec<String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    explanations: HashMap<String, Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    build_files: Vec<String>,
}

#[derive(serde::Serialize)]
struct JsonOutputDebug {
    dependees: Vec<String>,
    changed_files: Vec<String>,
    roots: Vec<String>,
    reasons: HashMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    build_files: Vec<String>,
}

struct BuildTarget {
    name: String,
    sources: Vec<String>,
    target_type: String,
}

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

/// Find all build files for a Python file by walking up from its directory.
/// Matches the exact `base_name` and `base_name.*` variants.
/// Returns files from the nearest directory that contains any match.
fn find_build_files(path: &Path, base_name: &str) -> Vec<PathBuf> {
    let prefix = format!("{}.", base_name);
    let mut dir = match path.parent() {
        Some(d) => d,
        None => return Vec::new(),
    };
    loop {
        let mut found = Vec::new();
        let build = dir.join(base_name);
        if build.is_file() {
            found.push(build);
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with(&prefix) && entry.path().is_file() {
                    found.push(entry.path());
                }
            }
        }
        if !found.is_empty() {
            found.sort();
            return found;
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return Vec::new(),
        }
    }
}

/// Collect build files for a set of paths, returning deduplicated sorted paths.
fn collect_build_files(paths: &[PathBuf], base_name: &str) -> Vec<PathBuf> {
    let mut build_files: HashSet<PathBuf> = HashSet::new();
    for path in paths {
        for build in find_build_files(path, base_name) {
            build_files.insert(build);
        }
    }
    let mut result: Vec<PathBuf> = build_files.into_iter().collect();
    result.sort();
    result
}

/// Check if a build file path matches any in the referencing set, using canonical paths.
fn is_referencing_build(path: &Path, referencing: &HashSet<PathBuf>) -> bool {
    if referencing.is_empty() {
        return false;
    }
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    referencing.iter().any(|r| {
        let r_canon = r.canonicalize().unwrap_or_else(|_| r.to_path_buf());
        canon == r_canon
    })
}

/// Scan all build files under the given roots to find ones whose `dependencies`
/// or `dependency_globs` reference any of the given non-Python changed files.
///
/// Returns `(dependee_files, build_files)`:
/// - `dependee_files`: Python source files in the directories of matching BUILD files
///   (these are the files "owned" by the targets that depend on the changed files)
/// - `build_files`: the matching BUILD file paths themselves
fn find_dependees_via_build_files(
    changed_files: &HashSet<PathBuf>,
    roots: &HashSet<PathBuf>,
    base_name: &str,
) -> (HashSet<PathBuf>, HashSet<PathBuf>) {
    use std::fs;
    use walkdir::WalkDir;

    if changed_files.is_empty() {
        return (HashSet::new(), HashSet::new());
    }

    let prefix = format!("{}.", base_name);
    let mut matching_build_files = HashSet::new();

    for root in roots {
        for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy();
            if name.as_ref() != base_name && !name.starts_with(&prefix) {
                continue;
            }
            let build_path = entry.path();
            let build_dir = match build_path.parent() {
                Some(d) => d,
                None => continue,
            };
            let content = match fs::read_to_string(build_path) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let mut matched = false;
            for changed in changed_files {
                if matched {
                    break;
                }
                // Try to get a relative path from root to the changed file
                let rel = match changed.strip_prefix(root) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let rel_str = rel.to_string_lossy();

                // Check if the BUILD file content contains this path as a literal string
                if content.contains(rel_str.as_ref()) {
                    matched = true;
                    continue;
                }

                // Also check relative to the BUILD file's directory
                if let Ok(rel_to_build) = changed.strip_prefix(build_dir) {
                    let rel_to_build_str = rel_to_build.to_string_lossy();
                    if content.contains(rel_to_build_str.as_ref()) {
                        matched = true;
                        continue;
                    }
                }

                // Check dependency_globs patterns in the content
                for line in content.lines() {
                    let trimmed = line.trim().trim_matches('"').trim_matches(',');
                    if !trimmed.contains('*') && !trimmed.contains('?') {
                        continue;
                    }
                    if let Ok(pat) = glob::Pattern::new(trimmed) {
                        if pat.matches(&rel_str) {
                            matched = true;
                            break;
                        }
                    }
                }
            }

            if matched {
                matching_build_files.insert(build_path.to_path_buf());
            }
        }
    }

    // Collect Python files referenced in matching BUILD files' dependencies.
    // Also collect Python files co-located in the BUILD file's directory.
    let mut dependee_files = HashSet::new();
    for build_path in &matching_build_files {
        let build_dir = match build_path.parent() {
            Some(d) => d,
            None => continue,
        };
        let content = match fs::read_to_string(build_path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Extract file paths from dependencies list entries
        for line in content.lines() {
            let trimmed = line.trim().trim_matches(',').trim();
            // Match quoted strings that look like file paths
            let dep = trimmed.trim_matches('"').trim_matches('\'');
            if !dep.ends_with(".py") {
                continue;
            }
            // Skip target references (contain ':')
            if dep.contains(':') {
                continue;
            }
            // Resolve relative to each root
            for root in roots {
                let candidate = root.join(dep);
                if candidate.is_file() {
                    dependee_files.insert(candidate);
                    break;
                }
            }
        }

        // Also collect Python files in the BUILD file's directory
        if let Ok(entries) = fs::read_dir(build_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                        if ext == "py" {
                            dependee_files.insert(path);
                        }
                    }
                }
            }
        }
    }

    (dependee_files, matching_build_files)
}

fn should_exclude(path: &Path, cwd: &Path, excludes: &[String]) -> bool {
    if excludes.is_empty() {
        return false;
    }
    let rel = path.strip_prefix(cwd).unwrap_or(path);
    let rel_str = rel.to_str().unwrap_or("");
    let basename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

    for pattern in excludes {
        let pat = match glob::Pattern::new(pattern) {
            Ok(p) => p,
            Err(_) => continue,
        };
        // Match against relative path
        if pat.matches(rel_str) {
            return true;
        }
        // Match against basename
        if pat.matches(basename) {
            return true;
        }
        // Match against directory components
        for component in rel.components() {
            if let Some(s) = component.as_os_str().to_str() {
                if pat.matches(s) {
                    return true;
                }
            }
        }
    }
    false
}

fn should_include(path: &Path, cwd: &Path, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }
    let rel = path.strip_prefix(cwd).unwrap_or(path);
    let rel_str = rel.to_str().unwrap_or("");
    let basename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

    for pattern in filters {
        let pat = match glob::Pattern::new(pattern) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if pat.matches(rel_str) {
            return true;
        }
        if pat.matches(basename) {
            return true;
        }
        for component in rel.components() {
            if let Some(s) = component.as_os_str().to_str() {
                if pat.matches(s) {
                    return true;
                }
            }
        }
    }
    false
}

fn format_path(path: &Path, cwd: &Path, relative: bool) -> String {
    if relative {
        path.strip_prefix(cwd)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string()
    } else {
        path.to_string_lossy().to_string()
    }
}

fn main() {
    let cli = Cli::parse();

    // Initialize logging
    let log_level = if cli.quiet {
        log::LevelFilter::Warn
    } else if cli.verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };
    env_logger::Builder::new().filter_level(log_level).init();

    let cwd = std::env::current_dir().expect("failed to get current directory");

    // 1. Discover git repo from "."
    let repo = GitRepo::discover(&cwd);

    // 2. Determine input files and upstream ref
    let files_from_cli = !cli.files.is_empty();
    let upstream_ref: Option<String> = if files_from_cli {
        None
    } else {
        let r = match &repo {
            Some(r) => r,
            None => {
                eprintln!("error: not in a git repository and no files specified");
                std::process::exit(1);
            }
        };
        Some(
            cli.git_ref
                .clone()
                .unwrap_or_else(|| r.default_upstream().to_string()),
        )
    };

    let input_files: HashSet<PathBuf> = if files_from_cli {
        let mut files = HashSet::new();
        for f in &cli.files {
            // Strip trailing :: (pants-style directory spec)
            let f = f.strip_suffix("::").unwrap_or(f);
            let p = PathBuf::from(f);
            let p = if p.is_absolute() { p } else { cwd.join(&p) };
            let p = match p.canonicalize() {
                Ok(p) => p,
                Err(_) => continue,
            };
            if p.is_dir() {
                // Expand directory to all .py files within
                for entry in walkdir::WalkDir::new(&p)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    let ep = entry.path();
                    if ep.is_file() && ep.extension().and_then(|e| e.to_str()) == Some("py") {
                        files.insert(ep.to_path_buf());
                    }
                }
            } else {
                files.insert(p);
            }
        }
        files
    } else {
        let r = repo.as_ref().unwrap();
        let git_root = r.root().to_path_buf();
        if cwd != git_root {
            log::warn!("running from subdirectory; changes are detected relative to git root");
        }
        r.changed_paths(upstream_ref.as_deref().unwrap())
    };

    // 3. Collect explicit roots
    let mut all_roots: HashSet<PathBuf> = HashSet::new();

    // --root flags
    for r in &cli.roots {
        let p = PathBuf::from(r);
        let p = if p.is_absolute() { p } else { cwd.join(&p) };
        if let Ok(abs) = p.canonicalize() {
            all_roots.insert(abs);
        } else {
            log::warn!("root path does not exist: {}", r);
        }
    }

    // --roots-from-stdin
    if cli.roots_from_stdin {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(line) => {
                    let line = line.trim().to_string();
                    if !line.is_empty() {
                        let p = PathBuf::from(&line);
                        let p = if p.is_absolute() { p } else { cwd.join(&p) };
                        if let Ok(abs) = p.canonicalize() {
                            all_roots.insert(abs);
                        } else {
                            log::warn!("stdin root path does not exist: {}", line);
                        }
                    }
                }
                Err(e) => {
                    log::warn!("error reading stdin: {}", e);
                    break;
                }
            }
        }
    }

    // 4. Expand --root-glob patterns
    for pattern in &cli.root_globs {
        let expanded = expand_root_glob(pattern);
        if expanded.is_empty() {
            log::warn!("root-glob pattern matched no directories: {}", pattern);
        }
        all_roots.extend(expanded);
    }

    // 5. Determine Python roots
    let has_explicit_roots =
        !cli.roots.is_empty() || !cli.root_globs.is_empty() || cli.roots_from_stdin;

    let python_roots = if has_explicit_roots {
        all_roots
    } else if cli.namespace_packages {
        calculate_namespace_roots(&input_files, repo.as_ref().map(|r| r.root()))
    } else {
        calculate_python_roots(&input_files)
    };

    log::debug!("python roots: {:?}", python_roots);
    log::debug!("input files: {:?}", input_files);

    // 5a. Handle --changed-files-only: output input files and exit
    if cli.changed_files_only {
        let mut file_strs: Vec<String> = input_files
            .iter()
            .filter(|f| !should_exclude(f, &cwd, &cli.excludes))
            .filter(|f| should_include(f, &cwd, &cli.filters))
            .map(|f| format_path(f, &cwd, cli.relative))
            .collect();
        file_strs.sort();

        if cli.json {
            let output = serde_json::json!({
                "changed_files": file_strs,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&output).expect("failed to serialize JSON")
            );
        } else {
            let sep = if cli.null_separator { "\0" } else { "\n" };
            for f in &file_strs {
                print!("{}{}", f, sep);
            }
        }
        return;
    }

    // 5b. Separate non-Python files for BUILD file reverse lookup
    //     Exclude BUILD files themselves — they are metadata, not source.
    let build_base = &cli.build_file_name;
    let build_prefix = format!("{}.", build_base);
    let is_build_file = |p: &Path| -> bool {
        p.file_name()
            .and_then(|n| n.to_str())
            .map_or(false, |n| n == build_base || n.starts_with(&build_prefix))
    };
    let non_python_files: HashSet<PathBuf> = input_files
        .iter()
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()).map_or(true, |e| e != "py")
                && !is_build_file(p)
        })
        .cloned()
        .collect();
    let input_files: HashSet<PathBuf> = input_files
        .into_iter()
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()).map_or(false, |e| e == "py")
                && !is_build_file(p)
        })
        .collect();
    if !non_python_files.is_empty() {
        log::debug!("non-python changed files: {:?}", non_python_files);
    }

    // 5c. Find targets that depend on non-Python changed files via BUILD files.
    //     Returns Python source files owned by those targets, plus the BUILD files.
    let (build_dependee_files, referencing_build_files) = if !non_python_files.is_empty() {
        let mut search_roots: HashSet<PathBuf> = python_roots.clone();
        if let Some(r) = &repo {
            search_roots.insert(r.root().to_path_buf());
        }
        find_dependees_via_build_files(&non_python_files, &search_roots, &cli.build_file_name)
    } else {
        (HashSet::new(), HashSet::new())
    };
    if !build_dependee_files.is_empty() {
        log::debug!("dependees via BUILD files: {:?}", build_dependee_files);
    }
    if !referencing_build_files.is_empty() {
        log::debug!("BUILD files referencing non-python changes: {:?}", referencing_build_files);
    }

    // 6. Determine cache directory
    let cache_dir_buf: Option<PathBuf> = if let Some(ref dir) = cli.cache_dir {
        let p = PathBuf::from(dir);
        Some(if p.is_absolute() { p } else { cwd.join(&p) })
    } else {
        shorts::cache::default_cache_dir()
    };
    if cache_dir_buf.is_none() {
        log::warn!("could not determine cache directory; caching disabled");
    }
    let cache_dir_opt: Option<&Path> = cache_dir_buf.as_deref();

    // Filter input files to only semantically changed ones
    //    (only when files come from git detection, not explicit CLI args)
    let input_files = if let (false, Some(r), Some(ref uref)) = (files_from_cli, &repo, &upstream_ref) {
        ImportCache::filter_semantically_changed(&input_files, r, uref)
    } else {
        input_files
    };
    log::debug!("semantically changed files: {:?}", input_files);

    // 6b. Detect which specific symbols changed (for symbol-aware filtering)
    let (symbol_changes, fallback_files) = if let (false, Some(r), Some(ref uref)) = (files_from_cli, &repo, &upstream_ref) {
        ImportCache::detect_changed_symbols(&input_files, r, uref)
    } else {
        // CLI-specified files: no upstream to compare, use full module-level
        (std::collections::HashMap::new(), input_files.clone())
    };
    log::debug!("symbol changes: {} files with symbol info, {} fallback files",
        symbol_changes.len(), fallback_files.len());

    // 7. Build trees
    let trees = Trees::build(python_roots.clone(), cli.namespace_packages, cache_dir_opt);

    // 7b. Read additional paths from stdin if --stdin
    let stdin_paths: HashSet<PathBuf> = if cli.stdin {
        let stdin = std::io::stdin();
        stdin
            .lock()
            .lines()
            .filter_map(|line| {
                let line = line.ok()?;
                let line = line.trim().to_string();
                if line.is_empty() {
                    return None;
                }
                let p = PathBuf::from(&line);
                let p = if p.is_absolute() { p } else { cwd.join(&p) };
                p.canonicalize().ok()
            })
            .collect()
    } else {
        HashSet::new()
    };

    // 8. Handle --dependencies: forward dependency query
    if cli.dependencies {
        // For --dependencies, use original input files (before semantic filtering)
        // since we want to know what these files depend on, not what changed
        let all_input: HashSet<PathBuf> = symbol_changes.keys().cloned()
            .chain(fallback_files.iter().cloned())
            .collect();
        let forward_deps = trees.get_dependencies(&all_input);
        let mut dep_paths: Vec<PathBuf> = forward_deps
            .into_iter()
            .filter(|p| p.is_file())
            .filter(|p| !should_exclude(p, &cwd, &cli.excludes))
            .filter(|p| should_include(p, &cwd, &cli.filters))
            .collect();
        dep_paths.sort();

        if cli.json {
            let dependees: Vec<String> = dep_paths
                .iter()
                .map(|p| format_path(p, &cwd, cli.relative))
                .collect();
            let mut root_strs: Vec<String> = python_roots
                .iter()
                .map(|r| format_path(r, &cwd, cli.relative))
                .collect();
            root_strs.sort();
            let mut changed_strs: Vec<String> = all_input
                .iter()
                .map(|f| format_path(f, &cwd, cli.relative))
                .collect();
            changed_strs.sort();
            let output = JsonOutput {
                dependees,
                changed_files: changed_strs,
                roots: root_strs,
                explanations: HashMap::new(),
                build_files: Vec::new(),
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&output).expect("failed to serialize JSON")
            );
        } else {
            let sep = if cli.null_separator { "\0" } else { "\n" };
            for dep in &dep_paths {
                let dep_str = format_path(dep, &cwd, cli.relative);
                print!("{}{}", dep_str, sep);
            }
        }
        return;
    }

    // 9. Compute dependees and output
    if cli.explain {
        let mut explained = trees.get_dependees_symbol_aware_explained(&symbol_changes, &fallback_files);
        // Add BUILD-discovered dependees with non-Python triggers
        let non_py_triggers: Vec<PathBuf> = non_python_files.iter().cloned().collect();
        for dep in build_dependee_files.iter() {
            explained.entry(dep.clone()).or_insert_with(|| non_py_triggers.clone());
        }
        if cli.show_build_files {
            for dep in referencing_build_files.iter() {
                explained.entry(dep.clone()).or_insert_with(|| non_py_triggers.clone());
            }
        }
        // Merge --stdin paths
        for path in &stdin_paths {
            explained.entry(path.clone()).or_insert_with(Vec::new);
        }

        // Sort for deterministic output
        let mut entries: Vec<(PathBuf, Vec<PathBuf>)> = explained
            .into_iter()
            .filter(|(p, _)| p.is_file())
            .filter(|(p, _)| !should_exclude(p, &cwd, &cli.excludes))
            .filter(|(p, _)| should_include(p, &cwd, &cli.filters))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let dep_paths: Vec<PathBuf> = entries.iter().map(|(p, _)| p.clone()).collect();
        let build_strs: Vec<String> = if cli.build_files {
            let mut all_builds: HashSet<PathBuf> = HashSet::new();
            for b in collect_build_files(&dep_paths, &cli.build_file_name) {
                if cli.show_build_files || !is_referencing_build(&b, &referencing_build_files) {
                    all_builds.insert(b);
                }
            }
            let mut sorted: Vec<String> = all_builds
                .iter()
                .map(|p| format_path(p, &cwd, cli.relative))
                .collect();
            sorted.sort();
            sorted
        } else {
            Vec::new()
        };

        if cli.json {
            let mut dependees: Vec<String> = Vec::new();
            let mut explanations: HashMap<String, Vec<String>> = HashMap::new();
            for (dep, triggers) in &entries {
                let dep_str = format_path(dep, &cwd, cli.relative);
                dependees.push(dep_str.clone());
                let mut trigger_strs: Vec<String> = triggers
                    .iter()
                    .map(|t| format_path(t, &cwd, cli.relative))
                    .collect();
                trigger_strs.sort();
                explanations.insert(dep_str, trigger_strs);
            }
            dependees.sort();

            let mut root_strs: Vec<String> = python_roots
                .iter()
                .map(|r| format_path(r, &cwd, cli.relative))
                .collect();
            root_strs.sort();

            let mut changed_strs: Vec<String> = input_files
                .iter()
                .map(|f| format_path(f, &cwd, cli.relative))
                .collect();
            changed_strs.sort();

            let output = JsonOutput {
                dependees,
                changed_files: changed_strs,
                roots: root_strs,
                explanations,
                build_files: build_strs,
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&output).expect("failed to serialize JSON")
            );
        } else {
            let sep = if cli.null_separator { "\0" } else { "\n" };
            for (dep, triggers) in &entries {
                let dep_str = format_path(dep, &cwd, cli.relative);
                let trigger_strs: Vec<String> = triggers
                    .iter()
                    .map(|t| format_path(t, &cwd, cli.relative))
                    .collect();
                print!(
                    "{}  (triggered by: {}){}",
                    dep_str,
                    trigger_strs.join(", "),
                    sep
                );
            }
            for build in &build_strs {
                print!("{}{}", build, sep);
            }
        }
    } else if cli.debug {
        let mut with_reasons = trees.get_dependees_symbol_aware_with_reasons(&symbol_changes, &fallback_files);
        // Add BUILD-discovered dependees with reason
        for dep in build_dependee_files.iter() {
            with_reasons.entry(dep.clone()).or_insert_with(|| "BUILD dependency".to_string());
        }
        if cli.show_build_files {
            for dep in referencing_build_files.iter() {
                with_reasons.entry(dep.clone()).or_insert_with(|| "BUILD dependency".to_string());
            }
        }
        // Merge --stdin paths
        for path in &stdin_paths {
            with_reasons.entry(path.clone()).or_insert_with(|| "stdin".to_string());
        }

        let mut entries: Vec<(PathBuf, String)> = with_reasons
            .into_iter()
            .filter(|(p, _)| p.is_file())
            .filter(|(p, _)| !should_exclude(p, &cwd, &cli.excludes))
            .filter(|(p, _)| should_include(p, &cwd, &cli.filters))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));

        let dep_paths: Vec<PathBuf> = entries.iter().map(|(p, _)| p.clone()).collect();
        let build_strs: Vec<String> = if cli.build_files {
            let mut all_builds: HashSet<PathBuf> = HashSet::new();
            for b in collect_build_files(&dep_paths, &cli.build_file_name) {
                if cli.show_build_files || !is_referencing_build(&b, &referencing_build_files) {
                    all_builds.insert(b);
                }
            }
            let mut sorted: Vec<String> = all_builds
                .iter()
                .map(|p| format_path(p, &cwd, cli.relative))
                .collect();
            sorted.sort();
            sorted
        } else {
            Vec::new()
        };

        if cli.json {
            let mut dependees: Vec<String> = Vec::new();
            let mut reasons: HashMap<String, String> = HashMap::new();
            for (dep, reason) in &entries {
                let dep_str = format_path(dep, &cwd, cli.relative);
                dependees.push(dep_str.clone());
                reasons.insert(dep_str, reason.clone());
            }
            dependees.sort();

            let mut root_strs: Vec<String> = python_roots
                .iter()
                .map(|r| format_path(r, &cwd, cli.relative))
                .collect();
            root_strs.sort();

            let mut changed_strs: Vec<String> = input_files
                .iter()
                .map(|f| format_path(f, &cwd, cli.relative))
                .collect();
            changed_strs.sort();

            let output = JsonOutputDebug {
                dependees,
                changed_files: changed_strs,
                roots: root_strs,
                reasons,
                build_files: build_strs,
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&output).expect("failed to serialize JSON")
            );
        } else {
            let sep = if cli.null_separator { "\0" } else { "\n" };
            for (dep, reason) in &entries {
                let dep_str = format_path(dep, &cwd, cli.relative);
                print!("{}  ({}){}",dep_str, reason, sep);
            }
            for build in &build_strs {
                print!("{}{}", build, sep);
            }
        }
    } else {
        let mut dependees = trees.get_dependees_symbol_aware(&symbol_changes, &fallback_files);
        dependees.extend(build_dependee_files.iter().cloned());
        if cli.show_build_files {
            dependees.extend(referencing_build_files.iter().cloned());
        }
        // Merge --stdin paths
        dependees.extend(stdin_paths.iter().cloned());

        let mut dep_paths: Vec<PathBuf> = dependees
            .into_iter()
            .filter(|p| p.is_file())
            .filter(|p| !should_exclude(p, &cwd, &cli.excludes))
            .filter(|p| should_include(p, &cwd, &cli.filters))
            .collect();
        dep_paths.sort();

        let build_strs: Vec<String> = if cli.build_files {
            let mut all_builds: HashSet<PathBuf> = HashSet::new();
            for b in collect_build_files(&dep_paths, &cli.build_file_name) {
                if cli.show_build_files || !is_referencing_build(&b, &referencing_build_files) {
                    all_builds.insert(b);
                }
            }
            let mut sorted: Vec<String> = all_builds
                .iter()
                .map(|p| format_path(p, &cwd, cli.relative))
                .collect();
            sorted.sort();
            sorted
        } else {
            Vec::new()
        };

        if cli.json {
            let dependees: Vec<String> = dep_paths
                .iter()
                .map(|p| format_path(p, &cwd, cli.relative))
                .collect();

            let mut root_strs: Vec<String> = python_roots
                .iter()
                .map(|r| format_path(r, &cwd, cli.relative))
                .collect();
            root_strs.sort();

            let mut changed_strs: Vec<String> = input_files
                .iter()
                .map(|f| format_path(f, &cwd, cli.relative))
                .collect();
            changed_strs.sort();

            let output = JsonOutput {
                dependees,
                changed_files: changed_strs,
                roots: root_strs,
                explanations: HashMap::new(),
                build_files: build_strs,
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&output).expect("failed to serialize JSON")
            );
        } else {
            let sep = if cli.null_separator { "\0" } else { "\n" };
            for dep in &dep_paths {
                let dep_str = format_path(dep, &cwd, cli.relative);
                print!("{}{}", dep_str, sep);
            }
            for build in &build_strs {
                print!("{}{}", build, sep);
            }
        }
    }
}

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
