pub mod args;
pub mod render;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};
use std::io::{self, Write};
use std::process::Command as ProcessCommand;

use crate::app::create_profile::{create_profile, CreateProfileResult};
use crate::app::doctor::{doctor_report, DoctorReport, HarnessStatus};
use crate::app::edit_profile::edit_profile_path;
use crate::app::harness_registry::{BuiltInHarnessRegistry, HarnessRegistry};
use crate::app::inspect_profile::inspect_profile;
use crate::app::use_profile::{
    use_profile_workflow, DriftDecision, UseProfileOutcome, UseProfileRequest, UseProfileTarget,
};
use crate::harness::apply::ProfileUseStatus;
use crate::harness::integration::AppEnvironment;
use crate::profile::{LazyagentsHome, ProfileName, ProfileStore};

use args::{Cli, Command, UseTarget};
use render::{
    render_artifact_status, render_json_value, render_mcp_summary, render_string_list,
    render_validation_issues,
};

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let home = LazyagentsHome::resolve()?;
    let runtime_env = AppEnvironment::resolve(home.path().to_path_buf())?;
    let store = ProfileStore::new(home);
    let registry = BuiltInHarnessRegistry;

    match cli.command {
        None => {
            Cli::command().print_help()?;
            println!();
        }
        Some(Command::Doctor) => print_doctor(&runtime_env, &store)?,
        Some(Command::Create(args)) => {
            let profile = ProfileName::parse(args.name)?;
            let from = args
                .from
                .as_deref()
                .map(|id| registry.require_kind(id))
                .transpose()?;
            match create_profile(&registry, &runtime_env, &store, profile, from)? {
                CreateProfileResult::Created { profile, path } => {
                    println!("Created profile {profile} at {}", path.display());
                }
                CreateProfileResult::Imported {
                    profile,
                    harness,
                    path,
                } => {
                    println!(
                        "Created profile {profile} from {harness} at {}",
                        path.display()
                    );
                }
            }
        }
        Some(Command::Show(args)) => {
            let profile = ProfileName::parse(args.name)?;
            let summary = inspect_profile(&store, &profile)?;
            println!("Profile: {}", summary.name);
            println!("Path: {}", summary.path.display());
            println!(
                "Display name: {}",
                summary.display_name.as_deref().unwrap_or("-")
            );
            println!(
                "Description: {}",
                summary.description.as_deref().unwrap_or("-")
            );
            println!(
                "Instruction Source: {}",
                render_artifact_status(&summary.instruction_source)
            );
            println!(
                "Valid Skills: {}",
                render_string_list(&summary.valid_skills)
            );
            println!(
                "Ignored Skills: {}",
                render_string_list(&summary.ignored_skills)
            );
            println!(
                "Profile Commands: {}",
                render_string_list(&summary.commands)
            );
            println!(
                "Ignored Command Files: {}",
                render_string_list(&summary.ignored_command_files)
            );
            println!("MCPs: {}", render_mcp_summary(&summary.mcp_summary));
            println!("Model Preferences:");
            for (harness, value) in summary.models {
                println!("  {harness}: {}", render_json_value(&value));
            }
            println!("Permission Preferences:");
            for (harness, value) in summary.permissions {
                println!("  {harness}: {}", render_json_value(&value));
            }

            if !summary.validation_issues.is_empty() {
                println!("\nValidation Issues:");
                print!("{}", render_validation_issues(&summary.validation_issues));
            }
        }
        Some(Command::Edit(args)) => {
            let path = edit_profile_path(&store, &args.name)?;
            if !open_editor_or_print_path(&path)? {
                println!("{}", path.display());
            }
        }
        Some(Command::Delete(args)) => {
            if !args.yes && !confirm_delete(&args.name)? {
                println!("Delete cancelled");
                return Ok(());
            }
            let path = crate::app::delete_profile::delete_profile(
                &registry,
                &runtime_env,
                &store,
                &args.name,
            )?;
            println!("Deleted profile {} at {}", args.name, path.display());
        }
        Some(Command::Use(args)) => {
            let profile = ProfileName::parse(args.profile.clone())?;
            args.validate()?;
            match args.target() {
                UseTarget::Harness(harness_id) => {
                    let kind = registry.require_kind(&harness_id)?;
                    let outcome = use_profile_workflow(
                        &registry,
                        &runtime_env,
                        &store,
                        UseProfileRequest {
                            profile: profile.clone(),
                            target: UseProfileTarget::Harness(kind),
                            drift_decision: args.drift_decision(),
                        },
                    )?;
                    let result = match outcome {
                        UseProfileOutcome::Applied(result) => result,
                        UseProfileOutcome::NeedsSingleHarnessDriftDecision {
                            harness,
                            drift,
                            ..
                        } => {
                            let _ = drift.is_clean();
                            let decision = prompt_single_drift_decision(harness.display_name())?;
                            match use_profile_workflow(
                                &registry,
                                &runtime_env,
                                &store,
                                UseProfileRequest {
                                    profile: profile.clone(),
                                    target: UseProfileTarget::Harness(kind),
                                    drift_decision: Some(decision),
                                },
                            )? {
                                UseProfileOutcome::Applied(result) => result,
                                _ => unreachable!(
                                    "drift decision should complete single harness use"
                                ),
                            }
                        }
                        _ => unreachable!("single harness request returned all-harness outcome"),
                    };
                    match result.status {
                        ProfileUseStatus::Applied => {
                            println!(
                                "Used profile {} with {}",
                                result.profile,
                                result.harness.display_name()
                            );
                        }
                        ProfileUseStatus::CancelledForDrift => {
                            println!(
                                "Use cancelled because {} has drift",
                                result.harness.display_name()
                            );
                        }
                    }
                }
                UseTarget::All => {
                    let outcome = use_profile_workflow(
                        &registry,
                        &runtime_env,
                        &store,
                        UseProfileRequest {
                            profile: profile.clone(),
                            target: UseProfileTarget::All,
                            drift_decision: args.drift_decision(),
                        },
                    )?;
                    let results = match outcome {
                        UseProfileOutcome::All(results) => results,
                        UseProfileOutcome::NeedsAllHarnessDriftDecision { harnesses } => {
                            let names = harnesses
                                .iter()
                                .map(|harness| harness.display_name())
                                .collect::<Vec<_>>();
                            if !prompt_all_drift_discard(&names)? {
                                anyhow::bail!(
                                    "operation cancelled due to drift in {}",
                                    names.join(", ")
                                );
                            }
                            match use_profile_workflow(
                                &registry,
                                &runtime_env,
                                &store,
                                UseProfileRequest {
                                    profile: profile.clone(),
                                    target: UseProfileTarget::All,
                                    drift_decision: Some(DriftDecision::DiscardChanges),
                                },
                            )? {
                                UseProfileOutcome::All(results) => results,
                                _ => {
                                    unreachable!("discard decision should complete all harness use")
                                }
                            }
                        }
                        _ => unreachable!("all-harness request returned single-harness outcome"),
                    };

                    let mut summary = Vec::new();
                    for res in results.applied {
                        summary.push(format!("✅ {} applied", res.harness.display_name()));
                    }
                    for (harness, e) in results.failures {
                        summary.push(format!("❌ {} failed: {}", harness.display_name(), e));
                    }

                    println!("\nSummary:");
                    for line in summary {
                        println!("{}", line);
                    }
                }
            }
        }
    }

    Ok(())
}

fn print_doctor(runtime_env: &AppEnvironment, store: &ProfileStore) -> Result<()> {
    let registry = BuiltInHarnessRegistry;
    let report = doctor_report(&registry, runtime_env, store)?;
    print_harness_doctor(&report.harnesses);
    print_profile_doctor(&report);
    Ok(())
}

fn open_editor_or_print_path(path: &std::path::Path) -> Result<bool> {
    match std::env::var_os("EDITOR") {
        Some(editor) if !editor.is_empty() => {
            let editor = std::path::PathBuf::from(editor);
            let status = ProcessCommand::new(&editor)
                .arg(path)
                .status()
                .with_context(|| format!("failed to run editor {}", editor.display()))?;
            if !status.success() {
                anyhow::bail!("editor {} exited with {status}", editor.display());
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn confirm_delete(name: &str) -> Result<bool> {
    print!("Delete profile {name}? [y/N] ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim(), "y" | "Y" | "yes" | "YES"))
}

fn prompt_single_drift_decision(harness: &str) -> Result<DriftDecision> {
    use std::io::IsTerminal;

    if !io::stdin().is_terminal() {
        anyhow::bail!(
            "drift detected in {harness}; pass --save-changes or --discard-changes to proceed"
        );
    }

    println!("Drift detected in {harness}");
    loop {
        print!("Save changes [s], discard [d], or cancel [c]? ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        match input.trim().to_lowercase().as_str() {
            "s" | "save" => return Ok(DriftDecision::SaveChanges),
            "d" | "discard" => return Ok(DriftDecision::DiscardChanges),
            "c" | "cancel" => return Ok(DriftDecision::Cancel),
            _ => println!("Please enter s, d, or c."),
        }
    }
}

fn prompt_all_drift_discard(names: &[&str]) -> Result<bool> {
    use std::io::IsTerminal;

    let joined = names.join(", ");
    if !io::stdin().is_terminal() {
        anyhow::bail!("drift detected in {joined}; pass --discard-changes to proceed");
    }

    println!("Drift detected in: {joined}");
    print!("Proceed and discard changes? [y/N] ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim(), "y" | "Y" | "yes" | "YES"))
}

fn print_harness_doctor(rows: &[HarnessStatus]) {
    if rows.is_empty() {
        println!("[x] Harnesses (0 available)");
        return;
    }

    let names = rows
        .iter()
        .map(|row| row.harness.id())
        .collect::<Vec<_>>()
        .join(", ");
    println!("[✓] Harnesses ({} available: {})", rows.len(), names);
}

fn print_profile_doctor(report: &DoctorReport) {
    let suffix = if report.profiles.summary.is_empty() {
        "".to_string()
    } else {
        format!(" ({}):", report.profiles.summary.join(", "))
    };

    println!("{} Profiles{suffix}", report.profiles.marker);
    for line in &report.profiles.lines {
        println!("{line}");
    }
}
