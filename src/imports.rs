use std::collections::HashSet;

use ruff_python_ast::Stmt;
use ruff_python_parser::parse_module;

/// A single imported name from an import statement.
#[derive(Debug, Clone)]
pub struct ImportedName {
    /// The symbol name in the source module
    pub name: String,
    /// The local binding (may differ if `as` is used)
    pub local: String,
}

/// Parsed representation of a single import statement.
#[derive(Debug, Clone)]
pub struct ParsedImport {
    /// Resolved absolute module path
    pub module: String,
    /// Specific names imported (empty for `import X` style)
    pub names: Vec<ImportedName>,
    /// True for `from X import *`
    pub is_star: bool,
    /// True for `import X` (module-level import, not `from X import ...`)
    pub is_module_import: bool,
}

/// Parse import statements from Python source code into structured form.
///
/// `module_class` is the dotted module path of the file being parsed
/// (e.g. `"myapp.utils"` for `myapp/utils.py`).
pub fn parse_import_statements(module_class: &str, source: &str) -> Vec<ParsedImport> {
    let mut result = Vec::new();

    let parsed = match parse_module(source) {
        Ok(parsed) => parsed,
        Err(_) => return result,
    };

    for stmt in parsed.suite() {
        match stmt {
            Stmt::Import(import_stmt) => {
                for alias in &import_stmt.names {
                    let name = alias.name.id.as_str();
                    result.push(ParsedImport {
                        module: name.to_string(),
                        names: Vec::new(),
                        is_star: false,
                        is_module_import: true,
                    });
                }
            }
            Stmt::ImportFrom(import_from) => {
                let base = if import_from.level > 0 {
                    resolve_relative(
                        module_class,
                        import_from.level,
                        import_from.module.as_ref().map(|id| id.id.as_str()),
                    )
                } else {
                    import_from
                        .module
                        .as_ref()
                        .map(|id| id.id.as_str().to_string())
                };

                if let Some(base) = base {
                    // Check for star import
                    let is_star = import_from
                        .names
                        .iter()
                        .any(|a| a.name.id.as_str() == "*");

                    if is_star {
                        result.push(ParsedImport {
                            module: base,
                            names: Vec::new(),
                            is_star: true,
                            is_module_import: false,
                        });
                    } else {
                        let names: Vec<ImportedName> = import_from
                            .names
                            .iter()
                            .map(|alias| {
                                let name = alias.name.id.as_str().to_string();
                                let local = alias
                                    .asname
                                    .as_ref()
                                    .map(|a| a.id.as_str().to_string())
                                    .unwrap_or_else(|| name.clone());
                                ImportedName { name, local }
                            })
                            .collect();
                        result.push(ParsedImport {
                            module: base,
                            names,
                            is_star: false,
                            is_module_import: false,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    result
}

/// Extract all possible import targets from Python source code.
///
/// `module_class` is the dotted module path of the file being parsed
/// (e.g. `"myapp.utils"` for `myapp/utils.py`, `"myapp.__init__"` for `myapp/__init__.py`).
/// It is used to resolve relative imports.
pub fn extract_imports(module_class: &str, source: &str) -> HashSet<String> {
    let mut result = HashSet::new();

    let parsed = match parse_module(source) {
        Ok(parsed) => parsed,
        Err(_) => return result,
    };

    for stmt in parsed.suite() {
        match stmt {
            Stmt::Import(import_stmt) => {
                for alias in &import_stmt.names {
                    let name = alias.name.id.as_str();
                    result.insert(name.to_string());
                    result.insert(format!("{name}.__init__"));
                    // Add immediate parent package (matches Go behavior)
                    if let Some(dot_idx) = name.rfind('.') {
                        result.insert(name[..dot_idx].to_string());
                    }
                }
            }
            Stmt::ImportFrom(import_from) => {
                let base = if import_from.level > 0 {
                    // Relative import: resolve against module_class
                    resolve_relative(module_class, import_from.level, import_from.module.as_ref().map(|id| id.id.as_str()))
                } else {
                    // Absolute import
                    import_from.module.as_ref().map(|id| id.id.as_str().to_string())
                };

                if let Some(base) = base {
                    result.insert(base.clone());
                    for alias in &import_from.names {
                        let name = alias.name.id.as_str();
                        let qualified = format!("{base}.{name}");
                        result.insert(qualified.clone());
                        result.insert(format!("{qualified}.__init__"));
                    }
                }
            }
            _ => {}
        }
    }

    result
}

/// Resolve a relative import base path.
///
/// Given `module_class` (the current file's dotted path), `level` (number of dots),
/// and an optional `module` name after the dots, compute the absolute base module path.
fn resolve_relative(module_class: &str, level: u32, module: Option<&str>) -> Option<String> {
    if module_class.is_empty() {
        return None;
    }

    let parts: Vec<&str> = module_class.split('.').collect();

    // Each level strips one component. If the last component is __init__,
    // the first dot stays at the package level (so __init__ counts as one
    // component to strip for "free" with the first dot).
    let strip_count = if parts.last() == Some(&"__init__") {
        // __init__ means we're in a package; first dot keeps us at the package level
        // so we strip __init__ + (level - 1) more components
        1 + (level - 1) as usize
    } else {
        // Regular module: each dot strips one component
        level as usize
    };

    if strip_count > parts.len() {
        return None;
    }

    let base_parts = &parts[..parts.len() - strip_count];

    let base = if base_parts.is_empty() {
        match module {
            Some(m) => m.to_string(),
            None => return None,
        }
    } else {
        let prefix = base_parts.join(".");
        match module {
            Some(m) => format!("{prefix}.{m}"),
            None => prefix,
        }
    };

    Some(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_import() {
        let result = extract_imports("", "import foo\n");
        assert!(result.contains("foo"));
        assert!(result.contains("foo.__init__"));
    }

    #[test]
    fn test_multiple_imports() {
        let result = extract_imports("", "import foo\nimport bar\n");
        assert!(result.contains("foo"));
        assert!(result.contains("foo.__init__"));
        assert!(result.contains("bar"));
        assert!(result.contains("bar.__init__"));
    }

    #[test]
    fn test_comma_separated_imports() {
        let result = extract_imports("myapp", "import foo, bar, baz\n");
        assert!(result.contains("foo"));
        assert!(result.contains("bar"));
        assert!(result.contains("baz"));
    }

    #[test]
    fn test_from_import() {
        let result = extract_imports("", "from foo import baz\n");
        assert!(result.contains("foo.baz"));
        assert!(result.contains("foo"));
        assert!(result.contains("foo.baz.__init__"));
    }

    #[test]
    fn test_from_import_multiline() {
        let source = "from foo import (bar,\n    baz,\n    biffo, bert)\n";
        let result = extract_imports("", source);
        for name in &["foo.bar", "foo.baz", "foo.biffo", "foo.bert", "foo"] {
            assert!(result.contains(*name), "missing {name}");
        }
    }

    #[test]
    fn test_relative_import_single_dot() {
        let result = extract_imports("myapp.__init__", "from .sibling import foo, bar, baz\n");
        assert!(result.contains("myapp.sibling.foo"));
        assert!(result.contains("myapp.sibling.bar"));
        assert!(result.contains("myapp.sibling.baz"));
        assert!(result.contains("myapp.sibling"));
    }

    #[test]
    fn test_relative_import_double_dot() {
        let result = extract_imports("myapp.utils.__init__", "from ..aunt import foo\n");
        assert!(result.contains("myapp.aunt.foo"));
        assert!(result.contains("myapp.aunt"));
    }

    #[test]
    fn test_absolute_from_import() {
        let result = extract_imports(
            "myapp.utils.__init__",
            "from myapp.deploy.services import foo, bar\n",
        );
        assert!(result.contains("myapp.deploy.services.foo"));
        assert!(result.contains("myapp.deploy.services.bar"));
        assert!(result.contains("myapp.deploy.services"));
    }

    #[test]
    fn test_dotted_import_parent_package() {
        let result = extract_imports("", "import foo.bar.baz\n");
        assert!(result.contains("foo.bar.baz"));
        assert!(result.contains("foo.bar.baz.__init__"));
        assert!(result.contains("foo.bar"), "should include parent package");
    }

    #[test]
    fn test_import_in_comment_ignored() {
        let result = extract_imports("", "# import foo\nimport bar\n");
        assert!(!result.contains("foo"));
        assert!(result.contains("bar"));
    }

    #[test]
    fn test_import_in_string_ignored() {
        let result = extract_imports("", "x = 'import foo'\nimport bar\n");
        assert!(!result.contains("foo"));
        assert!(result.contains("bar"));
    }

    // Tests for parse_import_statements

    #[test]
    fn test_parsed_from_import() {
        let imports = parse_import_statements("", "from foo import bar, baz\n");
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].module, "foo");
        assert!(!imports[0].is_star);
        assert!(!imports[0].is_module_import);
        assert_eq!(imports[0].names.len(), 2);
        assert_eq!(imports[0].names[0].name, "bar");
        assert_eq!(imports[0].names[0].local, "bar");
        assert_eq!(imports[0].names[1].name, "baz");
    }

    #[test]
    fn test_parsed_import_module() {
        let imports = parse_import_statements("", "import foo\n");
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].module, "foo");
        assert!(imports[0].is_module_import);
        assert!(imports[0].names.is_empty());
    }

    #[test]
    fn test_parsed_star_import() {
        let imports = parse_import_statements("", "from foo import *\n");
        assert_eq!(imports.len(), 1);
        assert!(imports[0].is_star);
        assert_eq!(imports[0].module, "foo");
    }

    #[test]
    fn test_parsed_aliased_import() {
        let imports = parse_import_statements("", "from foo import bar as b\n");
        assert_eq!(imports[0].names[0].name, "bar");
        assert_eq!(imports[0].names[0].local, "b");
    }
}
