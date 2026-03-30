//! MCP tool implementations.

use anyhow::{anyhow, Context, Result};
use glob::Pattern;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::index::{index_file, IndexState};
use crate::parser::extract_import_specifiers;
use crate::types::ToolCallResponse;
use crate::utils::{
    attach_next_cursor, format_path, paginate_range, parse_pagination, resolve_import,
    resolve_path, symbol_to_value, SymbolMatcher, TextMatcher,
};

/// Sanitize file_pattern by stripping the root directory name prefix if present.
/// This handles cases where editors (like Zed) incorrectly include the root folder name.
/// e.g., if root is "/Users/foo/platform" and pattern is "platform/**/*",
/// we strip it to "**/*".
fn sanitize_file_pattern(root: &std::path::Path, pattern: &str) -> String {
    if let Some(root_name) = root.file_name().and_then(|n| n.to_str()) {
        // Check if pattern starts with "root_name/" or "root_name/**" etc.
        if pattern.starts_with(root_name) {
            let rest = &pattern[root_name.len()..];
            if rest.is_empty() {
                // Pattern is exactly the root name -> match everything
                return "**/*".to_string();
            } else if rest.starts_with('/') {
                // Strip "root_name/" prefix
                return rest[1..].to_string();
            }
        }
    }
    pattern.to_string()
}

// ============================================================================
// Tool schemas
// ============================================================================

pub fn tool_schemas() -> Vec<Value> {
    vec![
        json!({
            "name": "list_files",
            "title": "List Files",
            "description": "List all supported files in the codebase (JS/TS/Vue/Python/Rust/Go/JSON/HTML/CSS/Markdown/YAML). Use to discover project structure.",
            "inputSchema": {
                "type":"object",
                "properties":{
                    "cursor":{"type":"string", "description": "Pagination cursor from previous response"},
                    "limit":{"type":"integer", "description": "Max number of results to return (default: 200, max: 1000)"}
                },
                "additionalProperties":false
            }
        }),
        json!({
            "name": "file_outline",
            "title": "File Outline",
            "description": "Get structure of a file: functions, classes, interfaces with their locations. Use before read_symbol to find what you need.",
            "inputSchema": {
                "type":"object",
                "properties":{
                    "path":{"type":"string", "description": "Path to the file (relative or absolute)"},
                    "cursor":{"type":"string", "description": "Pagination cursor from previous response"},
                    "limit":{"type":"integer", "description": "Max number of results to return (default: 200, max: 1000)"}
                },
                "required":["path"],
                "additionalProperties":false
            }
        }),
        json!({
            "name": "search_symbols",
            "title": "Search Symbols",
            "description": "Find symbols (functions, classes, etc.) by name pattern. Supports glob (*, ?) and regex (/pattern/). Use when you know part of a symbol name but not its file.",
            "inputSchema": {
                "type":"object",
                "properties":{
                    "query":{
                        "type":"string",
                        "description": "Symbol name, glob pattern (link*, *Handler), or regex (/^get.*Async$/)"
                    },
                    "kind":{
                        "type":"string",
                        "description": "Filter by symbol kind (function, class, method, variable, interface, type, etc.)"
                    },
                    "file_pattern":{
                        "type":"string",
                        "description": "Glob pattern for file paths (e.g., *.ts, src/api/**)"
                    },
                    "cursor":{"type":"string", "description": "Pagination cursor from previous response"},
                    "limit":{"type":"integer", "description": "Max number of results to return (default: 200, max: 1000)"}
                },
                "required":["query"],
                "additionalProperties":false
            }
        }),
        json!({
            "name": "find_definition",
            "title": "Find Definition",
            "description": "Find where a symbol is defined (exact name match). Use when you see a function/class used and need its implementation.",
            "inputSchema": {
                "type":"object",
                "properties":{
                    "name":{"type":"string", "description": "Exact symbol name to find (case-sensitive)"},
                    "cursor":{"type":"string", "description": "Pagination cursor from previous response"},
                    "limit":{"type":"integer", "description": "Max number of results to return (default: 200, max: 1000)"}
                },
                "required":["name"],
                "additionalProperties":false
            }
        }),
        json!({
            "name": "find_references",
            "title": "Find References",
            "description": "Find all usages of a symbol across the codebase. Use to understand impact before refactoring.",
            "inputSchema": {
                "type":"object",
                "properties":{
                    "name":{"type":"string", "description": "Symbol name to search for (word boundary match)"},
                    "cursor":{"type":"string", "description": "Pagination cursor from previous response"},
                    "limit":{"type":"integer", "description": "Max number of results to return (default: 200, max: 1000)"}
                },
                "required":["name"],
                "additionalProperties":false
            }
        }),
        json!({
            "name": "search_text",
            "title": "Search Text",
            "description": "Search for text/code patterns in files. Supports regex (/pattern/) and file filters. Use when searching for strings, comments, or code that isn't a symbol name.",
            "inputSchema": {
                "type":"object",
                "properties":{
                    "query":{
                        "type":"string",
                        "description": "Search pattern: simple text, multiple terms (space-separated), or regex (/pattern/)"
                    },
                    "file_pattern":{
                        "type":"string",
                        "description": "Glob pattern for files (e.g., *.ts, src/**/*.js)"
                    },
                    "match_mode":{
                        "type":"string",
                        "enum": ["all", "any"],
                        "description": "For multi-term: 'all' (AND, default) or 'any' (OR)"
                    },
                    "include_lines":{
                        "type":"boolean",
                        "description": "Include matching line numbers and content (default: false)"
                    },
                    "context_lines":{
                        "type":"integer",
                        "description": "Lines of context around matches (requires include_lines=true, default: 0)"
                    },
                    "cursor":{"type":"string", "description": "Pagination cursor from previous response"},
                    "limit":{"type":"integer", "description": "Max number of results to return (default: 200, max: 1000)"}
                },
                "required":["query"],
                "additionalProperties":false
            }
        }),
        json!({
            "name": "resource_graph",
            "title": "Resource Graph",
            "description": "Show import relationships between files. Use to understand dependencies before moving/deleting code.",
            "inputSchema": {
                "type":"object",
                "properties":{
                    "path":{"type":"string", "description": "Optional: center graph on this file"},
                    "depth":{"type":"integer", "description": "How many levels deep (default: 1)"},
                    "direction":{"type":"string", "enum": ["imports", "importers", "both"], "description": "Direction: imports (dependencies), importers (dependents), or both"}
                },
                "additionalProperties":false
            }
        }),
        json!({
            "name": "find_unused",
            "title": "Find Unused",
            "description": "Find dead code: files not imported anywhere and exported symbols never used. Use for cleanup tasks.",
            "inputSchema": {
                "type":"object",
                "properties":{
                    "type":{"type":"string", "enum": ["files", "exports", "all"], "description": "What to find: unused files, unused exports, or both (default: all)"}
                },
                "additionalProperties":false
            }
        }),
        json!({
            "name": "read_symbol",
            "title": "Read Symbol",
            "description": "Read a function/class with semantic summary: signature, calls, awaits, throws, variables. 10x fewer tokens than reading full file. Set include_code=true only when you need to edit.",
            "inputSchema": {
                "type":"object",
                "properties":{
                    "path":{"type":"string", "description": "Path to the file containing the symbol"},
                    "name":{"type":"string", "description": "Name of the symbol to read"},
                    "include_code":{"type":"boolean", "description": "Include the full source code (default: false, returns only semantic summary)"},
                    "context_lines":{"type":"integer", "description": "Lines of context before/after the symbol when include_code=true (default: 0)"}
                },
                "required":["path", "name"],
                "additionalProperties":false
            }
        }),
    ]
}

// ============================================================================
// Tool dispatcher
// ============================================================================

pub fn handle_tool_call(index: &IndexState, params: &Value) -> Result<ToolCallResponse> {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if name.is_empty() {
        return Err(anyhow!("missing tool name"));
    }
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = match name {
        "list_files" => tool_list_files(index, &args),
        "file_outline" => tool_file_outline(index, &args),
        "search_symbols" => tool_search_symbols(index, &args),
        "find_definition" => tool_find_definition(index, &args),
        "find_references" => tool_find_references(index, &args),
        "search_text" => tool_search_text(index, &args),
        "resource_graph" => tool_resource_graph(index, &args),
        "find_unused" => tool_find_unused(index, &args),
        "read_symbol" => tool_read_symbol(index, &args),
        _ => Err(anyhow!("unknown tool: {name}")),
    };

    match result {
        Ok(val) => Ok(ToolCallResponse {
            structured: val,
            is_error: false,
        }),
        Err(err) => Ok(ToolCallResponse {
            structured: json!({ "error": err.to_string() }),
            is_error: true,
        }),
    }
}

pub fn wrap_tool_result(result: Value, is_error: bool) -> Value {
    let text = serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string());
    json!({
        "content": [
            { "type": "text", "text": text }
        ],
        "structuredContent": result,
        "isError": is_error
    })
}

// ============================================================================
// Tool implementations
// ============================================================================

fn tool_list_files(index: &IndexState, args: &Value) -> Result<Value> {
    let (cursor, limit) = parse_pagination(args)?;
    let mut files: Vec<String> = index
        .files
        .read()
        .keys()
        .map(|p| format_path(&index.root, p, index.path_format))
        .collect();
    files.sort();
    let (start, end, next) = paginate_range(files.len(), cursor, limit);
    let slice = files[start..end].to_vec();
    let result = json!({"files": slice});
    Ok(attach_next_cursor(result, next))
}

fn tool_file_outline(index: &IndexState, args: &Value) -> Result<Value> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing path"))?;
    let resolved = resolve_path(&index.root, path);
    if !index.files.read().contains_key(&resolved) && resolved.exists() {
        let _ = index_file(index, &resolved);
    }
    let rec = index.files.read().get(&resolved).cloned();
    let (cursor, limit) = parse_pagination(args)?;
    let mut outline = rec.map(|r| r.outline).unwrap_or_default();
    outline.sort_by(|a, b| {
        (a.line, a.column, a.name.as_str(), a.kind.as_str()).cmp(&(
            b.line,
            b.column,
            b.name.as_str(),
            b.kind.as_str(),
        ))
    });
    let (start, end, next) = paginate_range(outline.len(), cursor, limit);
    let slice = outline[start..end].to_vec();

    let result = json!({
        "path": format_path(&index.root, &resolved, index.path_format),
        "outline": slice
    });
    Ok(attach_next_cursor(result, next))
}

fn tool_search_symbols(index: &IndexState, args: &Value) -> Result<Value> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing query"))?;

    // Optional filters
    let kind_filter = args.get("kind").and_then(|v| v.as_str());
    let file_pattern = args
        .get("file_pattern")
        .and_then(|v| v.as_str())
        .map(|p| sanitize_file_pattern(&index.root, p))
        .map(|p| Pattern::new(&p))
        .transpose()
        .map_err(|e| anyhow!("invalid file_pattern: {}", e))?;

    let (cursor, limit) = parse_pagination(args)?;
    let matcher = SymbolMatcher::from_query(query)?;

    let mut hits = Vec::new();
    for (path, rec) in index.files.read().iter() {
        // Filter by file pattern
        if let Some(ref fp) = file_pattern {
            let path_str = path.to_string_lossy();
            let file_name = path
                .file_name()
                .map(|f| f.to_string_lossy())
                .unwrap_or_default();
            if !fp.matches(&path_str) && !fp.matches(&file_name) {
                continue;
            }
        }

        for sym in &rec.symbols {
            // Filter by kind
            if let Some(k) = kind_filter {
                if !sym.kind.eq_ignore_ascii_case(k) {
                    continue;
                }
            }

            // Match the name
            if matcher.matches(&sym.name) {
                hits.push(sym.clone());
            }
        }
    }

    hits.sort_by(|a, b| {
        (
            a.name.as_str(),
            a.file.as_str(),
            a.line,
            a.column,
            a.kind.as_str(),
        )
            .cmp(&(
                b.name.as_str(),
                b.file.as_str(),
                b.line,
                b.column,
                b.kind.as_str(),
            ))
    });
    let (start, end, next) = paginate_range(hits.len(), cursor, limit);
    let symbols: Vec<Value> = hits[start..end]
        .iter()
        .map(|sym| symbol_to_value(&index.root, sym, index.path_format))
        .collect();

    let result = json!({"query": query, "symbols": symbols});
    Ok(attach_next_cursor(result, next))
}

fn tool_find_definition(index: &IndexState, args: &Value) -> Result<Value> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing name"))?;
    let (cursor, limit) = parse_pagination(args)?;
    let mut hits = Vec::new();
    for rec in index.files.read().values() {
        for sym in &rec.symbols {
            if sym.name == name {
                hits.push(sym.clone());
            }
        }
    }
    hits.sort_by(|a, b| {
        (
            a.file.as_str(),
            a.line,
            a.column,
            a.kind.as_str(),
            a.name.as_str(),
        )
            .cmp(&(
                b.file.as_str(),
                b.line,
                b.column,
                b.kind.as_str(),
                b.name.as_str(),
            ))
    });
    let (start, end, next) = paginate_range(hits.len(), cursor, limit);
    let definitions: Vec<Value> = hits[start..end]
        .iter()
        .map(|sym| symbol_to_value(&index.root, sym, index.path_format))
        .collect();

    let result = json!({"name": name, "definitions": definitions});
    Ok(attach_next_cursor(result, next))
}

fn tool_find_references(index: &IndexState, args: &Value) -> Result<Value> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing name"))?;
    let (cursor, limit) = parse_pagination(args)?;
    let pattern = Regex::new(&format!("\\b{}\\b", regex::escape(name)))?;

    let mut results = Vec::new();
    let mut seen = 0usize;
    let mut has_more = false;
    let mut files: Vec<PathBuf> = index.files.read().keys().cloned().collect();
    files.sort();
    for path in files {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for (i, line) in text.lines().enumerate() {
            if pattern.is_match(line) {
                if seen < cursor {
                    seen += 1;
                    continue;
                }
                if results.len() < limit {
                    results.push(json!({
                        "file": format_path(&index.root, &path, index.path_format),
                        "line": i + 1,
                        "text": line
                    }));
                } else {
                    has_more = true;
                    break;
                }
                seen += 1;
            }
        }
        if has_more {
            break;
        }
    }

    let next = if has_more {
        Some((cursor + limit).to_string())
    } else {
        None
    };
    let result = json!({"name": name, "references": results});
    Ok(attach_next_cursor(result, next))
}

fn tool_search_text(index: &IndexState, args: &Value) -> Result<Value> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing query"))?;

    let file_pattern = args
        .get("file_pattern")
        .and_then(|v| v.as_str())
        .map(|p| sanitize_file_pattern(&index.root, p))
        .map(|p| Pattern::new(&p))
        .transpose()
        .map_err(|e| anyhow!("invalid file_pattern: {}", e))?;

    let match_all = match args.get("match_mode").and_then(|v| v.as_str()) {
        Some("any") => false,
        _ => true, // default is "all"
    };

    let include_lines = args
        .get("include_lines")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let context_lines = args
        .get("context_lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let (cursor, limit) = parse_pagination(args)?;
    let matcher = TextMatcher::from_query(query, match_all)?;

    if matcher.is_empty() {
        return Ok(json!({"query": query, "results": [], "total": 0}));
    }

    let mut results: Vec<Value> = Vec::new();
    let mut files: Vec<PathBuf> = index.files.read().keys().cloned().collect();
    files.sort();

    for path in files {
        // File pattern filter
        if let Some(ref fp) = file_pattern {
            let path_str = path.to_string_lossy();
            let file_name = path
                .file_name()
                .map(|f| f.to_string_lossy())
                .unwrap_or_default();
            if !fp.matches(&path_str) && !fp.matches(&file_name) {
                continue;
            }
        }

        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };

        if !matcher.matches_file(&text) {
            continue;
        }

        let file_str = format_path(&index.root, &path, index.path_format);

        if include_lines {
            // Return matching lines with context
            let lines: Vec<&str> = text.lines().collect();
            let mut matches: Vec<Value> = Vec::new();

            for (i, line) in lines.iter().enumerate() {
                if matcher.matches_line(line) {
                    let mut m = json!({
                        "line": i + 1,
                        "content": line.to_string()
                    });

                    if context_lines > 0 {
                        let ctx_before: Vec<String> = lines
                            [i.saturating_sub(context_lines)..i]
                            .iter()
                            .map(|s| s.to_string())
                            .collect();
                        let ctx_after: Vec<String> = lines
                            [(i + 1).min(lines.len())..(i + 1 + context_lines).min(lines.len())]
                            .iter()
                            .map(|s| s.to_string())
                            .collect();
                        m["context_before"] = json!(ctx_before);
                        m["context_after"] = json!(ctx_after);
                    }
                    matches.push(m);
                }
            }

            if !matches.is_empty() {
                results.push(json!({
                    "file": file_str,
                    "matches": matches
                }));
            }
        } else {
            // Just return file names
            results.push(json!({"file": file_str}));
        }
    }

    let total = results.len();
    let (start, end, next) = paginate_range(results.len(), cursor, limit);
    let slice = results[start..end].to_vec();

    let result = json!({
        "query": query,
        "results": slice,
        "total": total
    });
    Ok(attach_next_cursor(result, next))
}

fn tool_resource_graph(index: &IndexState, args: &Value) -> Result<Value> {
    let center_path = args.get("path").and_then(|v| v.as_str());
    let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
    let direction = args
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("both");

    let files = index.files.read();

    // Build reverse index: file -> list of files that import it
    let mut importers_map: HashMap<String, Vec<String>> = HashMap::new();
    for (path, record) in files.iter() {
        let file_str = format_path(&index.root, path, index.path_format);
        for import in &record.imports {
            let resolved = resolve_import(&index.root, path, import);
            if let Some(resolved_str) = resolved {
                importers_map
                    .entry(resolved_str)
                    .or_default()
                    .push(file_str.clone());
            }
        }
    }

    let mut nodes: HashSet<String> = HashSet::new();
    let mut edges: Vec<Value> = Vec::new();

    if let Some(center) = center_path {
        // Graph centered on a specific file
        let resolved_center = resolve_path(&index.root, center);
        let center_str = format_path(&index.root, &resolved_center, index.path_format);
        nodes.insert(center_str.clone());

        let mut to_visit: Vec<(String, usize, bool)> = Vec::new(); // (path, current_depth, is_import_direction)

        if direction == "imports" || direction == "both" {
            to_visit.push((center_str.clone(), 0, true));
        }
        if direction == "importers" || direction == "both" {
            to_visit.push((center_str.clone(), 0, false));
        }

        let mut visited_imports: HashSet<String> = HashSet::new();
        let mut visited_importers: HashSet<String> = HashSet::new();

        while let Some((current, current_depth, is_import)) = to_visit.pop() {
            if current_depth >= depth {
                continue;
            }

            if is_import {
                if visited_imports.contains(&current) {
                    continue;
                }
                visited_imports.insert(current.clone());

                // Find what this file imports
                let current_path = resolve_path(&index.root, &current);
                if let Some(record) = files.get(&current_path) {
                    for import in &record.imports {
                        let resolved = resolve_import(&index.root, &current_path, import);
                        if let Some(resolved_str) = resolved {
                            nodes.insert(resolved_str.clone());
                            edges.push(json!({
                                "from": current,
                                "to": resolved_str,
                                "type": "imports"
                            }));
                            to_visit.push((resolved_str, current_depth + 1, true));
                        }
                    }
                }
            } else {
                if visited_importers.contains(&current) {
                    continue;
                }
                visited_importers.insert(current.clone());

                // Find what imports this file
                if let Some(importers) = importers_map.get(&current) {
                    for importer in importers {
                        nodes.insert(importer.clone());
                        edges.push(json!({
                            "from": importer,
                            "to": current,
                            "type": "imports"
                        }));
                        to_visit.push((importer.clone(), current_depth + 1, false));
                    }
                }
            }
        }

        // Dedupe edges
        let mut seen_edges: HashSet<String> = HashSet::new();
        edges.retain(|e| {
            let key = format!(
                "{}->{}",
                e.get("from").and_then(|v| v.as_str()).unwrap_or(""),
                e.get("to").and_then(|v| v.as_str()).unwrap_or("")
            );
            seen_edges.insert(key)
        });

        let mut nodes_vec: Vec<String> = nodes.into_iter().collect();
        nodes_vec.sort();

        Ok(json!({
            "center": center_str,
            "nodes": nodes_vec,
            "edges": edges
        }))
    } else {
        // Global graph - all files and their imports
        for (path, record) in files.iter() {
            let file_str = format_path(&index.root, path, index.path_format);
            nodes.insert(file_str.clone());

            for import in &record.imports {
                let resolved = resolve_import(&index.root, path, import);
                if let Some(resolved_str) = resolved {
                    nodes.insert(resolved_str.clone());
                    edges.push(json!({
                        "from": file_str,
                        "to": resolved_str,
                        "type": "imports"
                    }));
                }
            }
        }

        let mut nodes_vec: Vec<String> = nodes.into_iter().collect();
        nodes_vec.sort();

        Ok(json!({
            "nodes": nodes_vec,
            "edges": edges
        }))
    }
}

fn tool_find_unused(index: &IndexState, args: &Value) -> Result<Value> {
    let find_type = args.get("type").and_then(|v| v.as_str()).unwrap_or("all");

    let files = index.files.read();

    // Build set of all imported files (relative paths)
    let mut imported_files: HashSet<String> = HashSet::new();

    // Build map of what symbols are imported from each file
    // file -> set of imported symbol names
    let mut imported_symbols: HashMap<String, HashSet<String>> = HashMap::new();

    for (path, record) in files.iter() {
        for import in &record.imports {
            let resolved = resolve_import(&index.root, path, import);
            if let Some(resolved_str) = resolved {
                imported_files.insert(resolved_str.clone());

                // Parse what's imported from this file
                // We need to re-read the source to find the import specifiers
                let text = match std::fs::read_to_string(path) {
                    Ok(t) => t,
                    Err(_) => continue,
                };

                let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
                if let Ok(specifiers) = extract_import_specifiers(&text, import, ext) {
                    imported_symbols
                        .entry(resolved_str)
                        .or_default()
                        .extend(specifiers);
                }
            }
        }
    }

    let mut unused_files: Vec<String> = Vec::new();
    let mut unused_exports: Vec<Value> = Vec::new();

    // Find unused files and exports
    for (path, record) in files.iter() {
        let file_str = format_path(&index.root, path, index.path_format);

        // Check if file is imported anywhere
        if find_type == "files" || find_type == "all" {
            if !imported_files.contains(&file_str) {
                // Check if it's likely an entry point (index.*, main.*, app.*, etc.)
                let filename = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let is_entry_point = matches!(
                    filename.to_lowercase().as_str(),
                    "index" | "main" | "app" | "server" | "entry" | "bootstrap"
                );

                if !is_entry_point {
                    unused_files.push(file_str.clone());
                }
            }
        }

        // Check for unused exports
        if find_type == "exports" || find_type == "all" {
            let file_imported_symbols = imported_symbols.get(&file_str);

            for export_name in &record.exports {
                let is_used = file_imported_symbols
                    .map(|s| s.contains(export_name) || s.contains("*"))
                    .unwrap_or(false);

                if !is_used {
                    unused_exports.push(json!({
                        "file": file_str,
                        "name": export_name
                    }));
                }
            }
        }
    }

    unused_files.sort();
    unused_exports.sort_by(|a, b| {
        let a_file = a.get("file").and_then(|v| v.as_str()).unwrap_or("");
        let b_file = b.get("file").and_then(|v| v.as_str()).unwrap_or("");
        let a_name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let b_name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
        (a_file, a_name).cmp(&(b_file, b_name))
    });

    Ok(json!({
        "unusedFiles": unused_files,
        "unusedExports": unused_exports
    }))
}

fn tool_read_symbol(index: &IndexState, args: &Value) -> Result<Value> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing path"))?;
    let symbol_name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing name"))?;
    let include_code = args
        .get("include_code")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let context_lines = args
        .get("context_lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let resolved = resolve_path(&index.root, path);

    // Ensure file is indexed
    if !index.files.read().contains_key(&resolved) && resolved.exists() {
        let _ = index_file(index, &resolved);
    }

    let files = index.files.read();
    let record = files
        .get(&resolved)
        .ok_or_else(|| anyhow!("file not found: {}", path))?;

    // Find the symbol in the outline
    let symbol = record
        .outline
        .iter()
        .find(|item| item.name == symbol_name)
        .ok_or_else(|| anyhow!("symbol '{}' not found in {}", symbol_name, path))?;

    let mut result = json!({
        "name": symbol.name,
        "kind": symbol.kind,
        "file": format_path(&index.root, &resolved, index.path_format),
        "lines": [symbol.line, symbol.end_line],
        "loc": symbol.end_line.saturating_sub(symbol.line) + 1
    });

    // Add detail if available
    if let Some(ref detail) = symbol.detail {
        if let Some(ref sig) = detail.signature {
            result["signature"] = json!(sig);
        }
        result["exported"] = json!(detail.exported);
        if !detail.calls.is_empty() {
            result["calls"] = json!(detail.calls);
        }
        if !detail.awaits.is_empty() {
            result["awaits"] = json!(detail.awaits);
        }
        if !detail.throws.is_empty() {
            result["throws"] = json!(detail.throws);
        }
        if !detail.writes.is_empty() {
            result["writes"] = json!(detail.writes);
        }
    }

    // Include source code if requested
    if include_code {
        let text = std::fs::read_to_string(&resolved).context("failed to read file")?;
        let lines: Vec<&str> = text.lines().collect();

        let start = symbol.line.saturating_sub(1).saturating_sub(context_lines);
        let end = (symbol.end_line + context_lines).min(lines.len());

        let code: String = lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{}|{}", start + i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        result["code"] = json!(code);
        result["code_lines"] = json!([start + 1, end]);
    }

    Ok(result)
}
