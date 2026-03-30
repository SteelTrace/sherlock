//! Code parsing and symbol extraction using tree-sitter.

use anyhow::{anyhow, Result};
use blake3::Hasher;
use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

use crate::types::{OutlineItem, Symbol, SymbolDetail};

// ============================================================================
// Tree-sitter queries
// ============================================================================

const JS_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @function
(class_declaration name: (identifier) @name) @class
(method_definition name: (property_identifier) @name) @method
(lexical_declaration (variable_declarator name: (identifier) @name)) @variable
(function_expression name: (identifier) @name) @function
(lexical_declaration (variable_declarator name: (identifier) @name value: (arrow_function))) @function
(lexical_declaration (variable_declarator name: (identifier) @name value: (function))) @function
"#;

const TS_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @function
(class_declaration name: (type_identifier) @name) @class
(method_definition name: (property_identifier) @name) @method
(lexical_declaration (variable_declarator name: (identifier) @name)) @variable
(function_expression name: (identifier) @name) @function
(interface_declaration name: (type_identifier) @name) @interface
(type_alias_declaration name: (type_identifier) @name) @type
(enum_declaration name: (identifier) @name) @enum
(lexical_declaration (variable_declarator name: (identifier) @name value: (arrow_function))) @function
"#;

const IMPORT_QUERY: &str = r#"
(import_statement source: (string) @source)
(export_statement source: (string) @source)
"#;

const EXPORT_QUERY_JS: &str = r#"
(export_statement
  declaration: (function_declaration name: (identifier) @name))
(export_statement
  declaration: (class_declaration name: (identifier) @name))
(export_statement
  declaration: (lexical_declaration (variable_declarator name: (identifier) @name)))
(export_specifier name: (identifier) @name)
"#;

const EXPORT_QUERY_TS: &str = r#"
(export_statement
  (function_declaration name: (identifier) @name))
(export_statement
  (lexical_declaration (variable_declarator name: (identifier) @name)))
(export_specifier name: (identifier) @name)
"#;

// Python query
const PYTHON_QUERY: &str = r#"
(function_definition name: (identifier) @name) @function
(class_definition name: (identifier) @name) @class
(assignment left: (identifier) @name) @variable
"#;

// Rust query
const RUST_QUERY: &str = r#"
(function_item name: (identifier) @name) @function
(struct_item name: (type_identifier) @name) @class
(enum_item name: (type_identifier) @name) @enum
(impl_item type: (type_identifier) @name) @class
(trait_item name: (type_identifier) @name) @interface
(type_item name: (type_identifier) @name) @type
(const_item name: (identifier) @name) @variable
(static_item name: (identifier) @name) @variable
(mod_item name: (identifier) @name) @module
"#;

// Go query
const GO_QUERY: &str = r#"
(function_declaration name: (identifier) @name) @function
(method_declaration name: (field_identifier) @name) @method
(type_declaration (type_spec name: (type_identifier) @name)) @type
(const_declaration (const_spec name: (identifier) @name)) @variable
(var_declaration (var_spec name: (identifier) @name)) @variable
"#;

// JSON query (limited - just keys at top level)
const JSON_QUERY: &str = r#"
(pair key: (string) @name) @variable
"#;

// HTML query
const HTML_QUERY: &str = r#"
(element (start_tag (tag_name) @name)) @element
"#;

// CSS query
const CSS_QUERY: &str = r#"
(rule_set (selectors (class_selector (class_name) @name))) @class
(rule_set (selectors (id_selector (id_name) @name))) @id
"#;

// Markdown query (headers)
const MARKDOWN_QUERY: &str = r#"
(atx_heading (atx_h1_marker) (inline) @name) @heading
(atx_heading (atx_h2_marker) (inline) @name) @heading
(atx_heading (atx_h3_marker) (inline) @name) @heading
"#;

// YAML query
const YAML_QUERY: &str = r#"
(block_mapping_pair key: (flow_node) @name) @variable
"#;

/// Query to extract function calls within a node
const CALLS_QUERY_JS: &str = r#"
(call_expression function: (identifier) @call)
(call_expression function: (member_expression) @call)
"#;

/// Query to extract await expressions
const AWAITS_QUERY: &str = r#"
(await_expression (call_expression function: (identifier) @await_call))
(await_expression (call_expression function: (member_expression) @await_call))
"#;

/// Query to extract throw statements
const THROWS_QUERY: &str = r#"
(throw_statement argument: (new_expression constructor: (identifier) @thrown))
(throw_statement argument: (identifier) @thrown)
"#;

/// Query to extract variable declarations (writes)
const WRITES_QUERY: &str = r#"
(variable_declarator name: (identifier) @write)
(assignment_expression left: (identifier) @write)
(assignment_expression left: (member_expression) @write)
"#;

// ============================================================================
// Public API
// ============================================================================

/// Hash text content for change detection.
pub fn hash_text(text: &str) -> String {
    let mut hasher = Hasher::new();
    hasher.update(text.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Extract all symbols, outline, imports, and exports from a file.
pub fn extract_symbols(
    path: &Path,
    text: &str,
) -> Result<(Vec<Symbol>, Vec<OutlineItem>, Vec<String>, Vec<String>)> {
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    if ext == "vue" {
        return extract_symbols_vue(path, text);
    }

    // Extract imports first (simpler query, less likely to fail)
    let imports = extract_imports(text, ext).unwrap_or_default();
    let exports = extract_exports(text, ext).unwrap_or_default();

    // Extract symbols (may fail for some language versions)
    let (symbols, outline) = match language_and_query(ext) {
        Ok((lang, query)) => {
            extract_with_tree_sitter(path, text, &lang, &query, 0).unwrap_or_default()
        }
        Err(_) => (Vec::new(), Vec::new()),
    };

    Ok((symbols, outline, imports, exports))
}

/// Extract what symbols are imported from a specific path.
pub fn extract_import_specifiers(text: &str, import_path: &str, _ext: &str) -> Result<Vec<String>> {
    let escaped_path = regex::escape(import_path);
    let pattern = format!(
        r#"import\s+(?:\*\s+as\s+(\w+)|(\w+)|\{{\s*([^}}]+)\s*\}})\s+from\s+['"]{}['"]"#,
        escaped_path
    );

    let re = match Regex::new(&pattern) {
        Ok(r) => r,
        Err(_) => return Ok(Vec::new()),
    };

    let mut specifiers = Vec::new();

    for cap in re.captures_iter(text) {
        // * as name
        if cap.get(1).is_some() {
            specifiers.push("*".to_string());
        }
        // default import
        if cap.get(2).is_some() {
            specifiers.push("default".to_string());
        }
        // named imports { foo, bar as baz }
        if let Some(m) = cap.get(3) {
            for part in m.as_str().split(',') {
                let name = part.split(" as ").next().unwrap_or("").trim();
                if !name.is_empty() {
                    specifiers.push(name.to_string());
                }
            }
        }
    }

    Ok(specifiers)
}

// ============================================================================
// Internal helpers
// ============================================================================

fn extract_symbols_vue(
    path: &Path,
    text: &str,
) -> Result<(Vec<Symbol>, Vec<OutlineItem>, Vec<String>, Vec<String>)> {
    let script_re = Regex::new(r"(?s)<script\s*(?P<attrs>[^>]*)>(?P<body>.*?)</script>")?;
    let mut symbols = Vec::new();
    let mut outline = Vec::new();
    let mut imports = Vec::new();
    let mut exports = Vec::new();

    for cap in script_re.captures_iter(text) {
        let attrs = cap.name("attrs").map(|m| m.as_str()).unwrap_or("");
        let body = cap.name("body").map(|m| m.as_str()).unwrap_or("");
        let lang = if attrs.contains("lang=\"ts\"") || attrs.contains("lang='ts'") {
            "ts"
        } else if attrs.contains("lang=\"tsx\"") || attrs.contains("lang='tsx'") {
            "tsx"
        } else {
            "js"
        };

        let offset = script_start_line(text, cap.get(0).unwrap().start())?;
        let (lang, query) = language_and_query(lang)?;
        let (mut syms, mut out) = extract_with_tree_sitter(path, body, &lang, &query, offset)?;
        symbols.append(&mut syms);
        outline.append(&mut out);

        let mut imp = extract_imports(body, "ts")?;
        imports.append(&mut imp);

        let mut exp = extract_exports(body, "ts")?;
        exports.append(&mut exp);
    }

    Ok((symbols, outline, imports, exports))
}

fn script_start_line(text: &str, script_tag_start: usize) -> Result<usize> {
    let prefix = &text[..script_tag_start];
    Ok(prefix.bytes().filter(|b| *b == b'\n').count())
}

fn language_and_query(ext: &str) -> Result<(Language, Query)> {
    let (language, query_src): (Language, &str) = match ext {
        "js" | "jsx" => (tree_sitter_javascript::LANGUAGE.into(), JS_QUERY),
        "ts" => (tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), TS_QUERY),
        "tsx" => (tree_sitter_typescript::LANGUAGE_TSX.into(), TS_QUERY),
        "py" => (tree_sitter_python::LANGUAGE.into(), PYTHON_QUERY),
        "rs" => (tree_sitter_rust::LANGUAGE.into(), RUST_QUERY),
        "go" => (tree_sitter_go::LANGUAGE.into(), GO_QUERY),
        "json" => (tree_sitter_json::LANGUAGE.into(), JSON_QUERY),
        "html" | "htm" => (tree_sitter_html::LANGUAGE.into(), HTML_QUERY),
        "css" => (tree_sitter_css::LANGUAGE.into(), CSS_QUERY),
        "md" | "markdown" => (tree_sitter_md::LANGUAGE.into(), MARKDOWN_QUERY),
        "yaml" | "yml" => (tree_sitter_yaml::LANGUAGE.into(), YAML_QUERY),
        _ => return Err(anyhow!("unsupported language: {ext}")),
    };
    let query = Query::new(&language, query_src)?;
    Ok((language, query))
}

fn extract_with_tree_sitter(
    path: &Path,
    text: &str,
    language: &Language,
    query: &Query,
    line_offset: usize,
) -> Result<(Vec<Symbol>, Vec<OutlineItem>)> {
    let mut parser = Parser::new();
    parser.set_language(language)?;
    let tree = parser
        .parse(text, None)
        .ok_or_else(|| anyhow!("parse failed"))?;

    let mut cursor = QueryCursor::new();
    let mut symbols = Vec::new();
    let mut outline = Vec::new();

    let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
    while let Some(m) = matches.next() {
        let mut name = None;
        let mut name_node = None;
        let mut kind = None;
        let mut parent_node = None;

        for c in m.captures {
            let cap_name = query.capture_names()[c.index as usize];
            let node = c.node;
            let val = node.utf8_text(text.as_bytes()).unwrap_or("");
            if cap_name == "name" {
                name = Some(val.to_string());
                name_node = Some(node);
            } else if matches!(
                cap_name,
                "function" | "class" | "method" | "variable" | "interface" | "type" | "enum"
            ) {
                kind = Some(cap_name.to_string());
                parent_node = Some(node);
            }
        }

        let name = match name {
            Some(v) => v,
            None => continue,
        };
        let kind = kind.unwrap_or_else(|| "symbol".to_string());
        let node = match name_node {
            Some(n) => n,
            None => continue,
        };
        let pos = node.start_position();

        let line = pos.row + 1 + line_offset;
        let column = pos.column + 1;

        // Calculate end line from parent node
        let end_line = parent_node
            .map(|n| n.end_position().row + 1 + line_offset)
            .unwrap_or(line);

        // Extract detailed semantic info for functions/methods
        let detail = if matches!(kind.as_str(), "function" | "method") {
            parent_node.map(|pn| extract_symbol_detail(text, pn, language))
        } else {
            None
        };

        let sym = Symbol {
            name: name.clone(),
            kind: kind.clone(),
            file: path.to_string_lossy().to_string(),
            line,
            column,
        };
        symbols.push(sym);

        outline.push(OutlineItem {
            name,
            kind,
            line,
            column,
            end_line,
            detail,
        });
    }

    Ok((symbols, outline))
}

/// Extract detailed semantic information from a function/method node.
fn extract_symbol_detail(text: &str, node: tree_sitter::Node, language: &Language) -> SymbolDetail {
    let mut detail = SymbolDetail::default();

    // Calculate lines of code
    detail.loc = node.end_position().row.saturating_sub(node.start_position().row) + 1;

    // Extract signature (first line typically)
    let start_byte = node.start_byte();
    let node_text = &text[start_byte..node.end_byte().min(text.len())];
    if let Some(first_line) = node_text.lines().next() {
        let sig = first_line.trim();
        if !sig.is_empty() {
            detail.signature = Some(sig.to_string());
        }
    }

    // Check if exported (parent is export_statement)
    if let Some(parent) = node.parent() {
        if parent.kind() == "export_statement" {
            detail.exported = true;
        }
    }

    // Extract calls, awaits, throws, writes using sub-queries
    let text_bytes = text.as_bytes();

    // Extract function calls
    if let Ok(calls_query) = Query::new(language, CALLS_QUERY_JS) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&calls_query, node, text_bytes);
        let mut calls_set = HashSet::new();
        while let Some(m) = matches.next() {
            for c in m.captures {
                if let Ok(call_text) = c.node.utf8_text(text_bytes) {
                    let call = call_text.trim().to_string();
                    if !call.is_empty() {
                        calls_set.insert(call);
                    }
                }
            }
        }
        detail.calls = calls_set.into_iter().collect();
        detail.calls.sort();
    }

    // Extract await calls
    if let Ok(awaits_query) = Query::new(language, AWAITS_QUERY) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&awaits_query, node, text_bytes);
        let mut awaits_set = HashSet::new();
        while let Some(m) = matches.next() {
            for c in m.captures {
                if let Ok(await_text) = c.node.utf8_text(text_bytes) {
                    let await_call = await_text.trim().to_string();
                    if !await_call.is_empty() {
                        awaits_set.insert(await_call);
                    }
                }
            }
        }
        detail.awaits = awaits_set.into_iter().collect();
        detail.awaits.sort();
    }

    // Extract throws
    if let Ok(throws_query) = Query::new(language, THROWS_QUERY) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&throws_query, node, text_bytes);
        let mut throws_set = HashSet::new();
        while let Some(m) = matches.next() {
            for c in m.captures {
                if let Ok(thrown_text) = c.node.utf8_text(text_bytes) {
                    let thrown = thrown_text.trim().to_string();
                    if !thrown.is_empty() {
                        throws_set.insert(thrown);
                    }
                }
            }
        }
        detail.throws = throws_set.into_iter().collect();
        detail.throws.sort();
    }

    // Extract variable writes
    if let Ok(writes_query) = Query::new(language, WRITES_QUERY) {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&writes_query, node, text_bytes);
        let mut writes_set = HashSet::new();
        while let Some(m) = matches.next() {
            for c in m.captures {
                if let Ok(write_text) = c.node.utf8_text(text_bytes) {
                    let write = write_text.trim().to_string();
                    if !write.is_empty() {
                        writes_set.insert(write);
                    }
                }
            }
        }
        detail.writes = writes_set.into_iter().collect();
        detail.writes.sort();
    }

    detail
}

// Import queries for different languages
const PYTHON_IMPORT_QUERY: &str = r#"
(import_statement (dotted_name) @source)
(import_from_statement module_name: (dotted_name) @source)
"#;

const RUST_IMPORT_QUERY: &str = r#"
(use_declaration argument: (scoped_identifier) @source)
(use_declaration argument: (identifier) @source)
(use_declaration argument: (use_as_clause path: (scoped_identifier) @source))
"#;

const GO_IMPORT_QUERY: &str = r#"
(import_declaration (import_spec path: (interpreted_string_literal) @source))
"#;

fn extract_imports(text: &str, ext: &str) -> Result<Vec<String>> {
    let (language, import_query): (Language, &str) = match ext {
        "js" | "jsx" => (tree_sitter_javascript::LANGUAGE.into(), IMPORT_QUERY),
        "ts" => (tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), IMPORT_QUERY),
        "tsx" => (tree_sitter_typescript::LANGUAGE_TSX.into(), IMPORT_QUERY),
        "py" => (tree_sitter_python::LANGUAGE.into(), PYTHON_IMPORT_QUERY),
        "rs" => (tree_sitter_rust::LANGUAGE.into(), RUST_IMPORT_QUERY),
        "go" => (tree_sitter_go::LANGUAGE.into(), GO_IMPORT_QUERY),
        _ => return Ok(Vec::new()),
    };

    let query = Query::new(&language, import_query)?;

    let mut parser = Parser::new();
    parser.set_language(&language)?;
    let tree = match parser.parse(text, None) {
        Some(t) => t,
        None => return Ok(Vec::new()),
    };

    let mut cursor = QueryCursor::new();
    let mut imports = Vec::new();

    let mut matches = cursor.matches(&query, tree.root_node(), text.as_bytes());
    while let Some(m) = matches.next() {
        for c in m.captures {
            let cap_name = query.capture_names()[c.index as usize];
            if cap_name == "source" {
                let val = c.node.utf8_text(text.as_bytes()).unwrap_or("");
                // Remove quotes from import path
                let import_path = val.trim_matches(|c| c == '"' || c == '\'');
                if !import_path.is_empty() {
                    imports.push(import_path.to_string());
                }
            }
        }
    }

    imports.sort();
    imports.dedup();
    Ok(imports)
}

// Export queries for Rust (pub items)
const RUST_EXPORT_QUERY: &str = r#"
(function_item (visibility_modifier) name: (identifier) @name)
(struct_item (visibility_modifier) name: (type_identifier) @name)
(enum_item (visibility_modifier) name: (type_identifier) @name)
(type_item (visibility_modifier) name: (type_identifier) @name)
(const_item (visibility_modifier) name: (identifier) @name)
"#;

// Export queries for Go (capitalized = exported)
const GO_EXPORT_QUERY: &str = r#"
(function_declaration name: (identifier) @name)
(method_declaration name: (field_identifier) @name)
(type_declaration (type_spec name: (type_identifier) @name))
"#;

fn extract_exports(text: &str, ext: &str) -> Result<Vec<String>> {
    let (language, query_src): (Language, &str) = match ext {
        "js" | "jsx" => (tree_sitter_javascript::LANGUAGE.into(), EXPORT_QUERY_JS),
        "ts" => (tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), EXPORT_QUERY_TS),
        "tsx" => (tree_sitter_typescript::LANGUAGE_TSX.into(), EXPORT_QUERY_TS),
        "rs" => (tree_sitter_rust::LANGUAGE.into(), RUST_EXPORT_QUERY),
        "go" => (tree_sitter_go::LANGUAGE.into(), GO_EXPORT_QUERY),
        // Python doesn't have explicit exports (everything is accessible)
        _ => return Ok(Vec::new()),
    };

    let query = match Query::new(&language, query_src) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("Export query error: {e}");
            return Ok(Vec::new());
        }
    };

    let mut parser = Parser::new();
    parser.set_language(&language)?;
    let tree = match parser.parse(text, None) {
        Some(t) => t,
        None => return Ok(Vec::new()),
    };

    let mut cursor = QueryCursor::new();
    let mut exports = Vec::new();

    let mut matches = cursor.matches(&query, tree.root_node(), text.as_bytes());
    while let Some(m) = matches.next() {
        for c in m.captures {
            let cap_name = query.capture_names()[c.index as usize];
            if cap_name == "name" {
                let val = c.node.utf8_text(text.as_bytes()).unwrap_or("");
                if !val.is_empty() {
                    exports.push(val.to_string());
                }
            }
        }
    }

    exports.sort();
    exports.dedup();
    Ok(exports)
}
