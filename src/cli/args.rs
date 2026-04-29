use clap::{Args, Parser, Subcommand, ValueEnum};
use std::fmt;

use crate::harness::apply::DriftPolicy;
use crate::harness::kind::HarnessKind;

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
    #[arg(long, value_enum)]
    pub from: Option<HarnessId>,
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
    #[arg(long, value_enum, conflicts_with = "all")]
    pub harness: Option<HarnessId>,
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
        match self.harness {
            Some(harness) => UseTarget::Harness(harness),
            None => UseTarget::All,
        }
    }

    pub fn drift_policy(&self) -> Option<DriftPolicy> {
        if self.save_changes {
            Some(DriftPolicy::SaveChanges)
        } else if self.discard_changes {
            Some(DriftPolicy::Discard)
        } else {
            None
        }
    }
}

pub enum UseTarget {
    Harness(HarnessId),
    All,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum HarnessId {
    Codex,
    Claude,
    Opencode,
}

impl fmt::Display for HarnessId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Opencode => "opencode",
        };
        formatter.write_str(name)
    }
}

impl HarnessId {
    pub fn kind(self) -> HarnessKind {
        match self {
            Self::Codex => HarnessKind::Codex,
            Self::Claude => HarnessKind::Claude,
            Self::Opencode => HarnessKind::OpenCode,
        }
    }
}
