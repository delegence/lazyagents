pub mod doctor;
mod install;
mod profiles;
mod prompt;
#[cfg(test)]
pub(crate) mod test_setup;
mod uninstall;

use clap::{Parser, Subcommand};

use crate::error::Result;
use crate::tui;

#[derive(Debug, Parser)]
#[command(name = "mews", version, about = "Manage AI agent profiles")]
struct Cli {
    #[command(subcommand)]
    command: Option<CommandKind>,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    Tui,
    Profiles {
        #[command(subcommand)]
        command: Option<ProfileCommand>,
    },
    Install {
        source: String,
        #[arg(long = "agent", short = 'a')]
        agents: Vec<String>,
        #[arg(long = "profile", short = 'p')]
        profiles: Vec<String>,
    },
    Uninstall {
        #[arg(long = "agent", short = 'a')]
        agents: Vec<String>,
        #[arg(long = "profile", short = 'p')]
        profiles: Vec<String>,
        #[arg(long = "skill")]
        skills: Vec<String>,
        #[arg(long = "command")]
        commands: Vec<String>,
        #[arg(long = "mcp")]
        mcps: Vec<String>,
    },
    Doctor,
}

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    Rename {
        id: String,
        new_id: String,
    },
    New {
        id: String,
        #[arg(long = "agent", short = 'a')]
        agents: Vec<String>,
    },
    Edit {
        id: String,
        #[arg(long = "agent", short = 'a')]
        agents: Vec<String>,
        #[arg(long = "skill")]
        skills: Vec<String>,
        #[arg(long = "command")]
        commands: Vec<String>,
        #[arg(long = "mcp")]
        mcps: Vec<String>,
    },
    Switch {
        id: String,
        #[arg(long = "agent", short = 'a')]
        agent: Option<String>,
    },
    Rm {
        id: String,
    },
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    if !matches!(cli.command, Some(CommandKind::Doctor)) {
        doctor::sync()?;
    }
    match cli.command {
        None => tui::run(),
        Some(CommandKind::Tui) => tui::run(),
        Some(CommandKind::Profiles { command }) => profiles::handle(command),
        Some(CommandKind::Install {
            source,
            agents,
            profiles,
        }) => install::handle(&source, &agents, &profiles),
        Some(CommandKind::Uninstall {
            agents,
            profiles,
            skills,
            commands,
            mcps,
        }) => uninstall::handle(&agents, &profiles, &skills, &commands, &mcps),
        Some(CommandKind::Doctor) => doctor::run(),
    }
}
