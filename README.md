# Sherlock MCP (Rust)

Sherlock is an MCP server over stdio that indexes JavaScript/TypeScript/Vue source code using tree-sitter and exposes tools for file discovery, symbol search, and references.

## Download / installation

Prebuilt binaries are published on **[GitHub Releases](https://github.com/SteelTrace/sherlock/releases)**.

1. Open the latest release.
2. Download the archive for your platform:
   - **Linux (x86_64):** `sherlock-linux-x64.tar.gz`
   - **macOS (Apple Silicon):** `sherlock-macos-arm64.tar.gz`
   - **macOS (Intel):** `sherlock-macos-x64.tar.gz`
3. Extract the archive. You will get a single executable named `sherlock`.
4. Move it to a directory on your `PATH` (optional), or invoke it with a full path.

```bash
tar -xzf sherlock-macos-arm64.tar.gz
chmod +x sherlock
./sherlock --help
```

To build from source instead, see **Build** below.

## Features

- Background indexing with file watcher
- Honors `.gitignore` via `ignore` walker
- Persistent index on disk (default: `.sherlock/index.sled`)
- MCP tools: `list_files`, `file_outline`, `search_symbols`, `find_definition`, `find_references`, `search_text`

## Build

```bash
cargo build --release
```

## Run

```bash
./target/release/sherlock --root /path/to/workspace --paths relative
```

The server logs the workspace root and DB path to stderr and serves MCP over stdio.

## MCP Config Example

Example `mcp.json` (point `command` at the extracted release binary, or at `target/release/sherlock` after a local build):

```json
{
  "mcpServers": {
    "sherlock": {
      "command": "/path/to/sherlock",
      "args": ["--root", "/path/to/workspace", "--paths", "relative"]
    }
  }
}
```

See `examples/mcp.json`.

## Notes

- Supported extensions: `.js`, `.jsx`, `.ts`, `.tsx`, `.vue`
- Vue parsing extracts `<script>` blocks and indexes them via tree-sitter
- `--paths` can be `relative` (default) or `absolute` to control output paths
