# Sherlock MCP (Rust)

Sherlock is an MCP server over stdio that indexes JavaScript/TypeScript/Vue source code using tree-sitter and exposes tools for file discovery, symbol search, and references.

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

Example `mcp.json`:

```json
{
  "mcpServers": {
    "sherlock": {
      "command": "/path/to/target/release/sherlock",
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
