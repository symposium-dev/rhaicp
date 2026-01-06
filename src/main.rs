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

use anyhow::Result;
use clap::Parser;
use rhaicp::RhaiAgent;
use sacp::Component;
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
    Acp,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize tracing to stderr
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
        Command::Acp => {
            tracing::info!("Rhaicp starting");
            RhaiAgent::new()
                .serve(sacp_tokio::Stdio::new())
                .await?;
        }
    }

    Ok(())
}
