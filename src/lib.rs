pub mod client;
mod mcp_module;

use agent_client_protocol::schema::{
    AgentCapabilities, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
    ListSessionsRequest, ListSessionsResponse, LoadSessionRequest, LoadSessionResponse, McpServer,
    NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, ResumeSessionRequest,
    ResumeSessionResponse, SessionId, SessionInfo, SessionNotification, SessionUpdate, StopReason,
    TextContent, ToolCallLocation, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
};
use agent_client_protocol::{Agent, Client, ConnectTo, ConnectionTo, Responder};
use anyhow::Result;
use mcp_module::McpModule;
use rhai::{Engine, Module};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Messages sent from Rhai execution to the async runtime
pub enum RhaiMessage {
    /// Send text to the client via `say()`
    Say(String),
    /// Send a user message chunk (for replay)
    UserMessage(String),
    /// List tools from an MCP server
    ListTools {
        server: String,
        response_tx: std::sync::mpsc::Sender<Result<Vec<String>, String>>,
    },
    /// Call an MCP tool
    CallTool {
        server: String,
        tool: String,
        args: serde_json::Value,
        response_tx: std::sync::mpsc::Sender<Result<serde_json::Value, String>>,
    },
    /// Write a file on disk
    WriteFile { path: String, content: String },
    /// Script is ready to receive a prompt (blocks until one arrives)
    ReceivePrompt {
        response_tx: std::sync::mpsc::Sender<String>,
    },
}

/// Configuration for a prior session known at startup.
#[derive(Clone)]
pub struct PriorSession {
    pub session_id: SessionId,
    pub script: String,
}

/// Session data for each active session
struct SessionData {
    cwd: String,
    mcp_servers: Vec<McpServer>,
}

/// State for a scripted session (one running a long-lived script with receive_prompt())
struct ScriptedSession {
    prompt_tx: Option<std::sync::mpsc::Sender<String>>,
}

/// Rhai scripting ACP agent
#[derive(Clone)]
pub struct RhaiAgent {
    sessions: Arc<Mutex<HashMap<SessionId, SessionData>>>,
    prior_sessions: Arc<Vec<PriorSession>>,
    new_session_script: Arc<Option<String>>,
    scripted_sessions: Arc<Mutex<HashMap<SessionId, ScriptedSession>>>,
    msg_receivers: Arc<Mutex<HashMap<SessionId, mpsc::UnboundedReceiver<RhaiMessage>>>>,
}

impl RhaiAgent {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            prior_sessions: Arc::new(Vec::new()),
            new_session_script: Arc::new(None),
            scripted_sessions: Arc::new(Mutex::new(HashMap::new())),
            msg_receivers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn prior_sessions(mut self, sessions: Vec<PriorSession>) -> Self {
        self.prior_sessions = Arc::new(sessions);
        self
    }

    pub fn new_session_script(mut self, script: String) -> Self {
        self.new_session_script = Arc::new(Some(script));
        self
    }

    fn create_session(&self, session_id: &SessionId, cwd: String, mcp_servers: Vec<McpServer>) {
        let mcp_server_count = mcp_servers.len();
        let mut sessions = self.sessions.lock().unwrap();
        sessions.insert(session_id.clone(), SessionData { cwd, mcp_servers });
        tracing::info!(
            "Created session: {} with {} MCP servers",
            session_id,
            mcp_server_count
        );
    }

    fn get_session_data(&self, session_id: &SessionId) -> Option<(String, Vec<McpServer>)> {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .get(session_id)
            .map(|s| (s.cwd.clone(), s.mcp_servers.clone()))
    }

    async fn handle_new_session(
        &self,
        request: NewSessionRequest,
        responder: Responder<NewSessionResponse>,
        cx: ConnectionTo<Client>,
    ) -> Result<(), agent_client_protocol::Error> {
        tracing::debug!("New session request with cwd: {:?}", request.cwd);

        let cwd = request.cwd.to_string_lossy().to_string();
        let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
        self.create_session(&session_id, cwd, request.mcp_servers);

        if let Some(script) = self.new_session_script.as_ref() {
            self.start_scripted_session(session_id.clone(), script.clone(), false, &cx)?;
            self.drain_replay_messages(&session_id, &cx).await?;
        }

        responder.respond(NewSessionResponse::new(session_id))
    }

    async fn handle_load_session(
        &self,
        request: LoadSessionRequest,
        responder: Responder<LoadSessionResponse>,
        cx: ConnectionTo<Client>,
    ) -> Result<(), agent_client_protocol::Error> {
        let session_id = &request.session_id;
        tracing::debug!("Load session request: {:?}", session_id);

        self.create_session(session_id, String::new(), vec![]);

        if let Some(prior) = self
            .prior_sessions
            .iter()
            .find(|p| &p.session_id == session_id)
        {
            let script = prior.script.clone();
            self.start_scripted_session(session_id.clone(), script, true, &cx)?;

            // Process replay messages until the script hits receive_prompt() or finishes
            self.drain_replay_messages(session_id, &cx).await?;
        }

        responder.respond(LoadSessionResponse::new())
    }

    async fn handle_resume_session(
        &self,
        request: ResumeSessionRequest,
        responder: Responder<ResumeSessionResponse>,
        cx: ConnectionTo<Client>,
    ) -> Result<(), agent_client_protocol::Error> {
        let session_id = &request.session_id;
        tracing::debug!("Resume session request: {:?}", session_id);

        self.create_session(
            session_id,
            request.cwd.to_string_lossy().to_string(),
            request.mcp_servers,
        );

        if let Some(prior) = self
            .prior_sessions
            .iter()
            .find(|p| &p.session_id == session_id)
        {
            let script = prior.script.clone();
            // is_load = false for resume, so the script can skip replay
            self.start_scripted_session(session_id.clone(), script, false, &cx)?;

            // Still drain any messages the script emits before receive_prompt()
            self.drain_replay_messages(session_id, &cx).await?;
        }

        responder.respond(ResumeSessionResponse::new())
    }

    fn handle_list_sessions(
        &self,
        _request: ListSessionsRequest,
        responder: Responder<ListSessionsResponse>,
    ) -> Result<(), agent_client_protocol::Error> {
        let sessions: Vec<SessionInfo> = self
            .prior_sessions
            .iter()
            .map(|p| SessionInfo::new(p.session_id.clone(), PathBuf::from("/")))
            .collect();

        responder.respond(ListSessionsResponse::new(sessions))
    }

    /// Start a scripted session: spawn a blocking task running the script.
    /// The script can call receive_prompt() to block until a prompt arrives.
    fn start_scripted_session(
        &self,
        session_id: SessionId,
        script: String,
        is_load: bool,
        _cx: &ConnectionTo<Client>,
    ) -> Result<(), agent_client_protocol::Error> {
        let (msg_tx, msg_rx) = mpsc::unbounded_channel::<RhaiMessage>();

        let cwd = self
            .get_session_data(&session_id)
            .map(|(cwd, _)| cwd)
            .unwrap_or_default();

        let msg_tx_clone = msg_tx.clone();
        tokio::task::spawn_blocking(move || {
            run_scripted_session(&script, msg_tx_clone, &cwd, is_load);
        });

        drop(msg_tx);
        self.scripted_sessions
            .lock()
            .unwrap()
            .insert(session_id.clone(), ScriptedSession { prompt_tx: None });

        self.msg_receivers
            .lock()
            .unwrap()
            .insert(session_id, msg_rx);

        Ok(())
    }

    /// Drain replay messages from the scripted session until it blocks on receive_prompt()
    /// or finishes.
    async fn drain_replay_messages(
        &self,
        session_id: &SessionId,
        cx: &ConnectionTo<Client>,
    ) -> Result<(), agent_client_protocol::Error> {
        let mut msg_rx = {
            let mut receivers = self.msg_receivers.lock().unwrap();
            match receivers.remove(session_id) {
                Some(rx) => rx,
                None => return Ok(()),
            }
        };

        loop {
            // Use try_recv in a loop with a small yield to avoid holding the lock
            match msg_rx.recv().await {
                Some(RhaiMessage::Say(text)) => {
                    cx.send_notification(SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::AgentMessageChunk(ContentChunk::new(text.into())),
                    ))?;
                }
                Some(RhaiMessage::UserMessage(text)) => {
                    cx.send_notification(SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::UserMessageChunk(ContentChunk::new(text.into())),
                    ))?;
                }
                Some(RhaiMessage::ReceivePrompt { response_tx }) => {
                    // Script is blocked waiting for a prompt. Store the channel and the receiver.
                    let mut scripted = self.scripted_sessions.lock().unwrap();
                    if let Some(ss) = scripted.get_mut(session_id) {
                        ss.prompt_tx = Some(response_tx);
                    }
                    // Put the receiver back for future prompt processing
                    let mut receivers = self.msg_receivers.lock().unwrap();
                    receivers.insert(session_id.clone(), msg_rx);
                    return Ok(());
                }
                Some(RhaiMessage::WriteFile { path, content }) => {
                    let write_result = tokio::fs::write(&path, &content).await;
                    let update = match write_result {
                        Ok(()) => ToolCallUpdate::new(
                            "write_file_id",
                            ToolCallUpdateFields::new()
                                .status(ToolCallStatus::Completed)
                                .locations(vec![ToolCallLocation::new(path)])
                                .content(vec![
                                    ContentBlock::Text(TextContent::new("Finished writing file."))
                                        .into(),
                                ]),
                        ),
                        Err(e) => ToolCallUpdate::new(
                            "write_file_id",
                            ToolCallUpdateFields::new()
                                .status(ToolCallStatus::Failed)
                                .locations(vec![ToolCallLocation::new(path)])
                                .content(vec![
                                    ContentBlock::Text(TextContent::new(format!("{:?}", e))).into(),
                                ]),
                        ),
                    };
                    cx.send_notification(SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::ToolCallUpdate(update),
                    ))?;
                }
                Some(RhaiMessage::ListTools { response_tx, .. }) => {
                    let _ = response_tx.send(Err("MCP not available during replay".to_string()));
                }
                Some(RhaiMessage::CallTool { response_tx, .. }) => {
                    let _ = response_tx.send(Err("MCP not available during replay".to_string()));
                }
                None => {
                    // Script finished without calling receive_prompt()
                    return Ok(());
                }
            }
        }
    }

    /// Process the prompt by executing it as a Rhai script (relay mode)
    /// or forwarding to a scripted session.
    async fn process_prompt(
        &self,
        request: PromptRequest,
        responder: Responder<PromptResponse>,
        cx: ConnectionTo<Client>,
    ) -> Result<(), agent_client_protocol::Error> {
        let session_id = request.session_id.clone();

        // Check if there's a scripted session waiting for a prompt
        let prompt_tx = {
            let mut scripted = self.scripted_sessions.lock().unwrap();
            scripted
                .get_mut(&session_id)
                .and_then(|ss| ss.prompt_tx.take())
        };

        if let Some(prompt_tx) = prompt_tx {
            // Forward the prompt text to the blocked script
            let input_text = extract_text_from_prompt(&request.prompt);
            let _ = prompt_tx.send(input_text);

            // Now drain messages until the script hits receive_prompt() again or finishes
            self.process_scripted_prompt(&session_id, responder, &cx)
                .await
        } else {
            // Relay mode: execute the prompt text as a Rhai script
            self.process_relay_prompt(request, responder, cx).await
        }
    }

    /// Process messages from a scripted session after a prompt was delivered.
    async fn process_scripted_prompt(
        &self,
        session_id: &SessionId,
        responder: Responder<PromptResponse>,
        cx: &ConnectionTo<Client>,
    ) -> Result<(), agent_client_protocol::Error> {
        let mut msg_rx = {
            let mut receivers = self.msg_receivers.lock().unwrap();
            match receivers.remove(session_id) {
                Some(rx) => rx,
                None => return responder.respond(PromptResponse::new(StopReason::EndTurn)),
            }
        };

        let mcp_servers = self
            .get_session_data(session_id)
            .map(|(_, servers)| servers)
            .unwrap_or_default();

        loop {
            match msg_rx.recv().await {
                Some(RhaiMessage::Say(text)) => {
                    cx.send_notification(SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::AgentMessageChunk(ContentChunk::new(text.into())),
                    ))?;
                }
                Some(RhaiMessage::UserMessage(text)) => {
                    cx.send_notification(SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::UserMessageChunk(ContentChunk::new(text.into())),
                    ))?;
                }
                Some(RhaiMessage::ReceivePrompt { response_tx }) => {
                    let mut scripted = self.scripted_sessions.lock().unwrap();
                    if let Some(ss) = scripted.get_mut(session_id) {
                        ss.prompt_tx = Some(response_tx);
                    }
                    let mut receivers = self.msg_receivers.lock().unwrap();
                    receivers.insert(session_id.clone(), msg_rx);
                    return responder.respond(PromptResponse::new(StopReason::EndTurn));
                }
                Some(RhaiMessage::ListTools {
                    server,
                    response_tx,
                }) => {
                    let result = self.list_tools_async(&mcp_servers, &server).await;
                    let _ = response_tx.send(result);
                }
                Some(RhaiMessage::CallTool {
                    server,
                    tool,
                    args,
                    response_tx,
                }) => {
                    let result = self
                        .call_tool_async(&mcp_servers, &server, &tool, &args)
                        .await;
                    let _ = response_tx.send(result);
                }
                Some(RhaiMessage::WriteFile { path, content }) => {
                    let write_result = tokio::fs::write(&path, &content).await;
                    let update = match write_result {
                        Ok(()) => ToolCallUpdate::new(
                            "write_file_id",
                            ToolCallUpdateFields::new()
                                .status(ToolCallStatus::Completed)
                                .locations(vec![ToolCallLocation::new(&path)])
                                .content(vec![
                                    ContentBlock::Text(TextContent::new("Finished writing file."))
                                        .into(),
                                ]),
                        ),
                        Err(e) => ToolCallUpdate::new(
                            "write_file_id",
                            ToolCallUpdateFields::new()
                                .status(ToolCallStatus::Failed)
                                .locations(vec![ToolCallLocation::new(&path)])
                                .content(vec![
                                    ContentBlock::Text(TextContent::new(format!("{:?}", e))).into(),
                                ]),
                        ),
                    };
                    cx.send_notification(SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::ToolCallUpdate(update),
                    ))?;
                }
                None => {
                    // Script finished
                    return responder.respond(PromptResponse::new(StopReason::EndTurn));
                }
            }
        }
    }

    /// Relay mode: execute the prompt text directly as a Rhai script
    async fn process_relay_prompt(
        &self,
        request: PromptRequest,
        responder: Responder<PromptResponse>,
        cx: ConnectionTo<Client>,
    ) -> Result<(), agent_client_protocol::Error> {
        let session_id = request.session_id.clone();

        let input_text = extract_text_from_prompt(&request.prompt);
        let script = extract_rhai_script(&input_text);

        tracing::debug!(
            "Executing Rhai script in session {}: {}",
            session_id,
            script
        );

        let (cwd, mcp_servers) = self.get_session_data(&session_id).unwrap_or_default();

        let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<RhaiMessage>();

        let script_clone = script.clone();
        let rhai_handle =
            tokio::task::spawn_blocking(move || run_rhai_script(&script_clone, msg_tx, &cwd));

        while let Some(msg) = msg_rx.recv().await {
            match msg {
                RhaiMessage::Say(text) => {
                    tracing::debug!(?session_id, ?text, "Rhai say()");
                    cx.send_notification(SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::AgentMessageChunk(ContentChunk::new(text.into())),
                    ))?;
                }
                RhaiMessage::UserMessage(text) => {
                    cx.send_notification(SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::UserMessageChunk(ContentChunk::new(text.into())),
                    ))?;
                }
                RhaiMessage::ListTools {
                    server,
                    response_tx,
                } => {
                    let result = self.list_tools_async(&mcp_servers, &server).await;
                    let _ = response_tx.send(result);
                }
                RhaiMessage::CallTool {
                    server,
                    tool,
                    args,
                    response_tx,
                } => {
                    let result = self
                        .call_tool_async(&mcp_servers, &server, &tool, &args)
                        .await;
                    let _ = response_tx.send(result);
                }
                RhaiMessage::WriteFile { path, content } => {
                    let write_result = tokio::fs::write(&path, content).await;
                    match write_result {
                        Ok(()) => {
                            let update = ToolCallUpdate::new(
                                "write_file_id",
                                ToolCallUpdateFields::new()
                                    .status(ToolCallStatus::Completed)
                                    .locations(vec![ToolCallLocation::new(path)])
                                    .content(vec![
                                        ContentBlock::Text(TextContent::new(
                                            "Finished writing file.",
                                        ))
                                        .into(),
                                    ]),
                            );
                            cx.send_notification(SessionNotification::new(
                                session_id.clone(),
                                SessionUpdate::ToolCallUpdate(update),
                            ))?;
                        }
                        Err(e) => {
                            let update = ToolCallUpdate::new(
                                "write_file_id",
                                ToolCallUpdateFields::new()
                                    .status(ToolCallStatus::Failed)
                                    .locations(vec![ToolCallLocation::new(path)])
                                    .content(vec![
                                        ContentBlock::Text(TextContent::new(format!("{:?}", e)))
                                            .into(),
                                    ]),
                            );
                            cx.send_notification(SessionNotification::new(
                                session_id.clone(),
                                SessionUpdate::ToolCallUpdate(update),
                            ))?;
                        }
                    }
                }
                RhaiMessage::ReceivePrompt { .. } => {
                    tracing::warn!("receive_prompt() called in relay mode — ignoring");
                }
            }
        }

        match rhai_handle.await {
            Ok(Ok(())) => {
                tracing::debug!(?session_id, "Rhai script completed successfully");
            }
            Ok(Err(e)) => {
                let error_msg = format!("Rhai error: {}", e);
                tracing::warn!(?session_id, ?error_msg, "Rhai script failed");
                cx.send_notification(SessionNotification::new(
                    session_id.clone(),
                    SessionUpdate::AgentMessageChunk(ContentChunk::new(error_msg.into())),
                ))?;
            }
            Err(e) => {
                let error_msg = format!("Rhai task panicked: {}", e);
                tracing::error!(?session_id, ?error_msg, "Rhai task panic");
                cx.send_notification(SessionNotification::new(
                    session_id.clone(),
                    SessionUpdate::AgentMessageChunk(ContentChunk::new(error_msg.into())),
                ))?;
            }
        }

        responder.respond(PromptResponse::new(StopReason::EndTurn))
    }

    async fn list_tools_async(
        &self,
        mcp_servers: &[McpServer],
        server_name: &str,
    ) -> Result<Vec<String>, String> {
        use rmcp::ServiceExt;

        let mcp_server = mcp_servers
            .iter()
            .find(|s| match s {
                McpServer::Stdio(stdio) => stdio.name == server_name,
                McpServer::Http(http) => http.name == server_name,
                McpServer::Sse(sse) => sse.name == server_name,
                _ => false,
            })
            .ok_or_else(|| format!("MCP server '{}' not found", server_name))?;

        match mcp_server {
            McpServer::Stdio(stdio) => {
                use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
                use tokio::process::Command;

                let transport =
                    TokioChildProcess::new(Command::new(&stdio.command).configure(|cmd| {
                        cmd.args(&stdio.args);
                        for env_var in &stdio.env {
                            cmd.env(&env_var.name, &env_var.value);
                        }
                    }))
                    .map_err(|e| format!("Failed to spawn MCP server: {}", e))?;

                let mcp_client = ()
                    .serve(transport)
                    .await
                    .map_err(|e| format!("Failed to connect to MCP server: {}", e))?;

                let tools_result = mcp_client
                    .list_tools(None)
                    .await
                    .map_err(|e| format!("Failed to list tools: {}", e))?;

                let _ = mcp_client.cancel().await;

                Ok(tools_result
                    .tools
                    .into_iter()
                    .map(|t| t.name.to_string())
                    .collect())
            }
            McpServer::Http(http) => {
                use rmcp::transport::StreamableHttpClientTransport;

                let transport = StreamableHttpClientTransport::from_uri(http.url.clone());

                let mcp_client = ()
                    .serve(transport)
                    .await
                    .map_err(|e| format!("Failed to connect to HTTP MCP server: {}", e))?;

                let tools_result = mcp_client
                    .list_tools(None)
                    .await
                    .map_err(|e| format!("Failed to list tools: {}", e))?;

                let _ = mcp_client.cancel().await;

                Ok(tools_result
                    .tools
                    .into_iter()
                    .map(|t| t.name.to_string())
                    .collect())
            }
            _ => Err("SSE MCP servers are not currently supported".to_string()),
        }
    }

    async fn call_tool_async(
        &self,
        mcp_servers: &[McpServer],
        server_name: &str,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        use rmcp::{ServiceExt, model::CallToolRequestParam};

        let mcp_server = mcp_servers
            .iter()
            .find(|s| match s {
                McpServer::Stdio(stdio) => stdio.name == server_name,
                McpServer::Http(http) => http.name == server_name,
                McpServer::Sse(sse) => sse.name == server_name,
                _ => false,
            })
            .ok_or_else(|| format!("MCP server '{}' not found", server_name))?;

        match mcp_server {
            McpServer::Stdio(stdio) => {
                use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
                use tokio::process::Command;

                let transport =
                    TokioChildProcess::new(Command::new(&stdio.command).configure(|cmd| {
                        cmd.args(&stdio.args);
                        for env_var in &stdio.env {
                            cmd.env(&env_var.name, &env_var.value);
                        }
                    }))
                    .map_err(|e| format!("Failed to spawn MCP server: {}", e))?;

                let mcp_client = ()
                    .serve(transport)
                    .await
                    .map_err(|e| format!("Failed to connect to MCP server: {}", e))?;

                let tool_result = mcp_client
                    .call_tool(CallToolRequestParam {
                        name: tool_name.to_string().into(),
                        arguments: args.as_object().cloned(),
                    })
                    .await
                    .map_err(|e| format!("Failed to call tool: {}", e))?;

                let _ = mcp_client.cancel().await;

                extract_tool_result(tool_result)
            }
            McpServer::Http(http) => {
                use rmcp::transport::StreamableHttpClientTransport;

                let transport = StreamableHttpClientTransport::from_uri(http.url.clone());

                let mcp_client = ()
                    .serve(transport)
                    .await
                    .map_err(|e| format!("Failed to connect to HTTP MCP server: {}", e))?;

                let tool_result = mcp_client
                    .call_tool(CallToolRequestParam {
                        name: tool_name.to_string().into(),
                        arguments: args.as_object().cloned(),
                    })
                    .await
                    .map_err(|e| format!("Failed to call tool: {}", e))?;

                let _ = mcp_client.cancel().await;

                extract_tool_result(tool_result)
            }
            _ => Err("SSE MCP servers are not currently supported".to_string()),
        }
    }
}

/// Extract the result value from a CallToolResult.
fn extract_tool_result(result: rmcp::model::CallToolResult) -> Result<serde_json::Value, String> {
    if let Some(structured) = result.structured_content {
        return Ok(structured);
    }

    if let Some(text_content) = result.content.first().and_then(|c| c.as_text()) {
        return Ok(serde_json::from_str(&text_content.text)
            .unwrap_or_else(|_| serde_json::Value::String(text_content.text.clone())));
    }

    Err("Tool returned no content".to_string())
}

impl Default for RhaiAgent {
    fn default() -> Self {
        Self::new()
    }
}

/// Run a Rhai script in relay mode (single prompt execution)
fn run_rhai_script(
    script: &str,
    msg_tx: mpsc::UnboundedSender<RhaiMessage>,
    cwd: &str,
) -> Result<(), String> {
    let mut engine = Engine::new();
    register_common_functions(&mut engine, msg_tx, cwd);
    engine.run(script).map_err(|e| e.to_string())
}

/// Run a scripted session (long-lived script with receive_prompt())
fn run_scripted_session(
    script: &str,
    msg_tx: mpsc::UnboundedSender<RhaiMessage>,
    cwd: &str,
    is_load: bool,
) {
    let mut engine = Engine::new();
    register_common_functions(&mut engine, msg_tx.clone(), cwd);

    // Register receive_prompt() — blocks until a prompt is delivered
    let prompt_msg_tx = msg_tx.clone();
    engine.register_fn("receive_prompt", move || -> String {
        let (response_tx, response_rx) = std::sync::mpsc::channel();
        let _ = prompt_msg_tx.send(RhaiMessage::ReceivePrompt { response_tx });
        match response_rx.recv() {
            Ok(text) => text,
            Err(_) => panic!("Session closed while waiting for prompt"),
        }
    });

    // Register user() — emit a user message chunk (for replay)
    let user_tx = msg_tx.clone();
    engine.register_fn("user", move |text: &str| {
        let _ = user_tx.send(RhaiMessage::UserMessage(text.to_string()));
    });

    // Register is_load variable
    engine.register_fn("is_load", move || -> bool { is_load });

    if let Err(e) = engine.run(script) {
        tracing::warn!("Scripted session failed: {}", e);
    }
}

/// Register functions common to both relay and scripted modes
fn register_common_functions(
    engine: &mut Engine,
    msg_tx: mpsc::UnboundedSender<RhaiMessage>,
    cwd: &str,
) {
    let cwd_value = cwd.to_string();
    engine.register_fn("cwd", move || -> String { cwd_value.clone() });

    let say_tx = msg_tx.clone();
    engine.register_fn("say", move |text: &str| {
        let _ = say_tx.send(RhaiMessage::Say(text.to_string()));
    });

    let write_tx = msg_tx.clone();
    engine.register_fn("write_file", move |path: &str, content: &str| {
        let _ = write_tx.send(RhaiMessage::WriteFile {
            path: path.to_string(),
            content: content.to_string(),
        });
    });

    engine.register_fn("sleep", |ms: i64| {
        std::thread::sleep(std::time::Duration::from_millis(ms as u64));
    });

    engine.register_fn("exit", |code: i64| {
        std::process::exit(code as i32);
    });

    let mcp_module = McpModule::new(msg_tx);
    let module: Module = mcp_module.into();
    engine.register_static_module("mcp", module.into());
}

/// Extract text content from prompt blocks
fn extract_text_from_prompt(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(TextContent { text, .. }) => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract Rhai script from input text
fn extract_rhai_script(input: &str) -> String {
    if let (Some(start), Some(end)) = (input.find("<userRequest>"), input.find("</userRequest>")) {
        let content_start = start + "<userRequest>".len();
        if content_start < end {
            return input[content_start..end].trim().to_string();
        }
    }

    input.trim().to_string()
}

impl ConnectTo<Client> for RhaiAgent {
    async fn connect_to(
        self,
        client: impl ConnectTo<Agent>,
    ) -> Result<(), agent_client_protocol::Error> {
        Agent
            .builder()
            .name("rhaicp")
            .on_receive_request(
                async |initialize: InitializeRequest, responder, _cx| {
                    tracing::debug!("Received initialize request");

                    responder.respond(
                        InitializeResponse::new(initialize.protocol_version)
                            .agent_capabilities(AgentCapabilities::new()),
                    )
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = self.clone();
                    async move |request: NewSessionRequest, responder, cx| {
                        agent.handle_new_session(request, responder, cx).await
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = self.clone();
                    async move |request: LoadSessionRequest, responder, cx| {
                        agent.handle_load_session(request, responder, cx).await
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = self.clone();
                    async move |request: ResumeSessionRequest, responder, cx| {
                        agent.handle_resume_session(request, responder, cx).await
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = self.clone();
                    async move |request: ListSessionsRequest, responder, _cx| {
                        agent.handle_list_sessions(request, responder)
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = self.clone();
                    async move |request: PromptRequest, responder, cx| {
                        cx.spawn({
                            let agent = agent.clone();
                            let cx_clone = cx.clone();
                            async move { agent.process_prompt(request, responder, cx_clone).await }
                        })
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_to(client)
            .await
    }
}
