fix: MCP extensions work with servers on the stateless 2026-07-28 MCP revision: croft probes each server with `server/discover` and falls back to the `initialize` handshake only for servers that predate it.
fix: The `croft collab-agent` seat answers MCP clients on the 2026-07-28 revision, which previously failed to list its tools, while still serving clients that open with `initialize`.
