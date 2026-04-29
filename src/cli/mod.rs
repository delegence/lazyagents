pub mod args;
pub mod render;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};

use crate::harness::apply::{use_profile, DriftPolicy, ProfileUseStatus};
use crate::harness::integration::{Detection, RuntimeEnv};
use crate::harness::registry;
use crate::harness::status::{status_rows, DriftState, StatusProfile, StatusRow};
use crate::profile::ProfileConfigStatus;
use crate::profile::{confirm_delete, LazyagentsHome, ProfileName, ProfileStore};

use args::{Cli, Command, UseTarget};
use render::{
    render_artifact_status, render_json_value, render_mcp_summary, render_string_list,
    render_validation_issues,
};

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let home = LazyagentsHome::resolve()?;
    let runtime_env = RuntimeEnv::resolve(home.path().to_path_buf())?;
    let store = ProfileStore::new(home);

    match cli.command {
        None => {
            Cli::command().print_help()?;
            println!();
        }
        Some(Command::Doctor) => print_doctor(&runtime_env, &store)?,
        Some(Command::Create(args)) => {
            let profile = ProfileName::parse(args.name)?;
            match args.from {
                Some(harness) => {
                    let kind = harness.kind();
                    let integration = registry::all()
                        .into_iter()
                        .find(|integration| integration.kind() == kind)
                        .ok_or_else(|| anyhow::anyhow!("unsupported harness {harness}"))?;
                    match integration.detect(&runtime_env)? {
                        Detection::Detected { .. } => {}
                        Detection::NotDetected => {
                            anyhow::bail!("{harness} was not detected on PATH")
                        }
                    }
                    let paths = integration.paths(&runtime_env)?;
                    let path = store.create_skeleton(&profile)?;
                    if let Err(error) = integration
                        .import_from_harness(&paths)
                        .and_then(|imported| store.apply_import(&profile, kind.id(), imported))
                    {
                        let _ = std::fs::remove_dir_all(&path);
                        return Err(error.context(format!("failed to import from {harness}")));
                    }
                    println!(
                        "Created profile {profile} from {harness} at {}",
                        path.display()
                    );
                }
                None => {
                    let path = store.create_skeleton(&profile)?;
                    println!("Created profile {profile} at {}", path.display());
                }
            }
        }
        Some(Command::Show(args)) => {
            let profile = ProfileName::parse(args.name)?;
            let summary = store
                .summarize(&profile)
                .with_context(|| format!("failed to inspect profile {profile}"))?;
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
            if let Some(path) = store.edit_target(&args.name)?.execute()? {
                println!("{}", path.display());
            }
        }
        Some(Command::Delete(args)) => {
            if !args.yes && !confirm_delete(&args.name)? {
                println!("Delete cancelled");
                return Ok(());
            }
            let path = store.delete_profile(&args.name, &runtime_env)?;
            println!("Deleted profile {} at {}", args.name, path.display());
        }
        Some(Command::Use(args)) => {
            let profile = ProfileName::parse(args.profile.clone())?;
            args.validate()?;
            match args.target() {
                UseTarget::Harness(harness) => {
                    let kind = harness.kind();
                    let integration = registry::all()
                        .into_iter()
                        .find(|integration| integration.kind() == kind)
                        .ok_or_else(|| anyhow::anyhow!("unsupported harness {harness}"))?;
                    match integration.detect(&runtime_env)? {
                        Detection::Detected { .. } => {}
                        Detection::NotDetected => {
                            anyhow::bail!("{harness} was not detected on PATH")
                        }
                    }
                    let policy = match args.drift_policy() {
                        Some(p) => p,
                        None => {
                            let drift = crate::harness::apply::check_harness_drift(
                                integration.as_ref(),
                                &runtime_env,
                                &store,
                            )?;
                            if drift.is_clean() {
                                DriftPolicy::Discard
                            } else {
                                use std::io::{self, IsTerminal, Write};
                                if io::stdin().is_terminal() {
                                    println!(
                                        "Drift detected in {}",
                                        integration.kind().display_name()
                                    );
                                    loop {
                                        print!("Save changes [s], discard [d], or cancel [c]? ");
                                        io::stdout().flush()?;
                                        let mut input = String::new();
                                        io::stdin().read_line(&mut input)?;
                                        match input.trim().to_lowercase().as_str() {
                                            "s" | "save" => break DriftPolicy::SaveChanges,
                                            "d" | "discard" => break DriftPolicy::Discard,
                                            "c" | "cancel" => break DriftPolicy::Cancel,
                                            _ => println!("Please enter s, d, or c."),
                                        }
                                    }
                                } else {
                                    anyhow::bail!(
                                        "drift detected in {}; pass --save-changes or --discard-changes to proceed",
                                        integration.kind().display_name()
                                    );
                                }
                            }
                        }
                    };

                    let result =
                        use_profile(integration.as_ref(), &runtime_env, &store, &profile, policy)?;
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
                    let mut detected = Vec::new();
                    for integration in registry::all() {
                        if matches!(
                            integration.detect(&runtime_env)?,
                            Detection::Detected { .. }
                        ) {
                            detected.push(integration);
                        }
                    }
                    if detected.is_empty() {
                        anyhow::bail!("no supported harnesses detected");
                    }

                    let integrations: Vec<&dyn crate::harness::integration::HarnessIntegration> =
                        detected.iter().map(|i| i.as_ref()).collect();

                    let results = crate::harness::apply::use_profile_all(
                        &integrations,
                        &runtime_env,
                        &store,
                        &profile,
                        args.discard_changes,
                        |drifted_names| {
                            use std::io::{self, IsTerminal, Write};
                            if io::stdin().is_terminal() {
                                let names = drifted_names.join(", ");
                                println!("Drift detected in: {}", names);
                                print!("Proceed and discard changes? [y/N] ");
                                io::stdout().flush()?;
                                let mut input = String::new();
                                io::stdin().read_line(&mut input)?;
                                Ok(matches!(input.trim(), "y" | "Y" | "yes" | "YES"))
                            } else {
                                let names = drifted_names.join(", ");
                                anyhow::bail!(
                                    "drift detected in {}; pass --discard-changes to proceed",
                                    names
                                );
                            }
                        },
                    )?;

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

fn print_doctor(runtime_env: &RuntimeEnv, store: &ProfileStore) -> Result<()> {
    let rows = status_rows(runtime_env, store)?;
    print_harness_doctor(&rows);
    print_profile_doctor(store, &rows)?;
    Ok(())
}

fn print_harness_doctor(rows: &[StatusRow]) {
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

fn print_profile_doctor(store: &ProfileStore, rows: &[StatusRow]) -> Result<()> {
    let profiles = store.list_profiles()?;
    let mut lines = Vec::new();
    let mut drifted = 0usize;
    let mut errors = 0usize;

    for profile in profiles {
        let mut states = Vec::new();
        let mut clean = Vec::new();
        let mut drift = Vec::new();
        let mut error = Vec::new();

        for row in rows {
            let StatusProfile::Active {
                name,
                drift: drift_state,
                has_validation_errors,
            } = &row.profile
            else {
                continue;
            };
            if name != &profile.name {
                continue;
            }

            match drift_state {
                DriftState::Clean => clean.push(row.harness.id().to_string()),
                DriftState::Drifted => drift.push(row.harness.id().to_string()),
                DriftState::Error => error.push(row.harness.id().to_string()),
            }
            if *has_validation_errors && !error.contains(&row.harness.id().to_string()) {
                error.push(row.harness.id().to_string());
            }
        }

        drifted += drift.len();
        errors += error.len();

        let validation_error =
            profile_validation_error(store, &profile.name, &profile.config_status);
        if validation_error.is_some() {
            errors += 1;
        }

        if !clean.is_empty() {
            states.push(format!("used by {}", clean.join(", ")));
        }
        if !drift.is_empty() {
            states.push(format!("drifted by {}", drift.join(", ")));
        }
        if !error.is_empty() {
            states.push(format!("error: {}", error.join(", ")));
        }
        if let Some(error) = validation_error {
            states.push(format!("invalid: {error}"));
        }
        if states.is_empty() {
            states.push("unused".to_string());
        }

        lines.push(format!("  - {} ({})", profile.name, states.join(", ")));
    }

    let marker = if drifted == 0 && errors == 0 {
        "[✓]"
    } else {
        "[!]"
    };
    let mut summary = Vec::new();
    if drifted > 0 {
        summary.push(format!("{drifted} drifted"));
    }
    if errors > 0 {
        summary.push(format!("{errors} error"));
    }
    let suffix = if summary.is_empty() {
        "".to_string()
    } else {
        format!(" ({}):", summary.join(", "))
    };

    println!("{marker} Profiles{suffix}");
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

fn profile_validation_error(
    store: &ProfileStore,
    name: &ProfileName,
    config_status: &ProfileConfigStatus,
) -> Option<String> {
    match config_status {
        ProfileConfigStatus::Valid => {
            let path = store.profile_dir(name);
            let issues = crate::profile::validation::validate_profile(&path);
            issues
                .into_iter()
                .find(|issue| issue.severity == crate::profile::validation::Severity::Error)
                .map(|issue| issue.message)
        }
        ProfileConfigStatus::Missing => Some("missing config.json".to_string()),
        ProfileConfigStatus::Invalid(error) => Some(format!("config.json {error}")),
    }
}
