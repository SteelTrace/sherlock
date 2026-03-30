//! Index state management and file indexing.

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::RwLock;
use sled::Db;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::thread;

use crate::config::PathFormat;
use crate::parser::{extract_symbols, hash_text};
use crate::types::FileRecord;

/// The main index state, shared across threads.
#[derive(Clone)]
pub struct IndexState {
    pub root: PathBuf,
    pub db: Db,
    pub files: Arc<RwLock<HashMap<PathBuf, FileRecord>>>,
    pub path_format: PathFormat,
}

/// Load existing index from the database.
/// Returns the number of corruption errors found (0 means healthy).
pub fn load_db(index: &IndexState) -> Result<usize> {
    let mut map = index.files.write();
    let mut count = 0usize;
    let mut errors = 0usize;
    for item in index.db.iter() {
        match item {
            Ok((k, v)) => {
                let path_str = String::from_utf8(k.to_vec()).unwrap_or_default();
                match bincode::deserialize(&v) {
                    Ok(rec) => {
                        map.insert(PathBuf::from(path_str), rec);
                        count += 1;
                    }
                    Err(_) => {
                        errors += 1;
                    }
                }
            }
            Err(_) => {
                errors += 1;
            }
        }
    }
    eprintln!("Loaded index from disk. files={} errors={}", count, errors);
    Ok(errors)
}

/// Perform initial indexing of the codebase.
pub fn initial_index(index: &IndexState) -> Result<()> {
    let walker = WalkBuilder::new(&index.root)
        .standard_filters(true)
        .hidden(false)
        .build();

    let mut count = 0usize;
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if !is_supported(path) {
            continue;
        }
        if let Err(err) = index_file(index, path) {
            eprintln!("index error {}: {err}", path.display());
        } else {
            count += 1;
        }
    }
    let total = index.files.read().len();
    eprintln!("Initial index complete. indexed={count} total={total}");
    Ok(())
}

/// Start the file watcher for live updates.
pub fn start_watcher(index: IndexState) -> Result<()> {
    let (tx, rx) = mpsc::channel::<Result<Event, notify::Error>>();

    let mut watcher: RecommendedWatcher = Watcher::new(tx, notify::Config::default())?;
    watcher.watch(&index.root, RecursiveMode::Recursive)?;

    thread::spawn(move || {
        // Keep watcher alive
        let _watcher = watcher;
        for res in rx {
            match res {
                Ok(event) => {
                    handle_event(&index, event);
                }
                Err(err) => {
                    eprintln!("watch error: {err}");
                }
            }
        }
    });

    Ok(())
}

/// Handle a file system event.
fn handle_event(index: &IndexState, event: Event) {
    for path in event.paths {
        if !is_supported(&path) {
            continue;
        }
        if event.kind.is_remove() {
            remove_file(index, &path);
            continue;
        }
        // Re-index on create/modify
        if let Err(err) = index_file(index, &path) {
            eprintln!("reindex error {}: {err}", path.display());
        }
    }
}

/// Remove a file from the index.
fn remove_file(index: &IndexState, path: &Path) {
    index.files.write().remove(path);
    let _ = index.db.remove(path.to_string_lossy().as_bytes());
}

/// Index a single file.
pub fn index_file(index: &IndexState, path: &Path) -> Result<()> {
    let text = std::fs::read_to_string(path).context("read file")?;
    let hash = hash_text(&text);

    if let Some(existing) = index.files.read().get(path) {
        if existing.hash == hash {
            return Ok(());
        }
    }

    let (symbols, outline, imports, exports) = match extract_symbols(path, &text) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("parse error {}: {err}", path.display());
            (Vec::new(), Vec::new(), Vec::new(), Vec::new())
        }
    };

    let record = FileRecord {
        hash,
        symbols,
        outline,
        imports,
        exports,
    };

    index.db.insert(
        path.to_string_lossy().as_bytes(),
        bincode::serialize(&record)?,
    )?;

    index.files.write().insert(path.to_path_buf(), record);
    Ok(())
}

/// Check if a file extension is supported.
pub fn is_supported(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|s| s.to_str()),
        Some("js") | Some("jsx") | Some("ts") | Some("tsx") | Some("vue") |
        Some("py") | Some("rs") | Some("go") |
        Some("json") | Some("html") | Some("htm") | Some("css") |
        Some("md") | Some("markdown") | Some("yaml") | Some("yml")
    )
}
