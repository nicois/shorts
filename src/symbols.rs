use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};

use ruff_python_ast::{Expr, Stmt};
use ruff_python_parser::{parse_module, parse_unchecked, Mode, ParseOptions};
use ruff_text_size::Ranged;
use serde::{Deserialize, Serialize};

use crate::imports::parse_import_statements;

/// A named top-level symbol in a module.
/// Methods within a class are part of the class symbol.
/// Module-level code outside any def/class is represented as `ModuleBody`.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum Symbol {
    Function(String),
    Class(String),
    Variable(String),
    /// Everything at module level not inside a def/class
    /// (import statements, bare expressions, if __name__ blocks, etc.)
    ModuleBody,
}

impl Symbol {
    /// String key for use in cache serialization.
    pub fn cache_key(&self) -> String {
        match self {
            Symbol::Function(name) => format!("fn:{name}"),
            Symbol::Class(name) => format!("cls:{name}"),
            Symbol::Variable(name) => format!("var:{name}"),
            Symbol::ModuleBody => "__body__".to_string(),
        }
    }

    /// Parse from cache key string.
    pub fn from_cache_key(key: &str) -> Option<Self> {
        if key == "__body__" {
            Some(Symbol::ModuleBody)
        } else if let Some(name) = key.strip_prefix("fn:") {
            Some(Symbol::Function(name.to_string()))
        } else if let Some(name) = key.strip_prefix("cls:") {
            Some(Symbol::Class(name.to_string()))
        } else if let Some(name) = key.strip_prefix("var:") {
            Some(Symbol::Variable(name.to_string()))
        } else {
            None
        }
    }
}

/// Per-symbol semantic hashes for a module.
pub type SymbolHashes = HashMap<Symbol, u64>;

/// Classify a top-level statement into a Symbol.
fn classify_stmt(stmt: &Stmt) -> Symbol {
    match stmt {
        Stmt::FunctionDef(f) => Symbol::Function(f.name.id.to_string()),
        Stmt::ClassDef(c) => Symbol::Class(c.name.id.to_string()),
        Stmt::Assign(a) => {
            // Use the first target's name if it's a simple Name expr
            if let Some(Expr::Name(name)) = a.targets.first() {
                Symbol::Variable(name.id.to_string())
            } else {
                Symbol::ModuleBody
            }
        }
        Stmt::AnnAssign(a) => {
            if let Expr::Name(name) = a.target.as_ref() {
                Symbol::Variable(name.id.to_string())
            } else {
                Symbol::ModuleBody
            }
        }
        Stmt::TypeAlias(t) => {
            if let Expr::Name(name) = t.name.as_ref() {
                Symbol::Variable(name.id.to_string())
            } else {
                Symbol::ModuleBody
            }
        }
        _ => Symbol::ModuleBody,
    }
}

/// Extract per-symbol semantic hashes from Python source code.
///
/// Each top-level function, class, and variable gets its own hash.
/// All other top-level code (imports, bare expressions, if-blocks, etc.)
/// is hashed together as `ModuleBody`.
pub fn extract_symbol_hashes(source: &str) -> SymbolHashes {
    let parsed = parse_unchecked(source, ParseOptions::from(Mode::Module));

    // Build a list of (symbol, start_offset, end_offset) for each top-level statement.
    // Statements classified as ModuleBody are merged into a single bucket.
    let module = match parsed.syntax() {
        ruff_python_ast::Mod::Module(m) => m,
        _ => return HashMap::new(),
    };

    let tokens = parsed.tokens();

    // Collect ranges for each symbol
    let mut symbol_ranges: Vec<(Symbol, usize, usize)> = Vec::new();
    for stmt in &module.body {
        let sym = classify_stmt(stmt);
        let range = stmt.range();
        let start: usize = range.start().into();
        let end: usize = range.end().into();
        symbol_ranges.push((sym, start, end));
    }

    // Hash tokens per symbol. We iterate tokens once and assign each to a symbol
    // based on which statement range contains it.
    let mut hashers: HashMap<Symbol, DefaultHasher> = HashMap::new();

    // Pre-sort symbol_ranges by start offset (they should already be in order)
    // and build an index for efficient lookup
    let mut range_idx = 0;

    for token in tokens {
        if token.kind().is_trivia() {
            continue;
        }
        let tok_range = token.range();
        let tok_start: usize = tok_range.start().into();
        let tok_end: usize = tok_range.end().into();

        // Advance range_idx to find which symbol owns this token
        while range_idx < symbol_ranges.len() && symbol_ranges[range_idx].2 <= tok_start {
            range_idx += 1;
        }

        let symbol = if range_idx < symbol_ranges.len()
            && tok_start >= symbol_ranges[range_idx].1
            && tok_end <= symbol_ranges[range_idx].2
        {
            symbol_ranges[range_idx].0.clone()
        } else {
            Symbol::ModuleBody
        };

        let hasher = hashers.entry(symbol).or_insert_with(DefaultHasher::new);
        token.kind().hash(hasher);
        source[tok_start..tok_end].hash(hasher);
    }

    hashers
        .into_iter()
        .map(|(sym, hasher)| (sym, hasher.finish()))
        .collect()
}

/// Extract intra-module dependencies: for each top-level symbol, find which
/// other top-level names it references in its body.
///
/// Returns a map from symbol name to the set of other top-level names it uses.
/// This enables propagating changes through call chains within a module:
/// if `setup_vm` calls `_setup_tenant_certificates`, changing the latter
/// effectively changes the former.
pub fn extract_intra_module_deps(source: &str) -> HashMap<String, HashSet<String>> {
    let parsed = match parse_module(source) {
        Ok(p) => p,
        Err(_) => return HashMap::new(),
    };

    // Collect all top-level symbol names
    let mut top_level_names: HashSet<String> = HashSet::new();
    for stmt in parsed.suite() {
        match classify_stmt(stmt) {
            Symbol::Function(n) | Symbol::Class(n) | Symbol::Variable(n) => {
                top_level_names.insert(n);
            }
            Symbol::ModuleBody => {}
        }
    }

    if top_level_names.is_empty() {
        return HashMap::new();
    }

    // For each named symbol, walk its AST and collect references to other top-level names
    let mut deps: HashMap<String, HashSet<String>> = HashMap::new();
    for stmt in parsed.suite() {
        let sym_name = match classify_stmt(stmt) {
            Symbol::Function(n) | Symbol::Class(n) | Symbol::Variable(n) => n,
            Symbol::ModuleBody => continue,
        };

        let mut refs = HashSet::new();
        collect_name_refs(stmt, &top_level_names, &sym_name, &mut refs);
        if !refs.is_empty() {
            deps.insert(sym_name, refs);
        }
    }

    deps
}

/// Recursively collect references to `targets` names within an expression.
fn collect_name_refs_expr(expr: &Expr, targets: &HashSet<String>, refs: &mut HashSet<String>) {
    match expr {
        Expr::Name(name) => {
            let id = name.id.as_str();
            if targets.contains(id) {
                refs.insert(id.to_string());
            }
        }
        Expr::Attribute(attr) => {
            collect_name_refs_expr(&attr.value, targets, refs);
        }
        Expr::Call(call) => {
            collect_name_refs_expr(&call.func, targets, refs);
            for arg in call.arguments.args.iter() {
                collect_name_refs_expr(arg, targets, refs);
            }
            for kw in call.arguments.keywords.iter() {
                collect_name_refs_expr(&kw.value, targets, refs);
            }
        }
        Expr::BoolOp(b) => {
            for val in &b.values {
                collect_name_refs_expr(val, targets, refs);
            }
        }
        Expr::Compare(c) => {
            collect_name_refs_expr(&c.left, targets, refs);
            for comp in &c.comparators {
                collect_name_refs_expr(comp, targets, refs);
            }
        }
        Expr::BinOp(b) => {
            collect_name_refs_expr(&b.left, targets, refs);
            collect_name_refs_expr(&b.right, targets, refs);
        }
        Expr::UnaryOp(u) => {
            collect_name_refs_expr(&u.operand, targets, refs);
        }
        Expr::If(i) => {
            collect_name_refs_expr(&i.test, targets, refs);
            collect_name_refs_expr(&i.body, targets, refs);
            collect_name_refs_expr(&i.orelse, targets, refs);
        }
        Expr::Lambda(l) => {
            collect_name_refs_expr(&l.body, targets, refs);
        }
        Expr::Dict(d) => {
            for item in &d.items {
                if let Some(key) = &item.key {
                    collect_name_refs_expr(key, targets, refs);
                }
                collect_name_refs_expr(&item.value, targets, refs);
            }
        }
        Expr::List(l) => {
            for elt in &l.elts {
                collect_name_refs_expr(elt, targets, refs);
            }
        }
        Expr::Tuple(t) => {
            for elt in &t.elts {
                collect_name_refs_expr(elt, targets, refs);
            }
        }
        Expr::Set(s) => {
            for elt in &s.elts {
                collect_name_refs_expr(elt, targets, refs);
            }
        }
        Expr::Subscript(s) => {
            collect_name_refs_expr(&s.value, targets, refs);
            collect_name_refs_expr(&s.slice, targets, refs);
        }
        Expr::Starred(s) => {
            collect_name_refs_expr(&s.value, targets, refs);
        }
        Expr::Await(a) => {
            collect_name_refs_expr(&a.value, targets, refs);
        }
        Expr::Yield(y) => {
            if let Some(val) = &y.value {
                collect_name_refs_expr(val, targets, refs);
            }
        }
        Expr::YieldFrom(y) => {
            collect_name_refs_expr(&y.value, targets, refs);
        }
        Expr::Generator(g) => {
            collect_name_refs_expr(&g.elt, targets, refs);
            for comp in &g.generators {
                collect_name_refs_expr(&comp.target, targets, refs);
                collect_name_refs_expr(&comp.iter, targets, refs);
                for cond in &comp.ifs {
                    collect_name_refs_expr(cond, targets, refs);
                }
            }
        }
        Expr::ListComp(g) => {
            collect_name_refs_expr(&g.elt, targets, refs);
            for comp in &g.generators {
                collect_name_refs_expr(&comp.target, targets, refs);
                collect_name_refs_expr(&comp.iter, targets, refs);
                for cond in &comp.ifs {
                    collect_name_refs_expr(cond, targets, refs);
                }
            }
        }
        Expr::SetComp(g) => {
            collect_name_refs_expr(&g.elt, targets, refs);
            for comp in &g.generators {
                collect_name_refs_expr(&comp.target, targets, refs);
                collect_name_refs_expr(&comp.iter, targets, refs);
                for cond in &comp.ifs {
                    collect_name_refs_expr(cond, targets, refs);
                }
            }
        }
        Expr::DictComp(d) => {
            collect_name_refs_expr(&d.key, targets, refs);
            collect_name_refs_expr(&d.value, targets, refs);
            for comp in &d.generators {
                collect_name_refs_expr(&comp.target, targets, refs);
                collect_name_refs_expr(&comp.iter, targets, refs);
                for cond in &comp.ifs {
                    collect_name_refs_expr(cond, targets, refs);
                }
            }
        }
        Expr::FString(f) => {
            for part in f.value.elements() {
                if let ruff_python_ast::InterpolatedStringElement::Interpolation(e) = part {
                    collect_name_refs_expr(&e.expression, targets, refs);
                }
            }
        }
        Expr::Named(n) => {
            collect_name_refs_expr(&n.target, targets, refs);
            collect_name_refs_expr(&n.value, targets, refs);
        }
        _ => {}
    }
}

/// Collect name references within a statement (recursing into sub-statements).
fn collect_name_refs(stmt: &Stmt, targets: &HashSet<String>, self_name: &str, refs: &mut HashSet<String>) {
    match stmt {
        Stmt::FunctionDef(f) => {
            for decorator in &f.decorator_list {
                collect_name_refs_expr(&decorator.expression, targets, refs);
            }
            // Parameters defaults and annotations
            for param in f.parameters.iter_non_variadic_params() {
                if let Some(ann) = &param.parameter.annotation {
                    collect_name_refs_expr(ann, targets, refs);
                }
                if let Some(default) = &param.default {
                    collect_name_refs_expr(default, targets, refs);
                }
            }
            if let Some(ret) = &f.returns {
                collect_name_refs_expr(ret, targets, refs);
            }
            for s in &f.body {
                collect_name_refs(s, targets, self_name, refs);
            }
        }
        Stmt::ClassDef(c) => {
            for decorator in &c.decorator_list {
                collect_name_refs_expr(&decorator.expression, targets, refs);
            }
            if let Some(args) = &c.arguments {
                for arg in args.args.iter() {
                    collect_name_refs_expr(arg, targets, refs);
                }
                for kw in args.keywords.iter() {
                    collect_name_refs_expr(&kw.value, targets, refs);
                }
            }
            for s in &c.body {
                collect_name_refs(s, targets, self_name, refs);
            }
        }
        Stmt::Return(r) => {
            if let Some(val) = &r.value {
                collect_name_refs_expr(val, targets, refs);
            }
        }
        Stmt::Assign(a) => {
            for target in &a.targets {
                collect_name_refs_expr(target, targets, refs);
            }
            collect_name_refs_expr(&a.value, targets, refs);
        }
        Stmt::AnnAssign(a) => {
            collect_name_refs_expr(&a.target, targets, refs);
            collect_name_refs_expr(&a.annotation, targets, refs);
            if let Some(val) = &a.value {
                collect_name_refs_expr(val, targets, refs);
            }
        }
        Stmt::AugAssign(a) => {
            collect_name_refs_expr(&a.target, targets, refs);
            collect_name_refs_expr(&a.value, targets, refs);
        }
        Stmt::Expr(e) => {
            collect_name_refs_expr(&e.value, targets, refs);
        }
        Stmt::If(i) => {
            collect_name_refs_expr(&i.test, targets, refs);
            for s in &i.body {
                collect_name_refs(s, targets, self_name, refs);
            }
            for clause in &i.elif_else_clauses {
                if let Some(test) = &clause.test {
                    collect_name_refs_expr(test, targets, refs);
                }
                for s in &clause.body {
                    collect_name_refs(s, targets, self_name, refs);
                }
            }
        }
        Stmt::For(f) => {
            collect_name_refs_expr(&f.target, targets, refs);
            collect_name_refs_expr(&f.iter, targets, refs);
            for s in &f.body {
                collect_name_refs(s, targets, self_name, refs);
            }
            for s in &f.orelse {
                collect_name_refs(s, targets, self_name, refs);
            }
        }
        Stmt::While(w) => {
            collect_name_refs_expr(&w.test, targets, refs);
            for s in &w.body {
                collect_name_refs(s, targets, self_name, refs);
            }
            for s in &w.orelse {
                collect_name_refs(s, targets, self_name, refs);
            }
        }
        Stmt::With(w) => {
            for item in &w.items {
                collect_name_refs_expr(&item.context_expr, targets, refs);
                if let Some(v) = &item.optional_vars {
                    collect_name_refs_expr(v, targets, refs);
                }
            }
            for s in &w.body {
                collect_name_refs(s, targets, self_name, refs);
            }
        }
        Stmt::Try(t) => {
            for s in &t.body {
                collect_name_refs(s, targets, self_name, refs);
            }
            for handler in &t.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
                if let Some(ty) = &h.type_ {
                    collect_name_refs_expr(ty, targets, refs);
                }
                for s in &h.body {
                    collect_name_refs(s, targets, self_name, refs);
                }
            }
            for s in &t.orelse {
                collect_name_refs(s, targets, self_name, refs);
            }
            for s in &t.finalbody {
                collect_name_refs(s, targets, self_name, refs);
            }
        }
        Stmt::Raise(r) => {
            if let Some(exc) = &r.exc {
                collect_name_refs_expr(exc, targets, refs);
            }
            if let Some(cause) = &r.cause {
                collect_name_refs_expr(cause, targets, refs);
            }
        }
        Stmt::Assert(a) => {
            collect_name_refs_expr(&a.test, targets, refs);
            if let Some(msg) = &a.msg {
                collect_name_refs_expr(msg, targets, refs);
            }
        }
        Stmt::Delete(d) => {
            for target in &d.targets {
                collect_name_refs_expr(target, targets, refs);
            }
        }
        Stmt::TypeAlias(t) => {
            collect_name_refs_expr(&t.name, targets, refs);
            collect_name_refs_expr(&t.value, targets, refs);
        }
        _ => {}
    }
    // Remove self-references
    refs.remove(self_name);
}

/// Given a set of directly changed symbols and an intra-module dependency map,
/// compute the transitive closure of affected symbols.
pub fn propagate_intra_module_changes(
    directly_changed: &HashSet<Symbol>,
    intra_deps: &HashMap<String, HashSet<String>>,
) -> HashSet<Symbol> {
    let mut result = directly_changed.clone();

    // Build reverse map: if A depends on B, then changing B affects A
    let mut reverse_deps: HashMap<String, HashSet<String>> = HashMap::new();
    for (sym, deps) in intra_deps {
        for dep in deps {
            reverse_deps
                .entry(dep.clone())
                .or_default()
                .insert(sym.clone());
        }
    }

    // BFS from changed symbols
    let mut pending: Vec<String> = directly_changed
        .iter()
        .filter_map(|s| match s {
            Symbol::Function(n) | Symbol::Class(n) | Symbol::Variable(n) => Some(n.clone()),
            Symbol::ModuleBody => None,
        })
        .collect();

    let mut seen: HashSet<String> = pending.iter().cloned().collect();

    while let Some(name) = pending.pop() {
        if let Some(affected) = reverse_deps.get(&name) {
            for a in affected {
                if seen.insert(a.clone()) {
                    pending.push(a.clone());
                    // Determine the symbol type from the intra_deps keys
                    // We don't know the exact type, but we can look it up
                    // from the directly_changed set or infer from usage
                    // For simplicity, add as Function (the type doesn't matter
                    // for the BFS in graph.rs — only the name is used for lookup)
                    result.insert(Symbol::Function(a.clone()));
                }
            }
        }
    }

    // If ModuleBody changed, keep it
    if directly_changed.contains(&Symbol::ModuleBody) {
        result.insert(Symbol::ModuleBody);
    }

    result
}

/// What symbols a module uses from each imported module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SymbolUsage {
    /// Specific named symbols used
    Specific(HashSet<String>),
    /// All symbols (from `import *`, `getattr`, module object escaping, etc.)
    All,
}

/// Full symbol-level import info for one file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModuleSymbolUsage {
    /// Map from imported module name -> which symbols are used from it
    pub usage: HashMap<String, SymbolUsage>,
}

impl SymbolUsage {
    fn record_specific(&mut self, symbol: &str) {
        match self {
            SymbolUsage::All => {} // already capturing everything
            SymbolUsage::Specific(set) => {
                set.insert(symbol.to_string());
            }
        }
    }

    fn upgrade_to_all(&mut self) {
        *self = SymbolUsage::All;
    }
}

impl ModuleSymbolUsage {
    fn record_specific(&mut self, module: &str, symbol: &str) {
        self.usage
            .entry(module.to_string())
            .or_insert_with(|| SymbolUsage::Specific(HashSet::new()))
            .record_specific(symbol);
    }

    fn record_all(&mut self, module: &str) {
        self.usage
            .entry(module.to_string())
            .or_insert_with(|| SymbolUsage::Specific(HashSet::new()))
            .upgrade_to_all();
    }
}

/// Extract symbol-level usage information from Python source code.
///
/// Determines which specific symbols each import actually uses,
/// enabling fine-grained dependency tracking.
pub fn extract_symbol_usage(module_class: &str, source: &str) -> ModuleSymbolUsage {
    let parsed_imports = parse_import_statements(module_class, source);

    let parsed = match parse_module(source) {
        Ok(p) => p,
        Err(_) => return ModuleSymbolUsage::default(),
    };

    // Build import maps with references that live long enough
    // We need to collect strings that outlive the visitor
    let mut from_imports_owned: Vec<(String, String, String)> = Vec::new(); // (local, module, symbol)
    let mut module_imports_owned: Vec<(String, String)> = Vec::new(); // (local, module)

    let mut result = ModuleSymbolUsage::default();

    for imp in &parsed_imports {
        if imp.is_star {
            result.record_all(&imp.module);
            continue;
        }

        if imp.is_module_import {
            // `import foo.bar` -> local name is the first component
            let local = imp.module.split('.').next().unwrap_or(&imp.module);
            module_imports_owned.push((local.to_string(), imp.module.clone()));
        } else {
            for name in &imp.names {
                // The import itself creates a dependency on the symbol
                result.record_specific(&imp.module, &name.name);
                from_imports_owned.push((
                    name.local.clone(),
                    imp.module.clone(),
                    name.name.clone(),
                ));
            }
        }
    }

    // Build reference maps pointing into the owned data
    // We can't use string references into parsed_imports because the visitor
    // needs references with lifetime 'a matching the parsed AST.
    // Instead, use a simpler approach: build HashMaps with owned keys.
    let from_imports: HashMap<String, (String, String)> = from_imports_owned
        .iter()
        .map(|(local, module, symbol)| (local.clone(), (module.clone(), symbol.clone())))
        .collect();

    let module_imports: HashMap<String, String> = module_imports_owned
        .iter()
        .map(|(local, module)| (local.clone(), module.clone()))
        .collect();

    // Walk the AST to find actual usage of imported names
    // We need to work around the lifetime issue by using a different approach
    for stmt in parsed.suite() {
        collect_usage_from_stmt(stmt, &from_imports, &module_imports, &mut result);
    }

    result
}

/// Recursively collect symbol usage from a statement and its sub-expressions.
fn collect_usage_from_stmt(
    stmt: &Stmt,
    from_imports: &HashMap<String, (String, String)>,
    module_imports: &HashMap<String, String>,
    result: &mut ModuleSymbolUsage,
) {
    match stmt {
        Stmt::Import(_) | Stmt::ImportFrom(_) => {} // skip
        _ => {
            collect_usage_from_stmt_inner(stmt, from_imports, module_imports, result);
        }
    }
}

fn collect_usage_from_stmt_inner(
    stmt: &Stmt,
    from_imports: &HashMap<String, (String, String)>,
    module_imports: &HashMap<String, String>,
    result: &mut ModuleSymbolUsage,
) {
    // Use a simple stack-based expression walker instead of the Visitor trait
    // to avoid lifetime issues
    let mut expr_stack: Vec<&Expr> = Vec::new();

    // Collect all expressions from the statement
    collect_exprs_from_stmt(stmt, &mut expr_stack, from_imports, module_imports, result);
}

fn collect_exprs_from_stmt(
    stmt: &Stmt,
    _stack: &mut Vec<&Expr>,
    from_imports: &HashMap<String, (String, String)>,
    module_imports: &HashMap<String, String>,
    result: &mut ModuleSymbolUsage,
) {
    match stmt {
        Stmt::Import(_) | Stmt::ImportFrom(_) => {}
        Stmt::FunctionDef(f) => {
            // Walk decorators
            for dec in &f.decorator_list {
                walk_expr_for_usage(&dec.expression, from_imports, module_imports, result);
            }
            // Walk parameter defaults/annotations
            for param in f
                .parameters
                .posonlyargs
                .iter()
                .chain(f.parameters.args.iter())
                .chain(f.parameters.kwonlyargs.iter())
            {
                if let Some(ann) = &param.parameter.annotation {
                    walk_expr_for_usage(ann, from_imports, module_imports, result);
                }
                if let Some(default) = &param.default {
                    walk_expr_for_usage(default, from_imports, module_imports, result);
                }
            }
            if let Some(ret) = &f.returns {
                walk_expr_for_usage(ret, from_imports, module_imports, result);
            }
            // Walk body
            for s in &f.body {
                collect_exprs_from_stmt(s, _stack, from_imports, module_imports, result);
            }
        }
        Stmt::ClassDef(c) => {
            for dec in &c.decorator_list {
                walk_expr_for_usage(&dec.expression, from_imports, module_imports, result);
            }
            if let Some(args) = &c.arguments {
                for arg in args.args.iter() {
                    walk_expr_for_usage(arg, from_imports, module_imports, result);
                }
            }
            for s in &c.body {
                collect_exprs_from_stmt(s, _stack, from_imports, module_imports, result);
            }
        }
        Stmt::Assign(a) => {
            for target in &a.targets {
                walk_expr_for_usage(target, from_imports, module_imports, result);
            }
            walk_expr_for_usage(&a.value, from_imports, module_imports, result);
        }
        Stmt::AnnAssign(a) => {
            walk_expr_for_usage(&a.target, from_imports, module_imports, result);
            walk_expr_for_usage(&a.annotation, from_imports, module_imports, result);
            if let Some(val) = &a.value {
                walk_expr_for_usage(val, from_imports, module_imports, result);
            }
        }
        Stmt::AugAssign(a) => {
            walk_expr_for_usage(&a.target, from_imports, module_imports, result);
            walk_expr_for_usage(&a.value, from_imports, module_imports, result);
        }
        Stmt::Return(r) => {
            if let Some(val) = &r.value {
                walk_expr_for_usage(val, from_imports, module_imports, result);
            }
        }
        Stmt::Expr(e) => {
            walk_expr_for_usage(&e.value, from_imports, module_imports, result);
        }
        Stmt::If(i) => {
            walk_expr_for_usage(&i.test, from_imports, module_imports, result);
            for s in &i.body {
                collect_exprs_from_stmt(s, _stack, from_imports, module_imports, result);
            }
            for clause in &i.elif_else_clauses {
                if let Some(test) = &clause.test {
                    walk_expr_for_usage(test, from_imports, module_imports, result);
                }
                for s in &clause.body {
                    collect_exprs_from_stmt(s, _stack, from_imports, module_imports, result);
                }
            }
        }
        Stmt::For(f) => {
            walk_expr_for_usage(&f.target, from_imports, module_imports, result);
            walk_expr_for_usage(&f.iter, from_imports, module_imports, result);
            for s in &f.body {
                collect_exprs_from_stmt(s, _stack, from_imports, module_imports, result);
            }
            for s in &f.orelse {
                collect_exprs_from_stmt(s, _stack, from_imports, module_imports, result);
            }
        }
        Stmt::While(w) => {
            walk_expr_for_usage(&w.test, from_imports, module_imports, result);
            for s in &w.body {
                collect_exprs_from_stmt(s, _stack, from_imports, module_imports, result);
            }
            for s in &w.orelse {
                collect_exprs_from_stmt(s, _stack, from_imports, module_imports, result);
            }
        }
        Stmt::With(w) => {
            for item in &w.items {
                walk_expr_for_usage(&item.context_expr, from_imports, module_imports, result);
                if let Some(v) = &item.optional_vars {
                    walk_expr_for_usage(v, from_imports, module_imports, result);
                }
            }
            for s in &w.body {
                collect_exprs_from_stmt(s, _stack, from_imports, module_imports, result);
            }
        }
        Stmt::Try(t) => {
            for s in &t.body {
                collect_exprs_from_stmt(s, _stack, from_imports, module_imports, result);
            }
            for handler in &t.handlers {
                if let Some(h) = handler.as_except_handler() {
                    if let Some(ty) = &h.type_ {
                        walk_expr_for_usage(ty, from_imports, module_imports, result);
                    }
                    for s in &h.body {
                        collect_exprs_from_stmt(s, _stack, from_imports, module_imports, result);
                    }
                }
            }
            for s in &t.orelse {
                collect_exprs_from_stmt(s, _stack, from_imports, module_imports, result);
            }
            for s in &t.finalbody {
                collect_exprs_from_stmt(s, _stack, from_imports, module_imports, result);
            }
        }
        Stmt::Raise(r) => {
            if let Some(exc) = &r.exc {
                walk_expr_for_usage(exc, from_imports, module_imports, result);
            }
            if let Some(cause) = &r.cause {
                walk_expr_for_usage(cause, from_imports, module_imports, result);
            }
        }
        Stmt::Assert(a) => {
            walk_expr_for_usage(&a.test, from_imports, module_imports, result);
            if let Some(msg) = &a.msg {
                walk_expr_for_usage(msg, from_imports, module_imports, result);
            }
        }
        Stmt::Delete(d) => {
            for target in &d.targets {
                walk_expr_for_usage(target, from_imports, module_imports, result);
            }
        }
        Stmt::TypeAlias(t) => {
            walk_expr_for_usage(&t.name, from_imports, module_imports, result);
            walk_expr_for_usage(&t.value, from_imports, module_imports, result);
        }
        Stmt::Global(_) | Stmt::Nonlocal(_) | Stmt::Pass(_) | Stmt::Break(_)
        | Stmt::Continue(_) => {}
        _ => {}
    }
}

/// Walk an expression tree looking for imported name references.
/// Resolve a chained attribute expression to (module, symbol) for module imports.
///
/// For `import myapp.base` where local="myapp" maps to module="myapp.base":
/// - `myapp.base.foo` → Attribute(Attribute(Name("myapp"), "base"), "foo")
///   Collects chain ["myapp", "base", "foo"], finds "myapp" → "myapp.base",
///   checks that "myapp.base" matches prefix, returns ("myapp.base", "foo").
///
/// Returns None if the expression doesn't match any module import chain.
fn resolve_module_attr_chain(
    expr: &Expr,
    module_imports: &HashMap<String, String>,
) -> Option<(String, String)> {
    // Collect the full dotted chain from the attribute expression
    let mut parts = Vec::new();
    let mut current = expr;
    loop {
        match current {
            Expr::Attribute(attr) => {
                parts.push(attr.attr.id.as_str());
                current = &attr.value;
            }
            Expr::Name(name) => {
                parts.push(name.id.as_str());
                break;
            }
            _ => return None,
        }
    }
    parts.reverse(); // now [root, part1, part2, ..., symbol]

    if parts.len() < 2 {
        return None;
    }

    // Check if the root is a module import
    let root = parts[0];
    let module = module_imports.get(root)?;

    // The module path has N components. The chain must have at least N+1 parts
    // (the module components accessed via attributes + the actual symbol).
    let module_parts: Vec<&str> = module.split('.').collect();

    // Verify the chain matches the module path
    let chain_prefix = parts[..parts.len() - 1].join(".");
    if chain_prefix == *module {
        // Last part is the symbol
        Some((module.clone(), parts.last().unwrap().to_string()))
    } else if module.starts_with(&chain_prefix) || chain_prefix.starts_with(module) {
        // Partial match — could be deeper access or shorter
        // If chain has fewer parts than module, we're accessing an intermediate package
        // If chain has more parts, the extras after module are symbols/sub-attrs
        if parts.len() > module_parts.len() {
            // parts beyond the module are symbol access
            let symbol = parts[module_parts.len()];
            Some((module.clone(), symbol.to_string()))
        } else {
            None
        }
    } else {
        None
    }
}

fn walk_expr_for_usage(
    expr: &Expr,
    from_imports: &HashMap<String, (String, String)>,
    module_imports: &HashMap<String, String>,
    result: &mut ModuleSymbolUsage,
) {
    match expr {
        Expr::Attribute(attr) => {
            // Try to resolve chained attribute access to a module import.
            // For `import myapp.base` (local="myapp", module="myapp.base"),
            // `myapp.base.foo` is Attribute(Attribute(Name("myapp"), "base"), "foo")
            if let Some((module, symbol)) = resolve_module_attr_chain(expr, module_imports) {
                result.record_specific(&module, &symbol);
                return;
            }
            // Check for from-import used as module: `from X import Y; Y.attr()`
            // This handles `from aiven.logic import vm; vm.setup_vm()`
            // which should record usage of setup_vm from aiven.logic.vm
            if let Expr::Name(name) = attr.value.as_ref() {
                if let Some((parent_module, imported_name)) = from_imports.get(name.id.as_str()) {
                    // Record attribute access as symbol usage of the submodule
                    let submodule = format!("{parent_module}.{imported_name}");
                    result.record_specific(&submodule, attr.attr.id.as_str());
                    return;
                }
            }
            // Otherwise recurse into value
            walk_expr_for_usage(&attr.value, from_imports, module_imports, result);
        }
        Expr::Name(name) => {
            let id = name.id.as_str();
            if let Some((module, symbol)) = from_imports.get(id) {
                result.record_specific(module, symbol);
            }
            // Bare module name usage (not attribute access) = module escaping
            if module_imports.contains_key(id) {
                result.record_all(module_imports.get(id).unwrap());
            }
        }
        Expr::Call(call) => {
            // Check for getattr(module, ...) pattern
            if let Expr::Name(func_name) = call.func.as_ref() {
                if func_name.id.as_str() == "getattr" {
                    if let Some(first_arg) = call.arguments.args.first() {
                        if let Expr::Name(arg_name) = first_arg {
                            if let Some(module) = module_imports.get(arg_name.id.as_str()) {
                                result.record_all(module);
                                return;
                            }
                        }
                    }
                }
            }
            // Recurse into function and arguments
            walk_expr_for_usage(&call.func, from_imports, module_imports, result);
            for arg in call.arguments.args.iter() {
                walk_expr_for_usage(arg, from_imports, module_imports, result);
            }
            for kw in call.arguments.keywords.iter() {
                walk_expr_for_usage(&kw.value, from_imports, module_imports, result);
            }
        }
        Expr::BoolOp(b) => {
            for val in &b.values {
                walk_expr_for_usage(val, from_imports, module_imports, result);
            }
        }
        Expr::Compare(c) => {
            walk_expr_for_usage(&c.left, from_imports, module_imports, result);
            for comp in &c.comparators {
                walk_expr_for_usage(comp, from_imports, module_imports, result);
            }
        }
        Expr::BinOp(b) => {
            walk_expr_for_usage(&b.left, from_imports, module_imports, result);
            walk_expr_for_usage(&b.right, from_imports, module_imports, result);
        }
        Expr::UnaryOp(u) => {
            walk_expr_for_usage(&u.operand, from_imports, module_imports, result);
        }
        Expr::If(i) => {
            walk_expr_for_usage(&i.test, from_imports, module_imports, result);
            walk_expr_for_usage(&i.body, from_imports, module_imports, result);
            walk_expr_for_usage(&i.orelse, from_imports, module_imports, result);
        }
        Expr::Lambda(l) => {
            walk_expr_for_usage(&l.body, from_imports, module_imports, result);
        }
        Expr::Dict(d) => {
            for item in &d.items {
                if let Some(key) = &item.key {
                    walk_expr_for_usage(key, from_imports, module_imports, result);
                }
                walk_expr_for_usage(&item.value, from_imports, module_imports, result);
            }
        }
        Expr::Set(s) => {
            for elt in &s.elts {
                walk_expr_for_usage(elt, from_imports, module_imports, result);
            }
        }
        Expr::List(l) => {
            for elt in &l.elts {
                walk_expr_for_usage(elt, from_imports, module_imports, result);
            }
        }
        Expr::Tuple(t) => {
            for elt in &t.elts {
                walk_expr_for_usage(elt, from_imports, module_imports, result);
            }
        }
        Expr::Subscript(s) => {
            walk_expr_for_usage(&s.value, from_imports, module_imports, result);
            walk_expr_for_usage(&s.slice, from_imports, module_imports, result);
        }
        Expr::Starred(s) => {
            walk_expr_for_usage(&s.value, from_imports, module_imports, result);
        }
        Expr::Await(a) => {
            walk_expr_for_usage(&a.value, from_imports, module_imports, result);
        }
        Expr::Yield(y) => {
            if let Some(val) = &y.value {
                walk_expr_for_usage(val, from_imports, module_imports, result);
            }
        }
        Expr::YieldFrom(y) => {
            walk_expr_for_usage(&y.value, from_imports, module_imports, result);
        }
        Expr::Generator(g) => {
            walk_expr_for_usage(&g.elt, from_imports, module_imports, result);
            for comp in &g.generators {
                walk_expr_for_usage(&comp.target, from_imports, module_imports, result);
                walk_expr_for_usage(&comp.iter, from_imports, module_imports, result);
                for cond in &comp.ifs {
                    walk_expr_for_usage(cond, from_imports, module_imports, result);
                }
            }
        }
        Expr::ListComp(l) => {
            walk_expr_for_usage(&l.elt, from_imports, module_imports, result);
            for comp in &l.generators {
                walk_expr_for_usage(&comp.target, from_imports, module_imports, result);
                walk_expr_for_usage(&comp.iter, from_imports, module_imports, result);
                for cond in &comp.ifs {
                    walk_expr_for_usage(cond, from_imports, module_imports, result);
                }
            }
        }
        Expr::SetComp(s) => {
            walk_expr_for_usage(&s.elt, from_imports, module_imports, result);
            for comp in &s.generators {
                walk_expr_for_usage(&comp.target, from_imports, module_imports, result);
                walk_expr_for_usage(&comp.iter, from_imports, module_imports, result);
                for cond in &comp.ifs {
                    walk_expr_for_usage(cond, from_imports, module_imports, result);
                }
            }
        }
        Expr::DictComp(d) => {
            walk_expr_for_usage(&d.key, from_imports, module_imports, result);
            walk_expr_for_usage(&d.value, from_imports, module_imports, result);
            for comp in &d.generators {
                walk_expr_for_usage(&comp.target, from_imports, module_imports, result);
                walk_expr_for_usage(&comp.iter, from_imports, module_imports, result);
                for cond in &comp.ifs {
                    walk_expr_for_usage(cond, from_imports, module_imports, result);
                }
            }
        }
        Expr::Named(n) => {
            walk_expr_for_usage(&n.target, from_imports, module_imports, result);
            walk_expr_for_usage(&n.value, from_imports, module_imports, result);
        }
        // Literals and other leaf nodes — no recursion needed
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_function() {
        let hashes = extract_symbol_hashes("def foo():\n    pass\n");
        assert!(hashes.contains_key(&Symbol::Function("foo".into())));
    }

    #[test]
    fn test_classify_class() {
        let hashes = extract_symbol_hashes("class Foo:\n    pass\n");
        assert!(hashes.contains_key(&Symbol::Class("Foo".into())));
    }

    #[test]
    fn test_classify_variable() {
        let hashes = extract_symbol_hashes("X = 1\n");
        assert!(hashes.contains_key(&Symbol::Variable("X".into())));
    }

    #[test]
    fn test_classify_annotated_variable() {
        let hashes = extract_symbol_hashes("X: int = 1\n");
        assert!(hashes.contains_key(&Symbol::Variable("X".into())));
    }

    #[test]
    fn test_imports_are_module_body() {
        let hashes = extract_symbol_hashes("import foo\nfrom bar import baz\n");
        assert!(hashes.contains_key(&Symbol::ModuleBody));
        assert!(!hashes.contains_key(&Symbol::Function("foo".into())));
    }

    #[test]
    fn test_changing_one_function_doesnt_affect_another() {
        let source_v1 = "def foo():\n    return 1\n\ndef bar():\n    return 2\n";
        let source_v2 = "def foo():\n    return 99\n\ndef bar():\n    return 2\n";

        let hashes_v1 = extract_symbol_hashes(source_v1);
        let hashes_v2 = extract_symbol_hashes(source_v2);

        // foo changed
        assert_ne!(
            hashes_v1.get(&Symbol::Function("foo".into())),
            hashes_v2.get(&Symbol::Function("foo".into()))
        );
        // bar did not change
        assert_eq!(
            hashes_v1.get(&Symbol::Function("bar".into())),
            hashes_v2.get(&Symbol::Function("bar".into()))
        );
    }

    #[test]
    fn test_class_hash_includes_methods() {
        let source_v1 = "class Foo:\n    def method(self):\n        return 1\n";
        let source_v2 = "class Foo:\n    def method(self):\n        return 2\n";

        let hashes_v1 = extract_symbol_hashes(source_v1);
        let hashes_v2 = extract_symbol_hashes(source_v2);

        assert_ne!(
            hashes_v1.get(&Symbol::Class("Foo".into())),
            hashes_v2.get(&Symbol::Class("Foo".into()))
        );
    }

    #[test]
    fn test_comment_in_function_doesnt_change_hash() {
        let source_v1 = "def foo():\n    return 1\n";
        let source_v2 = "def foo():\n    # a comment\n    return 1\n";

        let hashes_v1 = extract_symbol_hashes(source_v1);
        let hashes_v2 = extract_symbol_hashes(source_v2);

        assert_eq!(
            hashes_v1.get(&Symbol::Function("foo".into())),
            hashes_v2.get(&Symbol::Function("foo".into()))
        );
    }

    #[test]
    fn test_symbol_cache_key_roundtrip() {
        let symbols = vec![
            Symbol::Function("foo".into()),
            Symbol::Class("Bar".into()),
            Symbol::Variable("X".into()),
            Symbol::ModuleBody,
        ];
        for sym in symbols {
            let key = sym.cache_key();
            let restored = Symbol::from_cache_key(&key).unwrap();
            assert_eq!(sym, restored);
        }
    }

    // Symbol usage tests

    #[test]
    fn test_usage_from_import_specific() {
        let usage = extract_symbol_usage("", "from foo import bar, baz\nbar()\n");
        match usage.usage.get("foo") {
            Some(SymbolUsage::Specific(set)) => {
                assert!(set.contains("bar"), "should record bar usage, got {:?}", set);
            }
            other => panic!("expected Specific usage, got {:?}", other),
        }
    }

    #[test]
    fn test_usage_import_attribute() {
        let usage = extract_symbol_usage("", "import foo\nfoo.bar()\n");
        match usage.usage.get("foo") {
            Some(SymbolUsage::Specific(set)) => {
                assert!(set.contains("bar"), "should record bar usage, got {:?}", set);
            }
            other => panic!("expected Specific usage for foo, got {:?}", other),
        }
    }

    #[test]
    fn test_usage_star_import() {
        let usage = extract_symbol_usage("", "from foo import *\n");
        assert!(
            matches!(usage.usage.get("foo"), Some(SymbolUsage::All)),
            "star import should be All, got {:?}",
            usage.usage.get("foo")
        );
    }

    #[test]
    fn test_usage_bare_module_name() {
        let usage = extract_symbol_usage("", "import foo\nx = foo\n");
        assert!(
            matches!(usage.usage.get("foo"), Some(SymbolUsage::All)),
            "bare module name should be All, got {:?}",
            usage.usage.get("foo")
        );
    }

    #[test]
    fn test_usage_getattr() {
        let usage = extract_symbol_usage("", "import foo\ngetattr(foo, 'bar')\n");
        assert!(
            matches!(usage.usage.get("foo"), Some(SymbolUsage::All)),
            "getattr should be All, got {:?}",
            usage.usage.get("foo")
        );
    }

    #[test]
    fn test_usage_aliased_import() {
        let usage = extract_symbol_usage("", "from foo import bar as b\nb()\n");
        match usage.usage.get("foo") {
            Some(SymbolUsage::Specific(set)) => {
                assert!(set.contains("bar"), "should record bar (not b), got {:?}", set);
            }
            other => panic!("expected Specific usage, got {:?}", other),
        }
    }

    #[test]
    fn test_usage_import_always_records_dependency() {
        // `from foo import bar, baz` — both bar and baz create a dependency
        // even if bar is never referenced in the body. The import itself is
        // a dependency: the module re-exports the symbol and other modules
        // may import it from here.
        let usage = extract_symbol_usage("", "from foo import bar, baz\nbaz()\n");
        match usage.usage.get("foo") {
            Some(SymbolUsage::Specific(set)) => {
                assert!(set.contains("baz"), "should record baz");
                assert!(set.contains("bar"), "should record bar — import creates dependency");
            }
            other => panic!("expected Specific usage, got {:?}", other),
        }
    }

    #[test]
    fn test_usage_multiple_attributes() {
        let usage = extract_symbol_usage("", "import foo\nfoo.bar()\nfoo.baz()\n");
        match usage.usage.get("foo") {
            Some(SymbolUsage::Specific(set)) => {
                assert!(set.contains("bar"));
                assert!(set.contains("baz"));
            }
            other => panic!("expected Specific usage, got {:?}", other),
        }
    }

    #[test]
    fn test_intra_module_deps_simple() {
        let source = r#"
def helper():
    return 42

def main():
    return helper()
"#;
        let deps = extract_intra_module_deps(source);
        assert!(deps.get("main").unwrap().contains("helper"),
            "main should depend on helper, got: {:?}", deps);
        assert!(deps.get("helper").is_none() || !deps.get("helper").unwrap().contains("main"),
            "helper should not depend on main");
    }

    #[test]
    fn test_intra_module_deps_transitive() {
        let source = r#"
def a():
    return b()

def b():
    return c()

def c():
    return 42
"#;
        let deps = extract_intra_module_deps(source);
        assert!(deps.get("a").unwrap().contains("b"));
        assert!(deps.get("b").unwrap().contains("c"));
        // a does NOT directly reference c (only via b)
        assert!(!deps.get("a").unwrap().contains("c"));
    }

    #[test]
    fn test_propagate_intra_module_changes() {
        // setup_vm calls _setup_tenant_certificates
        // _setup_tenant_certificates changed -> setup_vm should be affected
        let source = r#"
def _setup_tenant_certificates():
    return 42

def setup_vm():
    certs = _setup_tenant_certificates()
    return certs

def unrelated():
    return 0
"#;
        let deps = extract_intra_module_deps(source);
        let directly_changed: HashSet<Symbol> = [
            Symbol::Function("_setup_tenant_certificates".to_string()),
        ].into();

        let propagated = propagate_intra_module_changes(&directly_changed, &deps);
        assert!(propagated.contains(&Symbol::Function("_setup_tenant_certificates".to_string())));
        assert!(propagated.contains(&Symbol::Function("setup_vm".to_string())),
            "setup_vm calls _setup_tenant_certificates, should be affected, got: {:?}", propagated);
        assert!(!propagated.contains(&Symbol::Function("unrelated".to_string())),
            "unrelated should not be affected");
    }

    #[test]
    fn test_propagate_transitive_chain() {
        // a -> b -> c; c changes -> b and a should both be affected
        let source = r#"
def a():
    return b()

def b():
    return c()

def c():
    return 42
"#;
        let deps = extract_intra_module_deps(source);
        let directly_changed: HashSet<Symbol> = [
            Symbol::Function("c".to_string()),
        ].into();

        let propagated = propagate_intra_module_changes(&directly_changed, &deps);
        assert!(propagated.contains(&Symbol::Function("c".to_string())));
        assert!(propagated.contains(&Symbol::Function("b".to_string())));
        assert!(propagated.contains(&Symbol::Function("a".to_string())));
    }

    #[test]
    fn test_intra_module_deps_class_reference() {
        let source = r#"
class Base:
    pass

class Child(Base):
    pass
"#;
        let deps = extract_intra_module_deps(source);
        assert!(deps.get("Child").unwrap().contains("Base"),
            "Child should depend on Base, got: {:?}", deps);
    }

    #[test]
    fn test_intra_module_deps_variable_reference() {
        let source = r#"
DEFAULT_CONFIG = {"key": "value"}

def get_config():
    return DEFAULT_CONFIG.copy()
"#;
        let deps = extract_intra_module_deps(source);
        assert!(deps.get("get_config").unwrap().contains("DEFAULT_CONFIG"),
            "get_config should depend on DEFAULT_CONFIG, got: {:?}", deps);
    }
}
