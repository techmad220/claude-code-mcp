# claude-code-mcp

MCP server that exposes Claude Code CLI session history to Claude.ai (or any MCP client).

**The problem**: You do serious work in Claude Code CLI, but when you switch to Claude.ai web/mobile, it can't see any of that history. Your past chat search only covers web conversations.

**The solution**: This MCP server reads Claude Code's local session storage and exposes it via MCP tools, letting Claude.ai search and reference your CLI work.

## Installation

```bash
# Clone or download this repo
cd claude-code-mcp

# Build release binary
cargo build --release

# Binary is at ./target/release/claude-code-mcp
```

## Configuration

Add to your Claude Desktop config (`~/.config/claude/claude_desktop_config.json` or equivalent):

```json
{
  "mcpServers": {
    "claude-code-history": {
      "command": "/path/to/claude-code-mcp"
    }
  }
}
```

Or for Claude.ai MCP integration (when available), add to your MCP servers configuration.

## Available Tools

### `list_sessions`
List recent Claude Code CLI sessions.

```json
{
  "limit": 20  // optional, default 20, max 100
}
```

Returns session IDs, timestamps, message counts, and previews.

### `search_sessions`
Search sessions by keyword using fuzzy matching.

```json
{
  "query": "trading system regime detector",
  "limit": 10  // optional, default 10, max 50
}
```

### `get_session`
Get full content of a specific session.

```json
{
  "session_id": "abc123..."
}
```

Returns all messages with human/assistant labels.

### `get_session_context`
Get a condensed summary of a session for quick context.

```json
{
  "session_id": "abc123..."
}
```

Returns:
- Initial request
- Session stats
- Files mentioned
- Key terms extracted

## How It Works

1. Scans `~/.claude/` for session JSON files
2. Parses both single-object JSON and JSONL formats
3. Extracts messages, timestamps, and project paths
4. Exposes via MCP JSON-RPC over stdio

## Session Storage Locations

The server looks for sessions in:
- `~/.claude/projects/*/sessions/*.json`
- `~/.claude/*.json` (files containing message data)

Claude Code's exact storage format may vary by version. The parser handles:
- Single JSON objects with `messages` array
- JSONL (one message per line)
- Various content formats (string or array of text blocks)

## Development

```bash
# Run tests
cargo test

# Build debug
cargo build

# Run directly (expects MCP JSON-RPC on stdin)
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | cargo run
```

## Example Usage

Once configured, you can ask Claude.ai things like:

- "Search my Claude Code history for the trading system work"
- "What did I work on in Claude Code yesterday?"
- "Find my sessions about the game engine"
- "Get the full context from session abc123"

## License

MIT

## Contributing

PRs welcome. Main areas for improvement:
- Better session format detection
- Support for additional Claude Code storage layouts
- Caching for large session directories
- Incremental updates via MCP notifications
