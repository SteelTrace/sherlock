//! Configuration and argument parsing.

use anyhow::{Context, Result};
use blake3::Hasher;
use std::path::{Path, PathBuf};

/// How to format paths in output.
#[derive(Debug, Clone, Copy)]
pub enum PathFormat {
    Relative,
    Absolute,
}

/// Run mode for the server.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RunMode {
    /// We are the main server (own the DB, run watcher)
    Server,
    /// Connect to existing server
    Client,
}

/// Application configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    pub db_path: PathBuf,
    pub socket_path: PathBuf,
    pub path_format: PathFormat,
    pub force_server: bool,
}

/// Parse command-line arguments into Config.
pub fn parse_args() -> Result<Config> {
    let mut root = std::env::current_dir().context("current_dir")?;
    let mut db_path: Option<PathBuf> = None;
    let mut path_format = PathFormat::Relative;
    let mut force_server = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => {
                if let Some(val) = args.next() {
                    root = PathBuf::from(val);
                }
            }
            "--db" => {
                if let Some(val) = args.next() {
                    db_path = Some(PathBuf::from(val));
                }
            }
            "--paths" => {
                if let Some(val) = args.next() {
                    path_format = match val.as_str() {
                        "absolute" => PathFormat::Absolute,
                        "relative" => PathFormat::Relative,
                        _ => PathFormat::Relative,
                    };
                }
            }
            "--server" => {
                force_server = true;
            }
            _ => {}
        }
    }

    let (db_path, socket_path) = get_project_paths(&root, db_path);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    Ok(Config {
        root,
        db_path,
        socket_path,
        path_format,
        force_server,
    })
}

/// Get database and socket paths for a project.
fn get_project_paths(root: &Path, custom_db: Option<PathBuf>) -> (PathBuf, PathBuf) {
    let mut hasher = Hasher::new();
    hasher.update(root.to_string_lossy().as_bytes());
    let hash = hasher.finalize().to_hex();
    let short_hash = &hash[..12];

    let base = dirs::home_dir()
        .map(|h| h.join(".sherlock"))
        .unwrap_or_else(|| PathBuf::from("/tmp/.sherlock"));

    let project_dir = base.join(short_hash);

    let db_path = custom_db.unwrap_or_else(|| project_dir.join("index.sled"));
    let socket_path = project_dir.join("sherlock.sock");

    (db_path, socket_path)
}

#[cfg(unix)]
use std::os::unix::net::UnixStream;

/// Determine whether to run as server or client.
#[cfg(unix)]
pub fn determine_run_mode(config: &Config) -> RunMode {
    if config.force_server {
        return RunMode::Server;
    }

    // Try to connect to existing socket
    if config.socket_path.exists() {
        if let Ok(stream) = UnixStream::connect(&config.socket_path) {
            // Socket exists and is responsive
            drop(stream);
            return RunMode::Client;
        }
        // Socket file exists but server is dead - clean it up
        eprintln!(
            "Stale socket found, removing: {}",
            config.socket_path.display()
        );
        let _ = std::fs::remove_file(&config.socket_path);
    }

    RunMode::Server
}

#[cfg(not(unix))]
pub fn determine_run_mode(_config: &Config) -> RunMode {
    // On non-Unix systems, always run as server (no socket support)
    RunMode::Server
}
