# fetch-mcp-rs

A Rust implementation of an MCP `fetch` server using `rmcp` and stdio transport. It is aligned with the shape of Anthropic's Python fetch server at the tool and prompt level: lowercase `fetch` tool, `fetch` prompt, paginated content reads, and HTML-to-Markdown conversion.

## Features
- `fetch` tool
  - `url` (required)
  - `max_length` (default `5000`)
  - `start_index` (default `0`)
  - `raw` (default `false`)
- `fetch` prompt with required `url`
- HTML converted to Markdown via `html2md`
- Autonomous tool calls obey `robots.txt`
- Up to 5 redirects
- 30 second request timeout
- Internal response download cap for safety

Current differences from Anthropic's Python implementation:
- HTML simplification uses `html2md` instead of `readabilipy + markdownify`

## Quick start
Prerequisites: Rust toolchain (1.75+ recommended)

Build:
```
cargo build --release
```

Run (stdio):
```
cargo run --quiet
```

## Install (from source)
Install the binary to your Cargo bin directory (~/.cargo/bin by default):
```
cargo install --path .
```
Then run it directly (stdio server):
```
~/.cargo/bin/fetch-mcp-rs
```

## Set up in LLM agents (MCP stdio)
Most MCP-capable clients can launch a stdio server by running a command.
Use the installed binary path (e.g., ~/.cargo/bin/fetch-mcp-rs):

- Generic MCP client configuration (conceptual):
  - Command: ~/.cargo/bin/fetch-mcp-rs
  - Args: []

- Cursor (example): add an entry to your MCP servers configuration that runs the binary:
```json
{
  "mcpServers": {
    "fetch-mcp": {
      "command": "~/.cargo/bin/fetch-mcp-rs",
      "args": []
    }
  }
}
```
Restart Cursor if needed so it discovers the server and the `fetch` tool.

- Warp (example): open Warp AI, go to Tools (or MCP servers) and add a new server:
  - Command: ~/.cargo/bin/fetch-mcp-rs
  - Args: []
After adding, Warp should list the `fetch` tool for use in the agent.

## Tool API
Tool name: `fetch`
Description: Fetches a URL from the internet and optionally extracts its contents as markdown.

Parameters (JSON schema):
- `url` (string, required): The URL to fetch
- `max_length` (integer, optional): Maximum number of characters to return (default `5000`)
- `start_index` (integer, optional): Start content from this character index (default `0`)
- `raw` (boolean, optional): Return raw content without markdown conversion (default `false`)

Example calls (from an MCP client):
```json
{
  "name": "fetch",
  "arguments": { "url": "https://example.com" }
}
```
```json
{
  "name": "fetch",
  "arguments": {
    "url": "https://example.com",
    "max_length": 5000,
    "start_index": 0,
    "raw": false
  }
}
```

When the returned content is truncated, the server appends an error marker telling the caller which `start_index` to use for the next chunk.

## Prompt API
Prompt name: `fetch`

Arguments:
- `url` (string, required): The URL to fetch

## Runtime flags
- `--ignore-robots-txt`: disable `robots.txt` enforcement for tool calls
- `--user-agent <value>`: override both autonomous and manual user agents
- `--proxy-url <value>`: route requests through a proxy

## Build profiles
- `cargo build --release`: smallest shipping binary, optimized for size
- `cargo build --profile release-fast`: faster multi-core release builds with a modest size tradeoff

## Use with MCP Inspector
1) Install and run the Inspector:
```
npx @modelcontextprotocol/inspector
```
2) In the Inspector, configure a stdio server that spawns this binary (path to your built executable). The server advertises the `fetch` tool and `fetch` prompt automatically.

## Extend with more tools
This project uses `rmcp` macros for tool routing and manual `ServerHandler` methods for prompts. Add additional `#[tool]` functions to the `FetchServer` impl in `src/main.rs` to grow your toolset.

## Contributing
Issues and PRs are welcome. Please keep code idiomatic, documented, and tested. Consider adding examples and integration tests for new tools.

## License
MIT or Apache-2.0 (match your preference for redistribution).

