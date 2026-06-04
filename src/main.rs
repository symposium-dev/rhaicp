//! # rhaicp
//!
//! An ACP agent that executes Rhai scripts with MCP tool access.
//!
//! ## Overview
//!
//! Rhaicp provides a scriptable agent that:
//! - Accepts prompts that are either Rhai programs or contain `<userRequest>...</userRequest>` blocks
//! - Exposes `say(text)` to stream responses back to the client
//! - Exposes `mcp::list_tools(server)` and `mcp::call_tool(server, tool, args)` for MCP access

use std::path::PathBuf;

use agent_client_protocol::schema::SessionId;
use agent_client_protocol::{AcpAgent, ConnectTo};
use anyhow::Result;
use clap::Parser;
use rhaicp::client::RhaiClient;
use rhaicp::{PriorSession, RhaiAgent};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(author, version, about = "Rhai scripting ACP agent", long_about = None)]
struct Args {
    /// Enable debug logging
    #[arg(short, long)]
    debug: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Run as ACP agent over stdio
    Acp {
        /// Prior sessions in the form "session_id:script_path"
        #[arg(short, long = "session", value_parser = parse_session_arg)]
        sessions: Vec<(String, PathBuf)>,
        /// Script to run for new sessions (instead of relay mode)
        #[arg(long = "new-session")]
        new_session_script: Option<PathBuf>,
    },
    /// Run a Rhai script as a client against an external agent
    Client {
        /// Path to the Rhai script file
        #[arg(short, long)]
        script: PathBuf,
        /// The agent command to run (everything after --)
        #[arg(last = true, required = true)]
        agent_cmd: Vec<String>,
    },
}

fn parse_session_arg(s: &str) -> Result<(String, PathBuf), String> {
    let (id, path) = s
        .split_once(':')
        .ok_or_else(|| format!("expected 'session_id:script_path', got '{s}'"))?;
    Ok((id.to_string(), PathBuf::from(path)))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let env_filter = if args.debug {
        EnvFilter::new("rhaicp=debug")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("rhaicp=info"))
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_writer(std::io::stderr),
        )
        .init();

    match args.command {
        Command::Acp {
            sessions,
            new_session_script,
        } => {
            tracing::info!("Rhaicp starting");

            let prior_sessions: Vec<PriorSession> = sessions
                .into_iter()
                .map(|(id, path)| {
                    let script = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                        panic!("Failed to read session script {:?}: {}", path, e)
                    });
                    PriorSession {
                        session_id: SessionId::new(id),
                        script,
                    }
                })
                .collect();

            let mut agent = RhaiAgent::new();

            if !prior_sessions.is_empty() {
                agent = agent.prior_sessions(prior_sessions);
            }

            if let Some(script_path) = new_session_script {
                let script = std::fs::read_to_string(&script_path).unwrap_or_else(|e| {
                    panic!("Failed to read new-session script {:?}: {}", script_path, e)
                });
                agent = agent.new_session_script(script);
            }

            agent
                .connect_to(agent_client_protocol::Stdio::new())
                .await?;
        }
        Command::Client { script, agent_cmd } => {
            let script_content = std::fs::read_to_string(&script)
                .map_err(|e| anyhow::anyhow!("Failed to read script {:?}: {}", script, e))?;

            let cmd_str = agent_cmd.join(" ");
            let agent: AcpAgent = cmd_str
                .parse()
                .map_err(|e| anyhow::anyhow!("Failed to parse agent command: {}", e))?;

            let result = RhaiClient::new().execute(agent, &script_content).await;

            match result {
                Ok(output) => {
                    if !output.is_empty() {
                        println!("{output}");
                    }
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
    }

    Ok(())
}
