# rhaicp

An ACP agent that executes [Rhai](https://rhai.rs/) scripts with MCP tool access.

## Overview

Rhaicp provides a scriptable agent that:
- Accepts prompts containing Rhai programs (or `<userRequest>...</userRequest>` blocks)
- Exposes `say(text)` to stream responses back to the client
- Exposes `mcp::list_tools(server)` and `mcp::call_tool(server, tool, args)` for MCP server access

## Usage

```bash
# Run as an ACP agent over stdio
cargo run -- acp
```

## Rhai API

### `say(text)`

Streams text back to the client:

```rhai
say("Hello, ");
say("World!");
```

### `mcp::list_tools(server)`

Lists available tools from an MCP server:

```rhai
let tools = mcp::list_tools("my-server");
for tool in tools {
    say(tool + "\n");
}
```

### `mcp::call_tool(server, tool, args)`

Calls a tool on an MCP server:

```rhai
let result = mcp::call_tool("my-server", "echo", #{ message: "hello" });
say(result.content);
```

### `write_file(path, content)`

Writes `content` to the file at `path`. It then sends a `ToolCallUpdate` on the result.

Example:

```rhai
write_file("notes.txt", "Hello from Rhai!");
```

## Script Extraction

If your prompt contains `<userRequest>...</userRequest>` tags, only the content inside those tags is executed as Rhai. Otherwise, the entire prompt is treated as a Rhai script.

## Running Tests

```bash
cargo test
```
