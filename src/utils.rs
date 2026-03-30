//! Utility functions for pagination, path handling, and matching.

use anyhow::{anyhow, Result};
use glob::Pattern;
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::config::PathFormat;
use crate::types::Symbol;

// ============================================================================
// Pagination
// ============================================================================

pub const DEFAULT_LIMIT: usize = 200;
pub const MAX_LIMIT: usize = 1000;

/// Parse pagination parameters from tool arguments.
pub fn parse_pagination(args: &Value) -> Result<(usize, usize)> {
    let cursor = match args.get("cursor") {
        None | Some(Value::Null) => 0,
        Some(Value::String(s)) if s.is_empty() => 0,
        Some(Value::String(s)) => s.parse::<usize>().map_err(|_| anyhow!("invalid cursor"))?,
        Some(Value::Number(n)) => n.as_u64().ok_or_else(|| anyhow!("invalid cursor"))? as usize,
        _ => return Err(anyhow!("invalid cursor")),
    };

    let mut limit = match args.get("limit") {
        None | Some(Value::Null) => DEFAULT_LIMIT,
        Some(Value::Number(n)) => n.as_u64().ok_or_else(|| anyhow!("invalid limit"))? as usize,
        Some(Value::String(s)) => s.parse::<usize>().map_err(|_| anyhow!("invalid limit"))?,
        _ => return Err(anyhow!("invalid limit")),
    };

    if limit == 0 {
        return Err(anyhow!("limit must be greater than 0"));
    }
    if limit > MAX_LIMIT {
        limit = MAX_LIMIT;
    }

    Ok((cursor, limit))
}

/// Calculate pagination range and next cursor.
pub fn paginate_range(total: usize, cursor: usize, limit: usize) -> (usize, usize, Option<String>) {
    if total == 0 {
        return (0, 0, None);
    }
    let start = cursor.min(total);
    let end = (start + limit).min(total);
    let next = if end < total {
        Some(end.to_string())
    } else {
        None
    };
    (start, end, next)
}

/// Attach next cursor to a JSON value if present.
pub fn attach_next_cursor(mut value: Value, next: Option<String>) -> Value {
    if let Some(cursor) = next {
        if let Value::Object(map) = &mut value {
            map.insert("nextCursor".to_string(), Value::String(cursor));
        }
    }
    value
}

// ============================================================================
// Path handling
// ============================================================================

/// Resolve a path relative to root.
pub fn resolve_path(root: &Path, input: &str) -> PathBuf {
    let p = PathBuf::from(input);
    if p.is_absolute() {
        p
    } else {
        root.join(p)
    }
}

/// Format a path according to the configured format.
pub fn format_path(root: &Path, path: &Path, format: PathFormat) -> String {
    match format {
        PathFormat::Absolute => path.to_string_lossy().to_string(),
        PathFormat::Relative => path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string(),
    }
}

/// Resolve an import path to an absolute path.
pub fn resolve_import(root: &Path, from_file: &Path, import_path: &str) -> Option<String> {
    // Skip non-relative imports (node_modules, etc.)
    if !import_path.starts_with('.') {
        return None;
    }

    let from_dir = from_file.parent()?;
    let mut resolved = from_dir.join(import_path);

    // Try common extensions if no extension specified
    let extensions = [
        "",
        ".ts",
        ".tsx",
        ".js",
        ".jsx",
        ".vue",
        "/index.ts",
        "/index.tsx",
        "/index.js",
        "/index.jsx",
    ];

    for ext in extensions {
        let candidate = if ext.is_empty() {
            resolved.clone()
        } else if ext.starts_with('/') {
            resolved.join(&ext[1..])
        } else {
            PathBuf::from(format!("{}{}", resolved.display(), ext))
        };

        if candidate.exists() {
            resolved = candidate;
            break;
        }
    }

    // Normalize and return relative path
    if let Ok(canonical) = resolved.canonicalize() {
        // Also canonicalize root for proper comparison (handles /tmp vs /private/tmp on macOS)
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        if let Ok(rel) = canonical.strip_prefix(&canonical_root) {
            return Some(rel.to_string_lossy().to_string());
        }
    }

    None
}

/// Convert a Symbol to a JSON value.
pub fn symbol_to_value(root: &Path, sym: &Symbol, format: PathFormat) -> Value {
    json!({
        "name": sym.name.as_str(),
        "kind": sym.kind.as_str(),
        "file": format_path(root, Path::new(&sym.file), format),
        "line": sym.line,
        "column": sym.column
    })
}

// ============================================================================
// Matchers
// ============================================================================

/// Query matcher for symbol search - supports substring, glob, and regex.
pub enum SymbolMatcher {
    Substring(String),
    Glob(Pattern),
    Regex(Regex),
}

impl SymbolMatcher {
    pub fn from_query(query: &str) -> Result<Self> {
        // Regex: starts and ends with /
        if query.starts_with('/') && query.ends_with('/') && query.len() > 2 {
            let pattern = &query[1..query.len() - 1];
            let re = Regex::new(pattern).map_err(|e| anyhow!("invalid regex: {}", e))?;
            return Ok(SymbolMatcher::Regex(re));
        }

        // Glob: contains * or ?
        if query.contains('*') || query.contains('?') {
            let pattern =
                Pattern::new(query).map_err(|e| anyhow!("invalid glob pattern: {}", e))?;
            return Ok(SymbolMatcher::Glob(pattern));
        }

        // Default: substring case-insensitive
        Ok(SymbolMatcher::Substring(query.to_lowercase()))
    }

    pub fn matches(&self, name: &str) -> bool {
        match self {
            SymbolMatcher::Substring(q) => name.to_lowercase().contains(q),
            SymbolMatcher::Glob(p) => p.matches(name),
            SymbolMatcher::Regex(r) => r.is_match(name),
        }
    }
}

/// Text matcher for search_text - supports terms (AND/OR) and regex.
pub enum TextMatcher {
    Terms { words: Vec<String>, match_all: bool },
    Regex(Regex),
}

impl TextMatcher {
    pub fn from_query(query: &str, match_all: bool) -> Result<Self> {
        // Regex: starts and ends with /
        if query.starts_with('/') && query.ends_with('/') && query.len() > 2 {
            let pattern = &query[1..query.len() - 1];
            let re = Regex::new(pattern).map_err(|e| anyhow!("invalid regex: {}", e))?;
            return Ok(TextMatcher::Regex(re));
        }

        // Multi-term search
        let words: Vec<String> = query
            .split_whitespace()
            .map(|w| w.to_lowercase())
            .collect();
        Ok(TextMatcher::Terms { words, match_all })
    }

    pub fn matches_line(&self, line: &str) -> bool {
        match self {
            TextMatcher::Regex(re) => re.is_match(line),
            TextMatcher::Terms { words, match_all } => {
                let line_lower = line.to_lowercase();
                if *match_all {
                    words.iter().all(|w| line_lower.contains(w))
                } else {
                    words.iter().any(|w| line_lower.contains(w))
                }
            }
        }
    }

    pub fn matches_file(&self, text: &str) -> bool {
        match self {
            TextMatcher::Regex(re) => re.is_match(text),
            TextMatcher::Terms { words, match_all } => {
                let text_lower = text.to_lowercase();
                if *match_all {
                    words.iter().all(|w| text_lower.contains(w))
                } else {
                    words.iter().any(|w| text_lower.contains(w))
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            TextMatcher::Regex(_) => false,
            TextMatcher::Terms { words, .. } => words.is_empty(),
        }
    }
}
