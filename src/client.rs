use agent_client_protocol::schema::{
    ContentBlock, ContentChunk, InitializeRequest, ListSessionsRequest, LoadSessionRequest,
    ProtocolVersion, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    ResumeSessionRequest, SelectedPermissionOutcome, SessionId, SessionNotification, SessionUpdate,
    TextContent,
};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{
    ActiveSession, Agent, Client, ConnectTo, ConnectionTo, SessionMessage,
};
use rhai::{Array, Dynamic, Engine, Map};
use std::path::PathBuf;
use tokio::sync::mpsc;

/// Messages sent from the Rhai script thread to the async runtime.
enum ClientMessage {
    StartSession {
        response_tx: std::sync::mpsc::Sender<Result<SessionHandle, String>>,
    },
    LoadSession {
        session_id: String,
        response_tx: std::sync::mpsc::Sender<Result<SessionHandle, String>>,
    },
    ResumeSession {
        session_id: String,
        response_tx: std::sync::mpsc::Sender<Result<SessionHandle, String>>,
    },
    ListSessions {
        response_tx: std::sync::mpsc::Sender<Result<Vec<String>, String>>,
    },
    Prompt {
        session_idx: usize,
        text: String,
        response_tx: std::sync::mpsc::Sender<Result<String, String>>,
    },
}

/// Handle to an active session, exposed to Rhai scripts.
#[derive(Clone)]
pub struct SessionHandle {
    session_idx: usize,
    session_id: String,
    msg_tx: mpsc::UnboundedSender<ClientMessage>,
    updates: Vec<Dynamic>,
}

/// A scripted ACP client that drives sessions against external agents.
pub struct RhaiClient {
    cwd: Option<PathBuf>,
}

impl RhaiClient {
    pub fn new() -> Self {
        Self { cwd: None }
    }

    pub fn cwd(mut self, path: impl Into<PathBuf>) -> Self {
        self.cwd = Some(path.into());
        self
    }

    /// Execute a Rhai script against the given agent, returning the script's last expression
    /// as a string.
    pub async fn execute(
        self,
        agent: impl ConnectTo<Client>,
        script: &str,
    ) -> Result<String, anyhow::Error> {
        let script = script.to_string();
        let cwd = self
            .cwd
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let (msg_tx, msg_rx) = mpsc::unbounded_channel::<ClientMessage>();

        let script_handle = tokio::task::spawn_blocking({
            let msg_tx = msg_tx.clone();
            move || run_client_script(&script, msg_tx)
        });

        let connection_handle = tokio::spawn(run_connection(agent, msg_rx, cwd));

        let script_result = script_handle.await?;
        drop(msg_tx);
        let _ = connection_handle.await;

        script_result
    }
}

impl Default for RhaiClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Collected updates during a session load or prompt
struct CollectedUpdates {
    text: String,
    updates: Vec<Dynamic>,
}

/// Convert a SessionUpdate to a Rhai Dynamic map for script access
fn session_update_to_dynamic(update: &SessionUpdate) -> Dynamic {
    let mut map = Map::new();
    match update {
        SessionUpdate::AgentMessageChunk(ContentChunk {
            content: ContentBlock::Text(TextContent { text, .. }),
            ..
        }) => {
            map.insert("type".into(), "agent_message_chunk".into());
            map.insert("text".into(), Dynamic::from(text.clone()));
        }
        SessionUpdate::UserMessageChunk(ContentChunk {
            content: ContentBlock::Text(TextContent { text, .. }),
            ..
        }) => {
            map.insert("type".into(), "user_message_chunk".into());
            map.insert("text".into(), Dynamic::from(text.clone()));
        }
        SessionUpdate::ToolCallUpdate(tool_update) => {
            map.insert("type".into(), "tool_call_update".into());
            map.insert(
                "tool_call_id".into(),
                Dynamic::from(tool_update.tool_call_id.to_string()),
            );
        }
        _ => {
            map.insert("type".into(), "other".into());
        }
    }
    Dynamic::from(map)
}

/// Read all updates from a session until EndTurn, collecting both text and structured updates
async fn read_session_updates(session: &mut ActiveSession<'static, Agent>) -> CollectedUpdates {
    let mut text = String::new();
    let mut updates: Vec<Dynamic> = Vec::new();

    loop {
        match session.read_update().await {
            Ok(SessionMessage::SessionMessage(dispatch)) => {
                let result: Result<(), agent_client_protocol::Error> = MatchDispatch::new(dispatch)
                    .if_notification(async |notif: SessionNotification| {
                        updates.push(session_update_to_dynamic(&notif.update));
                        if let SessionUpdate::AgentMessageChunk(ContentChunk {
                            content: ContentBlock::Text(text_content),
                            ..
                        }) = &notif.update
                        {
                            text.push_str(&text_content.text);
                        }
                        Ok(())
                    })
                    .await
                    .otherwise_ignore();
                if result.is_err() {
                    break;
                }
            }
            Ok(SessionMessage::StopReason(_)) => break,
            Ok(_) => {}
            Err(_) => break,
        }
    }

    CollectedUpdates { text, updates }
}

/// Run the ACP client connection, processing messages from the Rhai script.
async fn run_connection(
    agent: impl ConnectTo<Client>,
    mut msg_rx: mpsc::UnboundedReceiver<ClientMessage>,
    cwd: PathBuf,
) {
    let result: Result<(), agent_client_protocol::Error> = Client
        .builder()
        .name("rhaicp-client")
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _cx| {
                let outcome = match request.options.first() {
                    Some(opt) => RequestPermissionOutcome::Selected(
                        SelectedPermissionOutcome::new(opt.option_id.clone()),
                    ),
                    None => RequestPermissionOutcome::Cancelled,
                };
                responder.respond(RequestPermissionResponse::new(outcome))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent, async move |cx: ConnectionTo<Agent>| {
            cx.send_request(InitializeRequest::new(ProtocolVersion::LATEST))
                .block_task()
                .await?;

            let mut sessions: Vec<ActiveSession<'static, Agent>> = Vec::new();
            let mut session_updates: Vec<Vec<Dynamic>> = Vec::new();

            while let Some(msg) = msg_rx.recv().await {
                match msg {
                    ClientMessage::StartSession { response_tx } => {
                        match cx.build_session(&cwd).block_task().start_session().await {
                            Ok(session) => {
                                let idx = sessions.len();
                                let session_id = session.session_id().to_string();
                                sessions.push(session);
                                session_updates.push(Vec::new());
                                let handle = SessionHandle {
                                    session_idx: idx,
                                    session_id,
                                    msg_tx: mpsc::unbounded_channel().0,
                                    updates: Vec::new(),
                                };
                                let _ = response_tx.send(Ok(handle));
                            }
                            Err(e) => {
                                let _ = response_tx.send(Err(e.to_string()));
                            }
                        }
                    }
                    ClientMessage::LoadSession {
                        session_id,
                        response_tx,
                    } => {
                        use agent_client_protocol::schema::NewSessionResponse;

                        // Register handler BEFORE sending load request so we capture
                        // replay notifications that arrive before the response.
                        let fake_response =
                            NewSessionResponse::new(SessionId::new(session_id.as_str()));
                        match cx.attach_session(fake_response, Default::default()) {
                            Ok(mut active_session) => {
                                let sid = SessionId::new(session_id.as_str());
                                let request = LoadSessionRequest::new(sid, &cwd);
                                match cx.send_request(request).block_task().await {
                                    Ok(_response) => {
                                        let idx = sessions.len();
                                        // Send a no-op prompt to get a StopReason so we can
                                        // drain all buffered notifications (replay + prompt end).
                                        if active_session.send_prompt("").is_ok() {
                                            let collected =
                                                read_session_updates(&mut active_session).await;
                                            session_updates.push(collected.updates.clone());
                                            sessions.push(active_session);
                                            let handle = SessionHandle {
                                                session_idx: idx,
                                                session_id,
                                                msg_tx: mpsc::unbounded_channel().0,
                                                updates: collected.updates,
                                            };
                                            let _ = response_tx.send(Ok(handle));
                                        } else {
                                            session_updates.push(Vec::new());
                                            sessions.push(active_session);
                                            let handle = SessionHandle {
                                                session_idx: idx,
                                                session_id,
                                                msg_tx: mpsc::unbounded_channel().0,
                                                updates: Vec::new(),
                                            };
                                            let _ = response_tx.send(Ok(handle));
                                        }
                                    }
                                    Err(e) => {
                                        let _ = response_tx.send(Err(e.to_string()));
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = response_tx.send(Err(e.to_string()));
                            }
                        }
                    }
                    ClientMessage::ResumeSession {
                        session_id,
                        response_tx,
                    } => {
                        use agent_client_protocol::schema::NewSessionResponse;

                        let fake_response =
                            NewSessionResponse::new(SessionId::new(session_id.as_str()));
                        match cx.attach_session(fake_response, Default::default()) {
                            Ok(active_session) => {
                                let sid = SessionId::new(session_id.as_str());
                                let request = ResumeSessionRequest::new(sid, &cwd);
                                match cx.send_request(request).block_task().await {
                                    Ok(_response) => {
                                        let idx = sessions.len();
                                        session_updates.push(Vec::new());
                                        sessions.push(active_session);
                                        let handle = SessionHandle {
                                            session_idx: idx,
                                            session_id,
                                            msg_tx: mpsc::unbounded_channel().0,
                                            updates: Vec::new(),
                                        };
                                        let _ = response_tx.send(Ok(handle));
                                    }
                                    Err(e) => {
                                        let _ = response_tx.send(Err(e.to_string()));
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = response_tx.send(Err(e.to_string()));
                            }
                        }
                    }
                    ClientMessage::ListSessions { response_tx } => {
                        match cx
                            .send_request(ListSessionsRequest::new())
                            .block_task()
                            .await
                        {
                            Ok(response) => {
                                let ids: Vec<String> = response
                                    .sessions
                                    .iter()
                                    .map(|s| s.session_id.to_string())
                                    .collect();
                                let _ = response_tx.send(Ok(ids));
                            }
                            Err(e) => {
                                let _ = response_tx.send(Err(e.to_string()));
                            }
                        }
                    }
                    ClientMessage::Prompt {
                        session_idx,
                        text,
                        response_tx,
                    } => {
                        if let Some(session) = sessions.get_mut(session_idx) {
                            match session.send_prompt(&text) {
                                Ok(()) => {
                                    let collected = read_session_updates(session).await;
                                    // Update stored updates for this session
                                    if let Some(stored) = session_updates.get_mut(session_idx) {
                                        stored.extend(collected.updates);
                                    }
                                    let _ = response_tx.send(Ok(collected.text));
                                }
                                Err(e) => {
                                    let _ = response_tx.send(Err(e.to_string()));
                                }
                            }
                        } else {
                            let _ = response_tx
                                .send(Err(format!("Invalid session index: {}", session_idx)));
                        }
                    }
                }
            }

            Ok(())
        })
        .await;

    if let Err(e) = result {
        tracing::warn!("Client connection ended with error: {}", e);
    }
}

fn run_client_script(
    script: &str,
    msg_tx: mpsc::UnboundedSender<ClientMessage>,
) -> Result<String, anyhow::Error> {
    let mut engine = Engine::new();

    // Register start_session() -> SessionHandle
    let tx = msg_tx.clone();
    engine.register_fn("start_session", move || -> SessionHandle {
        let (response_tx, response_rx) = std::sync::mpsc::channel();
        let _ = tx.send(ClientMessage::StartSession { response_tx });
        match response_rx.recv() {
            Ok(Ok(mut handle)) => {
                handle.msg_tx = tx.clone();
                handle
            }
            Ok(Err(e)) => panic!("Failed to start session: {}", e),
            Err(_) => panic!("Connection closed while starting session"),
        }
    });

    // Register load_session(session_id) -> SessionHandle
    let tx = msg_tx.clone();
    engine.register_fn("load_session", move |session_id: &str| -> SessionHandle {
        let (response_tx, response_rx) = std::sync::mpsc::channel();
        let _ = tx.send(ClientMessage::LoadSession {
            session_id: session_id.to_string(),
            response_tx,
        });
        match response_rx.recv() {
            Ok(Ok(mut handle)) => {
                handle.msg_tx = tx.clone();
                handle
            }
            Ok(Err(e)) => panic!("Failed to load session: {}", e),
            Err(_) => panic!("Connection closed while loading session"),
        }
    });

    // Register resume_session(session_id) -> SessionHandle
    let tx = msg_tx.clone();
    engine.register_fn("resume_session", move |session_id: &str| -> SessionHandle {
        let (response_tx, response_rx) = std::sync::mpsc::channel();
        let _ = tx.send(ClientMessage::ResumeSession {
            session_id: session_id.to_string(),
            response_tx,
        });
        match response_rx.recv() {
            Ok(Ok(mut handle)) => {
                handle.msg_tx = tx.clone();
                handle
            }
            Ok(Err(e)) => panic!("Failed to resume session: {}", e),
            Err(_) => panic!("Connection closed while resuming session"),
        }
    });

    // Register list_sessions() -> Array of session ID strings
    let tx = msg_tx.clone();
    engine.register_fn("list_sessions", move || -> Array {
        let (response_tx, response_rx) = std::sync::mpsc::channel();
        let _ = tx.send(ClientMessage::ListSessions { response_tx });
        match response_rx.recv() {
            Ok(Ok(ids)) => ids.into_iter().map(Dynamic::from).collect(),
            Ok(Err(e)) => panic!("Failed to list sessions: {}", e),
            Err(_) => panic!("Connection closed while listing sessions"),
        }
    });

    // Register session.prompt(text) -> String
    engine.register_fn(
        "prompt",
        |session: &mut SessionHandle, text: &str| -> String {
            let (response_tx, response_rx) = std::sync::mpsc::channel();
            let _ = session.msg_tx.send(ClientMessage::Prompt {
                session_idx: session.session_idx,
                text: text.to_string(),
                response_tx,
            });
            match response_rx.recv() {
                Ok(Ok(result)) => result,
                Ok(Err(e)) => panic!("Prompt failed: {}", e),
                Err(_) => panic!("Connection closed during prompt"),
            }
        },
    );

    // Register sleep(ms)
    engine.register_fn("sleep", |ms: i64| {
        std::thread::sleep(std::time::Duration::from_millis(ms as u64));
    });

    // Register session.session_id() -> String
    engine.register_fn("session_id", |session: &mut SessionHandle| -> String {
        session.session_id.clone()
    });

    // Register session.updates() -> Array of update maps
    engine.register_fn("updates", |session: &mut SessionHandle| -> Array {
        session.updates.clone()
    });

    let ast = engine.compile(script)?;
    let result: Dynamic = engine.eval_ast(&ast)?;
    Ok(result.to_string())
}
