//! MCP server implementation (stdio and socket).

use anyhow::{anyhow, Context, Result};
use parking_lot::RwLock;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};

use crate::config::{Config, RunMode};
use crate::index::{initial_index, load_db, start_watcher, IndexState};
use crate::tools::{handle_tool_call, tool_schemas, wrap_tool_result};

/// Supported MCP protocol versions.
const SUPPORTED_PROTOCOLS: &[&str] = &[
    "2025-03-26",
    "2024-11-05",
    "2025-11-25",
    "2025-06-18",
    "1.0",
    "0.1",
];

/// Run the application in the appropriate mode.
pub fn run(config: &Config, mode: RunMode) -> Result<()> {
    match mode {
        RunMode::Client => run_as_client(config),
        RunMode::Server => run_as_server(config),
    }
}

/// Run as the main server (owns DB and file watcher).
fn run_as_server(config: &Config) -> Result<()> {
    eprintln!("Running as SERVER (owns DB and file watcher)");

    eprintln!("Opening DB at: {}", config.db_path.display());
    let db = match sled::open(&config.db_path) {
        Ok(db) => {
            eprintln!("DB opened successfully");
            db
        }
        Err(e) => {
            eprintln!("Failed to open DB: {:?}", e);
            return Err(anyhow!("open sled db: {}", e));
        }
    };
    let index = IndexState {
        root: config.root.clone(),
        db,
        files: Arc::new(RwLock::new(HashMap::new())),
        path_format: config.path_format,
    };

    // Load existing index from disk (fast)
    match load_db(&index) {
        Ok(errors) if errors > 0 => {
            eprintln!("DB corrupted ({} errors), clearing and rebuilding...", errors);
            index.db.clear()?;
            index.db.flush()?;
            index.files.write().clear();
        }
        Ok(_) => {
            eprintln!("DB loaded successfully");
        }
        Err(e) => {
            eprintln!("Failed to load DB: {:?}, clearing and rebuilding...", e);
            index.db.clear()?;
            index.db.flush()?;
            index.files.write().clear();
        }
    }

    // Start indexing in background so we can respond to MCP requests immediately
    let index_for_bg = index.clone();
    thread::spawn(move || {
        if let Err(err) = initial_index(&index_for_bg) {
            eprintln!("Background indexing error: {err}");
        }
        if let Err(err) = start_watcher(index_for_bg) {
            eprintln!("Watcher start error: {err}");
        }
    });

    // Start the Unix socket listener for other agents
    #[cfg(unix)]
    {
        eprintln!("Starting socket server...");
        if let Err(e) = start_socket_server(config.socket_path.clone(), index.clone()) {
            eprintln!("Failed to start socket server: {:?}", e);
            return Err(e);
        }
        eprintln!("Socket server started");
    }

    // Handle MCP requests from stdin (this agent's own MCP connection)
    // This starts immediately, even while indexing is in progress
    run_stdio_server(index)?;

    // Cleanup socket on exit
    #[cfg(unix)]
    let _ = std::fs::remove_file(&config.socket_path);

    Ok(())
}

/// Run as a client (proxy to existing server).
#[cfg(unix)]
fn run_as_client(config: &Config) -> Result<()> {
    eprintln!(
        "Running as CLIENT (proxying to server at {})",
        config.socket_path.display()
    );

    let stream =
        UnixStream::connect(&config.socket_path).context("Failed to connect to server socket")?;

    run_proxy_client(stream)
}

#[cfg(not(unix))]
fn run_as_client(_config: &Config) -> Result<()> {
    Err(anyhow!("Client mode not supported on this platform"))
}

/// Run the stdio-based MCP server.
fn run_stdio_server(index: IndexState) -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    eprintln!("Stdio server ready, waiting for input...");

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                eprintln!("Stdin closed (EOF)");
                break;
            }
            Err(e) => {
                eprintln!("Stdin read error: {} (kind: {:?})", e, e.kind());
                return Err(e.into());
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        eprintln!("Received: {}", &line[..line.len().min(100)]);
        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(err) => {
                eprintln!("invalid json: {err}");
                continue;
            }
        };

        let response = handle_mcp_request(&index, &request);

        if let Some(resp) = response {
            writeln!(stdout, "{}", resp.to_string())?;
            stdout.flush()?;
        }
    }

    eprintln!("Stdio server shutting down");
    Ok(())
}

/// Handle a single MCP request and return the response (if any).
fn handle_mcp_request(index: &IndexState, request: &Value) -> Option<Value> {
    let method = request.get("method").and_then(|v| v.as_str());
    let params = request.get("params").cloned().unwrap_or(json!({}));
    let id = request.get("id").cloned();

    let method = match method {
        Some(m) if !m.is_empty() => m,
        _ => return None,
    };

    let has_id = id.is_some() && !id.as_ref().unwrap().is_null();

    let result = match method {
        "initialize" => handle_initialize(&params),
        "tools/list" => Ok(json!({ "tools": tool_schemas() })),
        "tools/call" => match handle_tool_call(index, &params) {
            Ok(tool_resp) => Ok(wrap_tool_result(tool_resp.structured, tool_resp.is_error)),
            Err(e) => Err(e),
        },
        _ => Err(anyhow!("unknown method: {method}")),
    };

    if has_id {
        let response = match result {
            Ok(val) => json!({"jsonrpc":"2.0", "id": id.unwrap(), "result": val}),
            Err(err) => {
                let code = if method == "tools/call" { -32602 } else { -32601 };
                json!({"jsonrpc":"2.0", "id": id.unwrap(), "error": {"code": code, "message": err.to_string()}})
            }
        };
        Some(response)
    } else {
        None
    }
}

/// Handle the initialize request.
fn handle_initialize(params: &Value) -> Result<Value> {
    let requested = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let protocol_version = if SUPPORTED_PROTOCOLS.contains(&requested) {
        requested
    } else {
        SUPPORTED_PROTOCOLS[0]
    };

    Ok(json!({
        "protocolVersion": protocol_version,
        "serverInfo": {"name": "sherlock", "version": "0.4.0"},
        "capabilities": {"tools": {"listChanged": false}}
    }))
}

/// Start a Unix socket server that accepts connections from other agents.
#[cfg(unix)]
fn start_socket_server(socket_path: PathBuf, index: IndexState) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Remove stale socket if exists
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path).context("Failed to bind Unix socket")?;

    eprintln!("Socket server listening at: {}", socket_path.display());

    // Spawn a thread to accept connections
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let index_clone = index.clone();
                    thread::spawn(move || {
                        if let Err(e) = handle_socket_client(stream, index_clone) {
                            eprintln!("Socket client error: {e}");
                        }
                    });
                }
                Err(e) => {
                    eprintln!("Socket accept error: {e}");
                }
            }
        }
    });

    Ok(())
}

/// Handle a single client connection over Unix socket.
#[cfg(unix)]
fn handle_socket_client(stream: UnixStream, index: IndexState) -> Result<()> {
    eprintln!("New socket client connected");

    let reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(err) => {
                eprintln!("Socket client invalid json: {err}");
                continue;
            }
        };

        if let Some(response) = handle_mcp_request(&index, &request) {
            writeln!(writer, "{}", response.to_string())?;
            writer.flush()?;
        }
    }

    eprintln!("Socket client disconnected");
    Ok(())
}

/// Run as a client: proxy MCP requests from stdin to the server socket.
#[cfg(unix)]
fn run_proxy_client(stream: UnixStream) -> Result<()> {
    let reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    // Spawn a thread to read responses from server and write to stdout
    let stdout_handle = thread::spawn(move || {
        let mut stdout = io::stdout();
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    if let Err(e) = writeln!(stdout, "{}", line) {
                        eprintln!("Failed to write to stdout: {e}");
                        break;
                    }
                    if let Err(e) = stdout.flush() {
                        eprintln!("Failed to flush stdout: {e}");
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("Server disconnected: {e}");
                    break;
                }
            }
        }
    });

    // Read from stdin and forward to server
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        writeln!(writer, "{}", line)?;
        writer.flush()?;
    }

    // Wait for response thread to finish
    let _ = stdout_handle.join();

    Ok(())
}
