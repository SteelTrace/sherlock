//! Core data types for Sherlock indexing.

use serde::{Deserialize, Serialize};

/// A symbol (function, class, etc.) found in the codebase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
}

/// An item in a file's outline (structure view).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutlineItem {
    pub name: String,
    pub kind: String,
    pub line: usize,
    pub column: usize,
    #[serde(default)]
    pub end_line: usize,
    #[serde(default)]
    pub detail: Option<SymbolDetail>,
}

/// Detailed semantic information about a symbol (for read_symbol tool).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SymbolDetail {
    /// Function signature (params + return type)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Whether the symbol is exported
    #[serde(default)]
    pub exported: bool,
    /// Functions/methods called in the body
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<String>,
    /// Async calls (awaited)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub awaits: Vec<String>,
    /// Errors thrown
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub throws: Vec<String>,
    /// Variables/properties read
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<String>,
    /// Variables/properties written
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<String>,
    /// Lines of code in body
    #[serde(default)]
    pub loc: usize,
}

/// A record stored in the database for each indexed file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub hash: String,
    pub symbols: Vec<Symbol>,
    pub outline: Vec<OutlineItem>,
    #[serde(default)]
    pub imports: Vec<String>,
    #[serde(default)]
    pub exports: Vec<String>,
}

/// Response from a tool call.
#[derive(Debug)]
pub struct ToolCallResponse {
    pub structured: serde_json::Value,
    pub is_error: bool,
}
