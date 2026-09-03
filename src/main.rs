mod acp;
mod config;
mod harness;
mod markdown;
mod mcp;
mod session;
mod slug;
mod tui;
mod wizard;

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Create an agent in the current folder.
    Init,
    /// Chat with the agent in the current folder.
    Chat(ChatArgs),
    /// Repair the runtime files for the agent in the current folder.
    Repair,
}

#[derive(Args)]
struct ChatArgs {
    /// Resume the most recent session.
    #[arg(short, long)]
    resume: bool,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Init) => wizard::run(std::env::current_dir()?).await,
        Some(Command::Chat(args)) => {
            let root = current_agent_root()?;
            tui::run(root, args.resume).await
        }
        Some(Command::Repair) => repair().await,
        None => print_help(),
    }
}

async fn repair() -> Result<()> {
    let root = current_agent_root()?;
    let config = config::AgentConfig::load(&root)?;
    let selected = harness::get(&config.harness)?;
    harness::find_command(selected.runtime_command()).with_context(|| {
        format!(
            "{} is not installed or is not on PATH",
            selected.display_name()
        )
    })?;
    harness::install_adapter(&root, selected).await?;
    println!("{} runtime files are ready.", selected.display_name());
    Ok(())
}

fn current_agent_root() -> Result<PathBuf> {
    let current = std::env::current_dir()?;
    if current.join(config::STATE_DIR).is_dir() {
        return Ok(current);
    }

    bail!("no agent found in the current folder; run `lazyagents init` first")
}

fn print_help() -> Result<()> {
    Cli::command().print_help()?;
    println!();
    Ok(())
}
