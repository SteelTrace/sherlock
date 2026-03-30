//! Sherlock - A fast code indexing and search MCP server for multi-language codebases.
//! Supports: JS/TS/Vue, Python, Rust, Go, JSON, HTML, CSS, Markdown, YAML.
//!
//! # Architecture
//!
//! - `types`: Core data structures (Symbol, FileRecord, etc.)
//! - `config`: Configuration and argument parsing
//! - `parser`: Code parsing with tree-sitter
//! - `index`: File indexing and watching
//! - `tools`: MCP tool implementations
//! - `server`: MCP protocol handling (stdio/socket)
//! - `utils`: Shared utilities (pagination, paths, matchers)

mod config;
mod index;
mod parser;
mod server;
mod tools;
mod types;
mod utils;

use anyhow::Result;

fn main() {
    if let Err(err) = run() {
        let msg = err.to_string();
        eprintln!("{:#}", err);
        if msg.contains("Resource temporarily unavailable")
            || msg.to_lowercase().contains("lock")
            || msg.to_lowercase().contains("locked")
        {
            eprintln!("Hint: the index DB may be locked by another Sherlock instance.");
            eprintln!(
                "Try stopping other instances or delete the lock file in ~/.sherlock/<hash>/index.sled/lock."
            );
        }
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let config = config::parse_args()?;
    let mode = config::determine_run_mode(&config);

    eprintln!(
        "Sherlock MCP starting. mode={:?} root={} socket={} paths={:?}",
        mode,
        config.root.display(),
        config.socket_path.display(),
        config.path_format
    );

    // Log the cwd for debugging
    if let Ok(cwd) = std::env::current_dir() {
        eprintln!("Current working directory: {}", cwd.display());
    }

    server::run(&config, mode)
}
