mod mcp_module;

use anyhow::Result;
use mcp_module::McpModule;
use rhai::{Engine, Module};
use sacp::schema::{
    AgentCapabilities, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
    LoadSessionRequest, LoadSessionResponse, McpServer, NewSessionRequest, NewSessionResponse,
    PromptRequest, PromptResponse, SessionId, SessionNotification, SessionUpdate, StopReason,
    TextContent, ToolCallLocation, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
};
use sacp::{AgentToClient, Component, JrConnectionCx, JrRequestCx};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Messages sent from Rhai execution to the async runtime
pub enum RhaiMessage {
    /// Send text to the client via `say()`
    Say(String),
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
}

/// Session data for each active session
struct SessionData {
    mcp_servers: Vec<McpServer>,
}

/// Rhai scripting ACP agent
#[derive(Clone)]
pub struct RhaiAgent {
    sessions: Arc<Mutex<HashMap<SessionId, SessionData>>>,
}

impl RhaiAgent {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn create_session(&self, session_id: &SessionId, mcp_servers: Vec<McpServer>) {
        let mcp_server_count = mcp_servers.len();
        let mut sessions = self.sessions.lock().unwrap();
        sessions.insert(session_id.clone(), SessionData { mcp_servers });
        tracing::info!(
            "Created session: {} with {} MCP servers",
            session_id,
            mcp_server_count
        );
    }

    fn get_mcp_servers(&self, session_id: &SessionId) -> Option<Vec<McpServer>> {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(session_id).map(|s| s.mcp_servers.clone())
    }

    async fn handle_new_session(
        &self,
        request: NewSessionRequest,
        request_cx: JrRequestCx<NewSessionResponse>,
    ) -> Result<(), sacp::Error> {
        tracing::debug!("New session request with cwd: {:?}", request.cwd);

        let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
        self.create_session(&session_id, request.mcp_servers);

        request_cx.respond(NewSessionResponse::new(session_id))
    }

    async fn handle_load_session(
        &self,
        request: LoadSessionRequest,
        request_cx: JrRequestCx<LoadSessionResponse>,
    ) -> Result<(), sacp::Error> {
        tracing::debug!("Load session request: {:?}", request.session_id);

        self.create_session(&request.session_id, vec![]);

        request_cx.respond(LoadSessionResponse::new())
    }

    /// Process the prompt by executing it as a Rhai script
    async fn process_prompt(
        &self,
        request: PromptRequest,
        request_cx: JrRequestCx<PromptResponse>,
        cx: JrConnectionCx<AgentToClient>,
    ) -> Result<(), sacp::Error> {
        let session_id = request.session_id.clone();

        // Extract the Rhai script from the prompt
        let input_text = extract_text_from_prompt(&request.prompt);
        let script = extract_rhai_script(&input_text);

        tracing::debug!(
            "Executing Rhai script in session {}: {}",
            session_id,
            script
        );

        // Get MCP servers for this session
        let mcp_servers = self.get_mcp_servers(&session_id).unwrap_or_default();

        // Create channel for Rhai -> async communication
        let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<RhaiMessage>();

        // Spawn blocking task to run Rhai
        let script_clone = script.clone();
        let rhai_handle =
            tokio::task::spawn_blocking(move || run_rhai_script(&script_clone, msg_tx));

        // Process messages from Rhai execution
        while let Some(msg) = msg_rx.recv().await {
            match msg {
                RhaiMessage::Say(text) => {
                    tracing::debug!(?session_id, ?text, "Rhai say()");
                    cx.send_notification(SessionNotification::new(
                        session_id.clone(),
                        SessionUpdate::AgentMessageChunk(ContentChunk::new(text.into())),
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
                    // Attempt to write the file asynchronously
                    let write_result = tokio::fs::write(&path, content).await;
                    match write_result {
                        Ok(_) => {
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
            }
        }

        // Wait for Rhai to complete and handle any errors
        match rhai_handle.await {
            Ok(Ok(())) => {
                tracing::debug!(?session_id, "Rhai script completed successfully");
            }
            Ok(Err(e)) => {
                // Rhai execution error - send error info to client
                let error_msg = format!("Rhai error: {}", e);
                tracing::warn!(?session_id, ?error_msg, "Rhai script failed");
                cx.send_notification(SessionNotification::new(
                    session_id.clone(),
                    SessionUpdate::AgentMessageChunk(ContentChunk::new(error_msg.into())),
                ))?;
            }
            Err(e) => {
                // Task panicked
                let error_msg = format!("Rhai task panicked: {}", e);
                tracing::error!(?session_id, ?error_msg, "Rhai task panic");
                cx.send_notification(SessionNotification::new(
                    session_id.clone(),
                    SessionUpdate::AgentMessageChunk(ContentChunk::new(error_msg.into())),
                ))?;
            }
        }

        request_cx.respond(PromptResponse::new(StopReason::EndTurn))
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
/// Prefers structured_content if available, otherwise tries to parse
/// the first text content item as JSON, falling back to returning it as a string.
fn extract_tool_result(result: rmcp::model::CallToolResult) -> Result<serde_json::Value, String> {
    // Prefer structured_content if available
    if let Some(structured) = result.structured_content {
        return Ok(structured);
    }

    // Fall back to first text content
    if let Some(content) = result.content.first() {
        if let Some(text_content) = content.as_text() {
            // Try parsing as JSON to preserve types (numbers, booleans, objects, etc.)
            // If that fails, return as a plain string
            return Ok(serde_json::from_str(&text_content.text)
                .unwrap_or_else(|_| serde_json::Value::String(text_content.text.clone())));
        }
    }

    // No usable content
    Err("Tool returned no content".to_string())
}

impl Default for RhaiAgent {
    fn default() -> Self {
        Self::new()
    }
}

/// Run a Rhai script with the given message channel
fn run_rhai_script(script: &str, msg_tx: mpsc::UnboundedSender<RhaiMessage>) -> Result<(), String> {
    let mut engine = Engine::new();

    // Register say() function
    let say_tx = msg_tx.clone();
    engine.register_fn("say", move |text: &str| {
        let _ = say_tx.send(RhaiMessage::Say(text.to_string()));
    });

    // FIXME: In the future, could make this return a bool/error based on the results
    // Register write_file(path, content)
    let write_tx = msg_tx.clone();
    engine.register_fn("write_file", move |path: &str, content: &str| {
        let _ = write_tx.send(RhaiMessage::WriteFile {
            path: path.to_string(),
            content: content.to_string(),
        });
    });

    // Register mcp module
    let mcp_module = McpModule::new(msg_tx);
    let module: Module = mcp_module.into();
    engine.register_static_module("mcp", module.into());

    // Execute the script
    engine.run(script).map_err(|e| e.to_string())
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
/// If the text contains `<userRequest>...</userRequest>`, extract that content
/// Otherwise, treat the entire text as a Rhai script
fn extract_rhai_script(input: &str) -> String {
    // Try to extract from <userRequest> tags
    if let Some(start) = input.find("<userRequest>") {
        if let Some(end) = input.find("</userRequest>") {
            let content_start = start + "<userRequest>".len();
            if content_start < end {
                return input[content_start..end].trim().to_string();
            }
        }
    }

    // Otherwise, use the whole input as the script
    input.trim().to_string()
}

impl Component<sacp::link::AgentToClient> for RhaiAgent {
    async fn serve(
        self,
        client: impl Component<sacp::link::ClientToAgent>,
    ) -> Result<(), sacp::Error> {
        AgentToClient::builder()
            .name("rhaicp")
            .on_receive_request(
                async |initialize: InitializeRequest, request_cx, _cx| {
                    tracing::debug!("Received initialize request");

                    request_cx.respond(
                        InitializeResponse::new(initialize.protocol_version)
                            .agent_capabilities(AgentCapabilities::new()),
                    )
                },
                sacp::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = self.clone();
                    async move |request: NewSessionRequest, request_cx, _cx| {
                        agent.handle_new_session(request, request_cx).await
                    }
                },
                sacp::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = self.clone();
                    async move |request: LoadSessionRequest, request_cx, _cx| {
                        agent.handle_load_session(request, request_cx).await
                    }
                },
                sacp::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let agent = self.clone();
                    async move |request: PromptRequest, request_cx, cx| {
                        let cx_clone = cx.clone();
                        cx.spawn({
                            let agent = agent.clone();
                            async move { agent.process_prompt(request, request_cx, cx_clone).await }
                        })
                    }
                },
                sacp::on_receive_request!(),
            )
            .connect_to(client)?
            .serve()
            .await
    }
}
