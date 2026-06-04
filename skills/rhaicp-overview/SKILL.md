---
name: rhaicp-overview
description: >
  Use rhaicp to build scripted ACP (agent-client-protocol) agents or clients
  for integration testing. Activate when writing tests that need controllable
  agent behavior, deterministic multi-turn conversations, session load/resume
  with replay, or when configuring RhaiAgent/RhaiClient in a crate that
  depends on rhaicp.
---

# rhaicp

Use rhaicp to build scripted agent-client-protocol (ACP) agents or clients.

## When to use

Activate this skill when:
- Building a crate that depends on `rhaicp` for scripted ACP agents or clients
- Writing integration tests that need controllable agent behavior (deterministic responses, replay, multi-turn)
- Configuring rhaicp for a test harness (prior sessions, scripted new sessions)

Do NOT use when:
- Building a real (non-test) ACP agent — use `agent-client-protocol` directly
- Working with MCP servers that don't involve rhaicp

## How to build a scripted agent

1. **Decide the mode.** Relay mode (agent executes prompt text as Rhai) is simplest for single-turn tests. Scripted mode (agent runs a persistent script with `receive_prompt()`) is needed for multi-turn, load/resume, or deterministic responses.

2. **Write the agent script** (scripted mode only). Use `receive_prompt()` to block until the client sends a prompt. Use `say(text)` to respond. For load replay, gate on `is_load()`.

3. **Configure `RhaiAgent`** with the builder pattern:
   ```rust
   let agent = RhaiAgent::new()
       .prior_sessions(vec![PriorSession { session_id, script }])
       .new_session_script(script);  // optional
   ```

4. **Connect to transport:**
   ```rust
   agent.connect_to(agent_client_protocol::Stdio::new()).await?;
   ```

5. **Write a client test script** using `RhaiClient` to drive the agent:
   ```rust
   RhaiClient::new()
       .execute(agent, r#"
           let s = start_session();
           s.prompt("hello")
       "#)
       .await?;
   ```

6. **Verify** by running `cargo test --test your_test_file` or via CLI:
   ```bash
   rhaicp client --script test.rhai -- rhaicp acp --session sess_id:agent.rhai
   ```

## Agent workflows

### Relay mode (default)

The agent executes each incoming prompt as a Rhai script. The client sends Rhai code; the agent runs it and streams back results.

```rust
RhaiAgent::new().connect_to(transport).await?;
```

**Tests:** `client_session::single_prompt_returns_agent_say_output`, `client_session::multi_turn_conversation`, `say_hello::test_say_hello_world`

### Scripted new sessions

New sessions run a predefined script with `receive_prompt()` instead of relay mode.

```rust
RhaiAgent::new()
    .new_session_script(script)
    .connect_to(transport).await?;
```

**Tests:** `scripted_sessions::new_session_with_script`

### Prior sessions (load/resume)

Configure known session IDs with scripts. These appear in `session/list` and can be loaded or resumed.

```rust
RhaiAgent::new()
    .prior_sessions(vec![
        PriorSession { session_id: SessionId::new("sess_abc"), script },
    ])
    .connect_to(transport).await?;
```

On `session/load`, the script runs with `is_load() == true` — emit replay via `user(text)` and `say(text)` before `receive_prompt()`.

On `session/resume`, `is_load() == false` — skip replay, go to `receive_prompt()`.

**Tests:** `scripted_sessions::load_session_replays_history`, `scripted_sessions::load_session_replay_content`, `scripted_sessions::resume_session_skips_replay`, `scripted_sessions::scripted_session_multi_turn`

## Client workflows

```rust
RhaiClient::new()
    .cwd("/path")
    .execute(agent, script)
    .await?;
```

Client Rhai functions:

- `start_session()` — create a new session, returns SessionHandle
- `load_session(session_id)` — load a prior session (collects replay updates)
- `resume_session(session_id)` — resume without replay
- `list_sessions()` — returns array of session ID strings
- `session.prompt(text)` — send a prompt, returns concatenated agent text
- `session.session_id()` — get the session ID string
- `session.updates()` — array of update maps from load (`{type, text}`)

**Tests:** `scripted_sessions::list_sessions_returns_prior_session_ids`, `scripted_sessions::session_id_accessible`, `client_session::multiple_sessions`

## Gotchas

- **`receive_prompt()` blocks forever if no prompt arrives.** If your script calls `receive_prompt()` but the client never sends a prompt (or sends fewer prompts than the script expects), the agent process hangs. Always match the number of `receive_prompt()` calls to the expected number of `prompt()` calls from the client.

- **Replay happens during `session/load`, not during the first prompt.** The `user()`/`say()` calls that emit replay notifications must come *before* `receive_prompt()`. If you put them after, they'll be treated as normal turn output, not replay.

- **`load_session()` on the client sends a hidden empty prompt.** To collect replay updates, the client internally sends a no-op prompt after load. This means the agent script's first `receive_prompt()` receives an empty string. Design scripts accordingly — the first real user prompt arrives on the *second* `receive_prompt()` call after replay, or handle the empty string explicitly.

- **`user(text)` only makes sense before `receive_prompt()`.** Calling `user()` after `receive_prompt()` emits a user message chunk mid-turn, which is valid ACP but probably not what you want for replay simulation.

- **Relay mode treats prompt text as Rhai.** If your client sends natural language in relay mode, the agent will try to parse it as Rhai and fail. Use scripted mode (with `new_session_script`) when the client needs to send non-Rhai prompt text.

- **Prior session scripts are re-run from scratch on each load/resume.** There is no persistent state between agent spawns. Each time the daemon loads or resumes a session, a fresh script execution starts.

## Error handling

| Symptom | Cause | Fix |
|---------|-------|-----|
| `Rhai error: Syntax error` | Client sent non-Rhai text in relay mode | Switch to scripted mode or send valid Rhai |
| `Session closed while waiting for prompt` | Script called `receive_prompt()` but agent process was shut down | Check that the client sends enough prompts before disconnecting |
| `MCP server not found` | Script calls `mcp::call_tool("name", ...)` but no server with that name in session config | Pass MCP servers via `NewSessionRequest.mcp_servers` |
| Test hangs indefinitely | Mismatch between `receive_prompt()` count and client `prompt()` count | Ensure they match; add timeouts in test harness |

## CLI usage

```bash
# Run as agent (stdio transport)
rhaicp acp --session sess_id:script.rhai --new-session new.rhai

# Run as client against an external agent
rhaicp client --script test.rhai -- /path/to/agent acp
```

**Tests:** `client_cli::cli_runs_script_against_self`

## Key types

| Type | Purpose |
|------|---------|
| `RhaiAgent` | Configurable ACP agent (builder pattern) |
| `RhaiClient` | Scriptable ACP client |
| `PriorSession` | Config for a known session (`session_id` + `script`) |
| `SessionHandle` | Rhai-exposed handle to an active session |

## Rhai functions reference (agent scripts)

| Function | Mode | Description |
|----------|------|-------------|
| `say(text)` | all | Emit an agent message chunk |
| `user(text)` | scripted | Emit a user message chunk (for replay) |
| `receive_prompt()` | scripted | Block until a prompt arrives, returns prompt text |
| `is_load()` | scripted | Returns true if session was loaded (vs resumed/new) |
| `cwd()` | all | Returns the session working directory |
| `write_file(path, content)` | all | Write a file to disk |
| `mcp::list_tools(server)` | all | List tools from an MCP server |
| `mcp::call_tool(server, tool, args)` | all | Call an MCP tool |
