use clap::{Args, Parser, Subcommand};

use crate::app::use_profile::DriftDecision;

#[derive(Parser)]
#[command(
    name = "lazyagents",
    version,
    about = "LazyAgents - manage reusable coding-agent profiles",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Show harness and profile health
    Doctor,
    /// Create a new profile skeleton or import from a harness
    New(NewArgs),
    /// Show details and validation status of a specific profile
    Show(ProfileArg),
    /// Apply a profile to one or more harnesses
    Use(UseArgs),
    /// Stop tracking the active profile without changing harness files
    Unset(UnsetArgs),
    /// Open a profile directory in the default editor
    Edit(ProfileArg),
    /// Delete an inactive profile
    Delete(DeleteArgs),
    /// Manage lazyagents settings
    Settings(SettingsArgs),
}

#[derive(Subcommand)]
pub enum SettingsCommand {
    /// Open settings.json in the default editor
    Edit,
    /// Reset settings.json to the built-in defaults
    Reset(SettingsResetArgs),
}

#[derive(Args)]
pub struct SettingsArgs {
    #[command(subcommand)]
    pub command: SettingsCommand,
}

#[derive(Args)]
pub struct SettingsResetArgs {
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args)]
pub struct NewArgs {
    #[arg(value_name = "PROFILE-NAME")]
    pub name: String,
    #[arg(long, short = 'H', value_name = "HARNESS")]
    pub harness: Option<String>,
}

#[derive(Args)]
pub struct ProfileArg {
    #[arg(value_name = "PROFILE-NAME")]
    pub name: String,
}

#[derive(Args)]
pub struct DeleteArgs {
    #[arg(value_name = "PROFILE-NAME")]
    pub name: String,
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args)]
pub struct UseArgs {
    #[arg(value_name = "PROFILE-NAME")]
    pub profile: String,
    #[arg(long, short = 'H', value_name = "HARNESS", conflicts_with = "all")]
    pub harness: Option<String>,
    #[arg(long, conflicts_with = "harness")]
    pub all: bool,
    #[arg(long, conflicts_with = "discard_changes")]
    pub save_changes: bool,
    #[arg(long)]
    pub discard_changes: bool,
}

#[derive(Args)]
pub struct UnsetArgs {
    #[arg(long, short = 'H', value_name = "HARNESS", conflicts_with = "all")]
    pub harness: Option<String>,
    #[arg(long, conflicts_with = "harness")]
    pub all: bool,
}

impl UnsetArgs {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.harness.is_none() && !self.all {
            anyhow::bail!("unset requires either --harness <harness> or --all");
        }
        Ok(())
    }

    pub fn target(&self) -> UnsetTarget {
        match &self.harness {
            Some(harness) => UnsetTarget::Harness(harness.clone()),
            None => UnsetTarget::All,
        }
    }
}

impl UseArgs {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.harness.is_none() && !self.all {
            anyhow::bail!("use requires either --harness <harness> or --all");
        }

        if self.all && self.save_changes {
            anyhow::bail!("--save-changes cannot be used with --all");
        }

        Ok(())
    }

    pub fn target(&self) -> UseTarget {
        match &self.harness {
            Some(harness) => UseTarget::Harness(harness.clone()),
            None => UseTarget::All,
        }
    }

    pub fn drift_decision(&self) -> Option<DriftDecision> {
        if self.save_changes {
            Some(DriftDecision::SaveChanges)
        } else if self.discard_changes {
            Some(DriftDecision::DiscardChanges)
        } else {
            None
        }
    }
}

pub enum UseTarget {
    Harness(String),
    All,
}

pub enum UnsetTarget {
    Harness(String),
    All,
}
