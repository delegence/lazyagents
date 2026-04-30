use clap::{Args, Parser, Subcommand};

use crate::app::use_profile::DriftDecision;

#[derive(Parser)]
#[command(
    name = "lazyagents",
    version,
    about = "Manage reusable coding-agent profiles"
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
    Create(CreateArgs),
    /// Show details and validation status of a specific profile
    Show(ProfileArg),
    /// Open a profile directory in the default editor
    Edit(ProfileArg),
    /// Delete an inactive profile
    Delete(DeleteArgs),
    /// Apply a profile to one or more harnesses
    Use(UseArgs),
}

#[derive(Args)]
pub struct CreateArgs {
    pub name: String,
    #[arg(long, value_name = "HARNESS")]
    pub from: Option<String>,
}

#[derive(Args)]
pub struct ProfileArg {
    pub name: String,
}

#[derive(Args)]
pub struct DeleteArgs {
    pub name: String,
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args)]
pub struct UseArgs {
    pub profile: String,
    #[arg(long, value_name = "HARNESS", conflicts_with = "all")]
    pub harness: Option<String>,
    #[arg(long, conflicts_with = "harness")]
    pub all: bool,
    #[arg(long, conflicts_with = "discard_changes")]
    pub save_changes: bool,
    #[arg(long)]
    pub discard_changes: bool,
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
