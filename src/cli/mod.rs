pub mod args;
pub mod render;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};
use std::io::{self, Write};
use std::process::Command as ProcessCommand;

use crate::app::create_profile::{create_profile, CreateProfileResult};
use crate::app::doctor::{
    doctor_report, DoctorReport, HarnessAvailability, HarnessProfileStatus, HarnessStatus,
    LazyagentsDoctorReport,
};
use crate::app::edit_profile::edit_profile_path;
use crate::app::harness_registry::{
    ensure_settings, reset_settings, settings_path, BuiltInHarnessRegistry, HarnessRegistry,
};
use crate::app::inspect_profile::inspect_profile;
use crate::app::state::{active_profile_for_aliases, LazyagentsHomeLock, LazyagentsState};
use crate::app::unset_profile::{unset_profile_workflow, UnsetProfileResult, UnsetProfileTarget};
use crate::app::use_profile::{
    use_profile_workflow, DriftDecision, HarnessDrift, UseProfileOutcome, UseProfileRequest,
    UseProfileTarget,
};
use crate::harness::apply::ProfileUseStatus;
use crate::harness::drift::DriftReport;
use crate::harness::integration::{AppEnvironment, HarnessIntegration, ProfileRef};
use crate::profile::{LazyagentsHome, ProfileName, ProfileStore};

use args::{Cli, Command, UnsetTarget, UseTarget};
use render::{
    mcp_summary_count, render_artifact_status, render_json_value, render_path, render_path_in_text,
    render_string_list, render_validation_issues,
};

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let home = LazyagentsHome::resolve()?;
    let ctx = CliContext {
        runtime_env: AppEnvironment::resolve(home.path().to_path_buf())?,
        home_path: home.path().to_path_buf(),
        store: ProfileStore::new(home),
        registry: BuiltInHarnessRegistry,
    };

    match cli.command {
        None => print_help()?,
        Some(Command::Doctor) => {
            let _lock = LazyagentsHomeLock::acquire(&ctx.home_path)?;
            print_doctor(&ctx.runtime_env, &ctx.store)?
        }
        Some(Command::Settings(args)) => run_settings(&ctx, args)?,
        Some(Command::New(args)) => run_new(&ctx, args)?,
        Some(Command::Show(args)) => run_show(&ctx, args)?,
        Some(Command::Edit(args)) => run_edit(&ctx, args)?,
        Some(Command::Delete(args)) => run_delete(&ctx, args)?,
        Some(Command::Use(args)) => run_use(&ctx, args)?,
        Some(Command::Unset(args)) => run_unset(&ctx, args)?,
    }

    Ok(())
}

struct CliContext {
    runtime_env: AppEnvironment,
    home_path: std::path::PathBuf,
    store: ProfileStore,
    registry: BuiltInHarnessRegistry,
}

fn acquire_mutation_lock(ctx: &CliContext) -> Result<LazyagentsHomeLock> {
    let lock = LazyagentsHomeLock::acquire(&ctx.home_path)?;
    crate::app::create_profile::recover_shared_skill_cleanup(&ctx.runtime_env)?;
    Ok(lock)
}

fn print_help() -> Result<()> {
    Cli::command().print_help()?;
    println!();
    Ok(())
}

fn run_settings(ctx: &CliContext, settings: args::SettingsArgs) -> Result<()> {
    match settings.command {
        args::SettingsCommand::Edit => {
            let _lock = acquire_mutation_lock(ctx)?;
            let path = ensure_settings(&ctx.runtime_env)?;
            if !open_editor_or_print_path(&path)? {
                println!("{}", render_path(&path));
            }
        }
        args::SettingsCommand::Reset(args) => {
            let path = settings_path(&ctx.runtime_env);
            if path.exists() && !args.yes && !confirm_settings_reset(&path)? {
                println!("Settings reset cancelled");
                return Ok(());
            }
            let _lock = acquire_mutation_lock(ctx)?;
            let path = reset_settings(&ctx.runtime_env)?;
            println!("Reset settings at {}", render_path(&path));
        }
    }
    Ok(())
}

fn run_new(ctx: &CliContext, args: args::NewArgs) -> Result<()> {
    let profile = ProfileName::parse(args.name)?;
    let harness = args
        .harness
        .as_deref()
        .map(|id| ctx.registry.require_id(&ctx.runtime_env, id))
        .transpose()?;
    let _lock = acquire_mutation_lock(ctx)?;
    match create_profile(
        &ctx.registry,
        &ctx.runtime_env,
        &ctx.store,
        profile,
        harness,
    )? {
        CreateProfileResult::Created { profile, path } => {
            println!("Created profile {profile} at {}", path.display());
        }
        CreateProfileResult::Imported {
            profile,
            harness,
            path,
            cleanup_warning,
        } => {
            println!(
                "Created profile {profile} from {harness} at {}",
                path.display()
            );
            if let Some(warning) = cleanup_warning {
                eprintln!("Warning: {warning}");
            }
        }
    }
    Ok(())
}

fn run_show(ctx: &CliContext, args: args::ProfileArg) -> Result<()> {
    let _lock = LazyagentsHomeLock::acquire(&ctx.home_path)?;
    let profile = ProfileName::parse(args.name)?;
    let summary = inspect_profile(&ctx.store, &profile)?;
    println!(
        "Profile:  {} ({})",
        summary
            .display_name
            .as_deref()
            .unwrap_or(summary.name.as_str()),
        summary.name
    );
    println!("Location: {}", render_path(&summary.path));
    if let Some(description) = summary
        .description
        .as_deref()
        .filter(|description| !description.is_empty())
    {
        println!("Description: {description}");
    }
    println!();
    println!("Resources:");
    println!(
        " Skills ({}): {}",
        summary.valid_skills.len(),
        render_resource_list(&summary.valid_skills)
    );
    println!(
        " Commands ({}): {}",
        summary.commands.len(),
        render_resource_list(&summary.commands)
    );
    println!(
        " Sub-agents ({}): {}",
        summary.agents.len(),
        render_resource_list(&summary.agents)
    );
    println!(
        " MCPs ({}): {}",
        mcp_summary_count(&summary.mcp_summary),
        render_mcp_resource_list(&summary.mcp_summary)
    );
    println!();
    println!("Preferences:");
    println!(
        " Model: {}",
        render_preferences(&ctx.registry, &ctx.runtime_env, &summary.models)?
    );
    println!(
        " Permission: {}",
        render_preferences(&ctx.registry, &ctx.runtime_env, &summary.permissions)?
    );
    println!();
    println!(
        "Instructions: {}",
        render_artifact_status(&summary.instruction_source)
    );

    let has_ignored = !summary.ignored_skills.is_empty()
        || !summary.ignored_command_files.is_empty()
        || !summary.ignored_agent_files.is_empty();
    if has_ignored || !summary.validation_issues.is_empty() {
        println!("\nIssues:");
        if !summary.ignored_skills.is_empty() {
            println!(
                "Ignored Skill files: {}",
                render_string_list(&summary.ignored_skills)
            );
        }
        if !summary.ignored_command_files.is_empty() {
            println!(
                "Ignored Command files: {}",
                render_string_list(&summary.ignored_command_files)
            );
        }
        if !summary.ignored_agent_files.is_empty() {
            println!(
                "Ignored Sub-agent files: {}",
                render_string_list(&summary.ignored_agent_files)
            );
        }
        let other_issues = summary
            .validation_issues
            .into_iter()
            .filter(|issue| !is_rendered_ignored_artifact_issue(issue))
            .collect::<Vec<_>>();
        if !other_issues.is_empty() {
            print!("{}", render_validation_issues(&other_issues));
        }
    }

    let drift_reports =
        profile_drift_reports(&ctx.registry, &ctx.runtime_env, &ctx.store, &profile)?;
    if !drift_reports.is_empty() {
        println!();
        for (index, (harness, drift)) in drift_reports.into_iter().enumerate() {
            if index > 0 {
                println!();
            }
            if index == 0 {
                println!("Changes:");
            }
            println!(" {harness}:");
            print_drift_report(&drift, "  ", None);
        }
    }
    Ok(())
}

fn run_edit(ctx: &CliContext, args: args::ProfileArg) -> Result<()> {
    let _lock = acquire_mutation_lock(ctx)?;
    let path = edit_profile_path(&ctx.store, &args.name)?;
    if !open_editor_or_print_path(&path)? {
        println!("{}", path.display());
    }
    Ok(())
}

fn run_delete(ctx: &CliContext, args: args::DeleteArgs) -> Result<()> {
    {
        let _lock = acquire_mutation_lock(ctx)?;
        crate::app::delete_profile::deletable_profile_path(
            &ctx.registry,
            &ctx.runtime_env,
            &ctx.store,
            &args.name,
        )?;
    }
    if !args.yes && !confirm_delete(&args.name)? {
        println!("Delete cancelled");
        return Ok(());
    }
    let _lock = acquire_mutation_lock(ctx)?;
    let path = crate::app::delete_profile::delete_profile(
        &ctx.registry,
        &ctx.runtime_env,
        &ctx.store,
        &args.name,
    )?;
    println!("Deleted profile {} at {}", args.name, path.display());
    Ok(())
}

fn run_use(ctx: &CliContext, args: args::UseArgs) -> Result<()> {
    let requested_profile = ProfileName::parse(args.profile.clone())?;
    args.validate()?;
    let _lock = acquire_mutation_lock(ctx)?;
    match args.target() {
        UseTarget::Harness(harness_id) => run_use_one(ctx, &args, requested_profile, harness_id),
        UseTarget::All => run_use_all(ctx, &args, requested_profile),
    }
}

fn run_use_one(
    ctx: &CliContext,
    args: &args::UseArgs,
    requested_profile: ProfileName,
    harness_id: String,
) -> Result<()> {
    let id = ctx.registry.require_id(&ctx.runtime_env, &harness_id)?;
    let outcome = use_profile_workflow(
        &ctx.registry,
        &ctx.runtime_env,
        &ctx.store,
        UseProfileRequest {
            profile: requested_profile.clone(),
            target: UseProfileTarget::Harness(id.clone()),
            drift_decision: args.drift_decision(),
        },
    )?;
    let result = match outcome {
        UseProfileOutcome::Applied(result) => result,
        UseProfileOutcome::NeedsSingleHarnessDriftDecision {
            display_name,
            profile: active_profile,
            drift,
        } => {
            let decision = prompt_single_drift_decision(&display_name, &active_profile, &drift)?;
            match use_profile_workflow(
                &ctx.registry,
                &ctx.runtime_env,
                &ctx.store,
                UseProfileRequest {
                    profile: requested_profile,
                    target: UseProfileTarget::Harness(id),
                    drift_decision: Some(decision),
                },
            )? {
                UseProfileOutcome::Applied(result) => result,
                _ => unreachable!("drift decision should complete single harness use"),
            }
        }
        _ => unreachable!("single harness request returned all-harness outcome"),
    };
    match result.status {
        ProfileUseStatus::Applied => {
            println!(
                "Used profile {} with {}",
                result.profile, result.display_name
            );
            if !result.alias_updates.is_empty() {
                println!(
                    "Also marked {} active because they share configDir with {}",
                    result.alias_updates.join(", "),
                    result.harness
                );
            }
            for warning in &result.warnings {
                eprintln!("Warning: {warning}");
            }
        }
        ProfileUseStatus::CancelledForDrift => {
            println!("Profile switch for {} cancelled", result.display_name);
        }
    }
    Ok(())
}

fn run_use_all(
    ctx: &CliContext,
    args: &args::UseArgs,
    requested_profile: ProfileName,
) -> Result<()> {
    let outcome = use_profile_workflow(
        &ctx.registry,
        &ctx.runtime_env,
        &ctx.store,
        UseProfileRequest {
            profile: requested_profile.clone(),
            target: UseProfileTarget::All,
            drift_decision: args.drift_decision(),
        },
    )?;
    let results = match outcome {
        UseProfileOutcome::All(results) => results,
        UseProfileOutcome::NeedsAllHarnessDriftDecision { harnesses } => {
            let names = harnesses
                .iter()
                .map(|drift| drift.display_name.as_str())
                .collect::<Vec<_>>();
            if !prompt_all_drift_discard(&harnesses)? {
                anyhow::bail!("Profile switch for {} cancelled", names.join(", "));
            }
            match use_profile_workflow(
                &ctx.registry,
                &ctx.runtime_env,
                &ctx.store,
                UseProfileRequest {
                    profile: requested_profile,
                    target: UseProfileTarget::All,
                    drift_decision: Some(DriftDecision::DiscardChanges),
                },
            )? {
                UseProfileOutcome::All(results) => results,
                _ => unreachable!("discard decision should complete all harness use"),
            }
        }
        _ => unreachable!("all-harness request returned single-harness outcome"),
    };

    let failure_count = results.failures.len();
    println!("\nSummary:");
    for result in results.applied {
        println!("✅ {} applied", result.display_name);
        for warning in result.warnings {
            println!("⚠️  {warning}");
        }
    }
    for (_harness, display_name, error) in results.failures {
        println!("❌ {display_name} failed: {error}");
    }
    if failure_count > 0 {
        anyhow::bail!("profile switch failed for {failure_count} harness(es)");
    }
    Ok(())
}

fn run_unset(ctx: &CliContext, args: args::UnsetArgs) -> Result<()> {
    args.validate()?;
    let (target, inactive_name) = match args.target() {
        UnsetTarget::Harness(id) => {
            let id = ctx.registry.require_id(&ctx.runtime_env, &id)?;
            let display_name = ctx
                .registry
                .get(&ctx.runtime_env, &id)?
                .expect("validated harness id must resolve")
                .display_name()
                .to_string();
            (UnsetProfileTarget::Harness(id), Some(display_name))
        }
        UnsetTarget::All => (UnsetProfileTarget::All, None),
    };
    let _lock = acquire_mutation_lock(ctx)?;
    let results = unset_profile_workflow(&ctx.registry, &ctx.runtime_env, target)?;
    if results.is_empty() {
        match inactive_name {
            Some(display_name) => {
                println!("No active profile for {display_name}; harness files were left unchanged")
            }
            None => println!("No active profiles; harness files were left unchanged"),
        }
    } else {
        for result in results {
            print_unset_result(&result);
        }
    }
    Ok(())
}

fn print_unset_result(result: &UnsetProfileResult) {
    println!(
        "Deactivated profile {} for {}; harness files were left unchanged",
        result.profile, result.display_name
    );
    if !result.alias_updates.is_empty() {
        println!(
            "Also deactivated {} because they share configDir with {}",
            result.alias_updates.join(", "),
            result.harness
        );
    }
}

fn render_resource_list(values: &[String]) -> String {
    if values.is_empty() {
        "—".to_string()
    } else {
        values.join(", ")
    }
}

fn render_mcp_resource_list(summary: &crate::profile::McpSummary) -> String {
    match summary {
        crate::profile::McpSummary::Empty => "—".to_string(),
        crate::profile::McpSummary::Servers(names) => render_resource_list(names),
        crate::profile::McpSummary::Invalid(error) => format!("invalid: {error}"),
    }
}

fn is_rendered_ignored_artifact_issue(issue: &crate::profile::validation::ValidationIssue) -> bool {
    use crate::profile::validation::ValidationCategory;

    matches!(
        (issue.category, issue.message.as_str()),
        (
            ValidationCategory::Skills,
            "ignored skill directory or missing SKILL.md"
        ) | (
            ValidationCategory::Commands,
            "ignored non-markdown command file"
        ) | (
            ValidationCategory::Subagents,
            "ignored non-markdown sub-agent file"
        )
    )
}

fn render_preferences(
    registry: &dyn HarnessRegistry,
    runtime_env: &AppEnvironment,
    values: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<String> {
    Ok(registry
        .all(runtime_env)?
        .into_iter()
        .map(|integration| {
            let id = integration.instance_id();
            let value = values
                .get(id)
                .map(render_json_value)
                .unwrap_or_else(|| "default".to_string());
            format!("{id}={value}")
        })
        .collect::<Vec<_>>()
        .join(" | "))
}

fn profile_drift_reports(
    registry: &dyn HarnessRegistry,
    runtime_env: &AppEnvironment,
    store: &ProfileStore,
    profile: &ProfileName,
) -> Result<Vec<(String, DriftReport)>> {
    let state = LazyagentsState::load(&runtime_env.lazyagents_home.join("state.json"))?;
    let mut reports = Vec::new();
    for integration in registry.all(runtime_env)? {
        let aliases = registry.aliases_for(runtime_env, integration.as_ref())?;
        if active_profile_for_aliases(&state, &aliases)?.as_ref() != Some(profile) {
            continue;
        }
        if !matches!(
            integration.detect(runtime_env)?,
            crate::harness::integration::HarnessDetection::Detected { .. }
        ) {
            continue;
        }
        let active = active_profile_for_show(integration.as_ref(), store, profile)?;
        let paths = integration.paths(runtime_env)?;
        let drift = integration.detect_drift(&active, &paths)?;
        if !drift.is_clean() {
            reports.push((integration.display_name().to_string(), drift));
        }
    }
    Ok(reports)
}

fn active_profile_for_show(
    integration: &dyn HarnessIntegration,
    store: &ProfileStore,
    name: &ProfileName,
) -> Result<ProfileRef> {
    let path = store.profile_dir(name);
    if !path.is_dir() {
        anyhow::bail!("active profile {name} is missing at {}", path.display());
    }
    store.load_config(name)?;
    if integration.supports_mcp() {
        crate::profile::mcp::read_mcp_definitions(&path)?;
    }
    if integration.supports_subagents() {
        crate::harness::agents::profile_agents(&path)?;
    }
    Ok(ProfileRef {
        name: name.clone(),
        path,
        harness_id: integration.instance_id().to_string(),
    })
}

fn print_doctor(runtime_env: &AppEnvironment, store: &ProfileStore) -> Result<()> {
    let registry = BuiltInHarnessRegistry;
    let report = doctor_report(&registry, runtime_env, store)?;
    println!("Doctor summary:");
    print_lazyagents_doctor(&report.lazyagents);
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

fn confirm_settings_reset(path: &std::path::Path) -> Result<bool> {
    print!(
        "Reset settings at {} to defaults? This removes custom harness instances. [y/N] ",
        render_path(path)
    );
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim(), "y" | "Y" | "yes" | "YES"))
}

fn prompt_single_drift_decision(
    harness: &str,
    profile: &ProfileName,
    drift: &DriftReport,
) -> Result<DriftDecision> {
    use std::io::IsTerminal;

    if !io::stdin().is_terminal() {
        anyhow::bail!(
            "{harness} has changes in current profile {profile}; pass --save-changes or --discard-changes to proceed"
        );
    }

    println!("{harness} has changes in current profile {profile}:");
    print_drift_report(drift, "  ", Some(10));
    println!();
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

fn prompt_all_drift_discard(harnesses: &[HarnessDrift]) -> Result<bool> {
    use std::io::IsTerminal;

    let joined = harnesses
        .iter()
        .map(|drift| drift.display_name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if !io::stdin().is_terminal() {
        anyhow::bail!(
            "Changes in current profiles detected in {joined}; pass --discard-changes to proceed"
        );
    }

    println!("Resolve changes in current profiles:");
    for harness in harnesses {
        println!();
        println!(
            "  {} has changes for profile {}:",
            harness.display_name, harness.profile
        );
        print_drift_report(&harness.drift, "    ", Some(10));
    }
    println!();
    print!("Proceed and discard changes? [y/N] ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim(), "y" | "Y" | "yes" | "YES"))
}

fn print_drift_report(drift: &DriftReport, indent: &str, limit: Option<usize>) {
    let visible = limit.unwrap_or(drift.items.len());

    for item in drift.items.iter().take(visible) {
        println!(
            "{indent}{} {}: {}",
            drift_marker(&item.detail),
            item.surface,
            render_path_in_text(&item.detail)
        );
    }

    let remaining = drift.items.len().saturating_sub(visible);
    if remaining > 0 {
        println!(
            "{indent}... and {remaining} more ({})",
            drift_marker_counts(&drift.items[visible..])
        );
    }
}

fn drift_marker_counts(items: &[crate::harness::drift::DriftItem]) -> String {
    let mut additions = 0usize;
    let mut removals = 0usize;
    let mut changes = 0usize;
    for item in items {
        match drift_marker(&item.detail) {
            '+' => additions += 1,
            '-' => removals += 1,
            _ => changes += 1,
        }
    }
    format!("+{additions}, -{removals}, ~{changes}")
}

fn drift_marker(detail: &str) -> char {
    if detail.contains("unexpected harness entry") {
        '+'
    } else if detail.contains(" is missing") {
        '-'
    } else {
        '~'
    }
}

fn print_lazyagents_doctor(report: &LazyagentsDoctorReport) {
    let suffix = if report.summary.is_empty() {
        "".to_string()
    } else {
        format!(" ({}):", report.summary.join(", "))
    };

    println!(
        "{} LazyAgents ({}){suffix}",
        report.marker,
        env!("CARGO_PKG_VERSION")
    );
    for line in &report.lines {
        println!("{line}");
    }
}

fn print_harness_doctor(rows: &[HarnessStatus]) {
    if rows.is_empty() {
        println!("[x] Harnesses (0 available)");
        return;
    }

    let available = rows
        .iter()
        .filter(|row| matches!(row.availability, HarnessAvailability::Available))
        .count();
    let unavailable = rows.len() - available;
    let marker = if unavailable == 0 { "[✓]" } else { "[!]" };
    if unavailable == 0 {
        println!("{marker} Harnesses ({available} available):");
    } else {
        println!("{marker} Harnesses ({available} available, {unavailable} unavailable):");
    }

    for row in rows {
        println!("  - {} ({})", row.harness, render_harness_status(row));
    }
}

fn render_harness_status(row: &HarnessStatus) -> String {
    match row.availability {
        HarnessAvailability::BinaryMissing => {
            format!("unavailable: binary not found: {}", row.binary)
        }
        HarnessAvailability::ConfigDirMissing => {
            format!(
                "unavailable: configDir missing {}",
                render_path(&row.config_dir)
            )
        }
        HarnessAvailability::Available => {
            let mut parts = Vec::new();
            match &row.profile {
                HarnessProfileStatus::Inactive => parts.push("no active profile".to_string()),
                HarnessProfileStatus::Active { name, .. } => parts.push(name.to_string()),
            }
            if let Some(shared) = &row.shared_config_with {
                parts.push(format!("shares configDir with {shared}"));
            }
            parts.join(", ")
        }
    }
}

fn print_profile_doctor(report: &DoctorReport) {
    let suffix = if report.profiles.summary.is_empty() {
        "".to_string()
    } else {
        format!(" ({}):", report.profiles.summary.join(", "))
    };

    println!("{} Profiles{suffix}", report.profiles.marker);
    if report.profiles.lines.is_empty() {
        println!("   No profiles yet. Create one with: lazyagents new <profile-name>");
    }
    for line in &report.profiles.lines {
        println!("{line}");
    }
}
