use agent_client_protocol::schema::{
    InitializeRequest, ProtocolVersion, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome,
};
use agent_client_protocol::{ActiveSession, Agent, Client, ConnectTo, ConnectionTo};
use rhai::{Dynamic, Engine};
use std::path::PathBuf;
use tokio::sync::mpsc;

/// Messages sent from the Rhai script thread to the async runtime.
enum ClientMessage {
    StartSession {
        response_tx: std::sync::mpsc::Sender<Result<SessionHandle, String>>,
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
    msg_tx: mpsc::UnboundedSender<ClientMessage>,
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
        // Drop the sender so the connection loop sees the channel close
        drop(msg_tx);
        // Wait for the connection task to finish (ignore its result — script result takes priority)
        let _ = connection_handle.await;

        script_result
    }
}

impl Default for RhaiClient {
    fn default() -> Self {
        Self::new()
    }
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

            while let Some(msg) = msg_rx.recv().await {
                match msg {
                    ClientMessage::StartSession { response_tx } => {
                        match cx.build_session(&cwd).block_task().start_session().await {
                            Ok(session) => {
                                let idx = sessions.len();
                                sessions.push(session);
                                let handle = SessionHandle {
                                    session_idx: idx,
                                    msg_tx: mpsc::unbounded_channel().0, // placeholder, not used by async side
                                };
                                let _ = response_tx.send(Ok(handle));
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
                                Ok(()) => match session.read_to_string().await {
                                    Ok(result) => {
                                        let _ = response_tx.send(Ok(result));
                                    }
                                    Err(e) => {
                                        let _ = response_tx.send(Err(e.to_string()));
                                    }
                                },
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

    let ast = engine.compile(script)?;
    let result: Dynamic = engine.eval_ast(&ast)?;
    Ok(result.to_string())
}
