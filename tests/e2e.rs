use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

struct TestContext {
    temp: TempDir,
    home: PathBuf,
    user_home: PathBuf,
}

impl TestContext {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("lazyagents");
        let user_home = temp.path().join("user");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&user_home).unwrap();

        // Mock detected harnesses by creating dummy binaries in PATH
        let bin_dir = temp.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();

        // Create dummies for supported harnesses
        for bin in &["claude", "codex", "gemini", "opencode", "pi"] {
            let bin_path = bin_dir.join(bin);
            fs::write(&bin_path, "#!/bin/sh\necho hi").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&bin_path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        Self {
            temp,
            home,
            user_home,
        }
    }

    fn run_cli(&self, args: &[&str]) -> std::process::Output {
        self.command(args).output().unwrap()
    }

    fn command(&self, args: &[&str]) -> Command {
        let bin_path = env!("CARGO_BIN_EXE_lazyagents");

        let path_env = env::var_os("PATH").unwrap_or_default();
        let mut new_path = env::join_paths(vec![self.temp.path().join("bin")]).unwrap();
        new_path.push(":");
        new_path.push(&path_env);

        let mut command = Command::new(bin_path);
        command
            .args(args)
            .env("LAZYAGENTS_HOME", &self.home)
            .env("HOME", &self.user_home)
            .env("PATH", new_path)
            .env_remove("EDITOR");
        command
    }
}

#[test]
fn e2e_profile_switching_isolation() {
    let ctx = TestContext::new();

    // Create profile 'full'
    let out = ctx.run_cli(&["new", "full"]);
    assert!(out.status.success(), "new full failed: {:?}", out);

    // Create profile 'empty'
    let out = ctx.run_cli(&["new", "empty"]);
    assert!(out.status.success(), "new empty failed");

    // Add Skills, Commands, MCPs to 'full'
    let full_path = ctx.home.join("profiles/full");
    fs::create_dir_all(full_path.join("skills/writer")).unwrap();
    fs::write(full_path.join("skills/writer/SKILL.md"), "test").unwrap();

    fs::create_dir_all(full_path.join("commands")).unwrap();
    fs::write(full_path.join("commands/plan.md"), "plan").unwrap();

    fs::write(
        full_path.join("mcps.json"),
        r#"[{"name":"local","transport":"stdio","command":"server"}]"#,
    )
    .unwrap();

    // Use 'full' for Claude
    let out = ctx.run_cli(&["use", "full", "--harness", "claude"]);
    assert!(
        out.status.success(),
        "use full failed: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Verify surfaces
    let claude_dir = ctx.user_home.join(".claude");
    assert!(
        claude_dir.join("skills/writer/SKILL.md").exists(),
        "skill missing"
    );
    assert!(
        claude_dir.join("commands/plan.md").exists(),
        "command missing"
    );
    let mcps = fs::read_to_string(ctx.user_home.join(".claude.json")).unwrap_or_default();
    assert!(mcps.contains("local"), "mcp missing");

    // Switch to 'empty'
    let out = ctx.run_cli(&["use", "empty", "--harness", "claude", "--discard-changes"]);
    assert!(
        out.status.success(),
        "use empty failed: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Criterion 1: stale surfaces removed
    assert!(
        !claude_dir.join("skills/writer/SKILL.md").exists(),
        "stale skill remains"
    );
    assert!(
        !claude_dir.join("commands/plan.md").exists(),
        "stale command remains"
    );
    let mcps = fs::read_to_string(ctx.user_home.join(".claude.json")).unwrap_or_default();
    assert!(!mcps.contains("local"), "stale mcp remains");
}

#[test]
fn e2e_drift_handling() {
    let ctx = TestContext::new();

    // Create profiles
    ctx.run_cli(&["new", "work"]);
    ctx.run_cli(&["new", "home"]);

    // Apply 'work' to both claude and codex
    ctx.run_cli(&["use", "work", "--harness", "claude"]);
    ctx.run_cli(&["use", "work", "--harness", "codex"]);

    // Create drift in Claude
    let claude_dir = ctx.user_home.join(".claude");
    fs::create_dir_all(claude_dir.join("skills/drift-skill")).unwrap();
    fs::write(claude_dir.join("skills/drift-skill/SKILL.md"), "drifted").unwrap();

    // 1. Test --discard-changes
    let out = ctx.run_cli(&["use", "home", "--harness", "claude", "--discard-changes"]);
    assert!(out.status.success(), "use discard failed");

    // Verify drift was NOT imported into 'work'
    let work_path = ctx.home.join("profiles/work");
    assert!(
        !work_path.join("skills/drift-skill").exists(),
        "drift was incorrectly imported on discard"
    );

    // Re-apply 'work' and recreate drift to test --save-changes
    ctx.run_cli(&["use", "work", "--harness", "claude"]);
    fs::create_dir_all(claude_dir.join("skills/save-skill")).unwrap();
    fs::write(claude_dir.join("skills/save-skill/SKILL.md"), "drifted").unwrap();

    // 2. Test --save-changes
    let out = ctx.run_cli(&["use", "home", "--harness", "claude", "--save-changes"]);
    assert!(out.status.success(), "use save failed");

    // Verify drift WAS imported into 'work'
    assert!(
        work_path.join("skills/save-skill/SKILL.md").exists(),
        "drift was NOT imported on save"
    );

    // Verify Codex 'work' profile remains active for Codex in state, but Claude is 'home'
    let state = fs::read_to_string(ctx.home.join("state.json")).unwrap();
    assert!(state.contains(r#""claude": "home""#));
    assert!(state.contains(r#""codex": "work""#));
}

#[test]
fn e2e_new_from_imports_and_removes_shared_agent_skills() {
    let ctx = TestContext::new();
    let codex_dir = ctx.user_home.join(".codex");
    let shared_dir = ctx.user_home.join(".agents/skills");

    fs::create_dir_all(codex_dir.join("skills/native")).unwrap();
    fs::write(codex_dir.join("skills/native/SKILL.md"), "native").unwrap();
    fs::create_dir_all(codex_dir.join("skills/shared")).unwrap();
    fs::write(codex_dir.join("skills/shared/SKILL.md"), "native wins").unwrap();
    fs::create_dir_all(codex_dir.join("skills/.native-hidden")).unwrap();
    fs::write(
        codex_dir.join("skills/.native-hidden/SKILL.md"),
        "native hidden",
    )
    .unwrap();

    fs::create_dir_all(shared_dir.join("shared")).unwrap();
    fs::write(shared_dir.join("shared/SKILL.md"), "shared shadowed").unwrap();
    fs::create_dir_all(shared_dir.join("global")).unwrap();
    fs::write(shared_dir.join("global/SKILL.md"), "global").unwrap();
    fs::create_dir_all(shared_dir.join(".global-hidden")).unwrap();
    fs::write(shared_dir.join(".global-hidden/SKILL.md"), "hidden global").unwrap();
    fs::create_dir_all(shared_dir.join("broken")).unwrap();
    fs::write(shared_dir.join(".DS_Store"), "").unwrap();

    let out = ctx.run_cli(&["new", "imported", "-H", "codex"]);
    assert!(
        out.status.success(),
        "new --harness failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let profile = ctx.home.join("profiles/imported");
    assert_eq!(
        fs::read_to_string(profile.join("skills/native/SKILL.md")).unwrap(),
        "native"
    );
    assert_eq!(
        fs::read_to_string(profile.join("skills/shared/SKILL.md")).unwrap(),
        "native wins"
    );
    assert_eq!(
        fs::read_to_string(profile.join("skills/global/SKILL.md")).unwrap(),
        "global"
    );
    assert!(!profile.join("skills/.native-hidden/SKILL.md").exists());
    assert!(!profile.join("skills/.global-hidden/SKILL.md").exists());

    assert!(!shared_dir.join("global").exists());
    assert!(
        shared_dir.join("shared").exists(),
        "shadowed shared skill should be left alone"
    );
    assert!(codex_dir.join("skills/.native-hidden").exists());
    assert!(shared_dir.join(".global-hidden").exists());
    assert!(shared_dir.join("broken").exists());
    assert!(shared_dir.join(".DS_Store").exists());
}

#[cfg(unix)]
#[test]
fn e2e_shared_skill_cleanup_failure_keeps_committed_profile_and_source() {
    use std::os::unix::fs::PermissionsExt;

    let ctx = TestContext::new();
    let shared_dir = ctx.user_home.join(".agents/skills");
    fs::create_dir_all(shared_dir.join("global")).unwrap();
    fs::write(shared_dir.join("global/SKILL.md"), "global").unwrap();
    fs::set_permissions(&shared_dir, fs::Permissions::from_mode(0o555)).unwrap();

    let out = ctx.run_cli(&["new", "imported", "-H", "codex"]);

    fs::set_permissions(&shared_dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        out.status.success(),
        "profile import failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("do not retry"));
    assert_eq!(
        fs::read_to_string(shared_dir.join("global/SKILL.md")).unwrap(),
        "global"
    );
    assert_eq!(
        fs::read_to_string(ctx.home.join("profiles/imported/skills/global/SKILL.md")).unwrap(),
        "global"
    );
}

#[test]
fn e2e_shared_skill_process_exit_keeps_a_durable_profile_copy() {
    let ctx = TestContext::new();
    let shared = ctx.user_home.join(".agents/skills/global");
    fs::create_dir_all(&shared).unwrap();
    fs::write(shared.join("SKILL.md"), "global").unwrap();
    let second = ctx.user_home.join(".agents/skills/second");
    fs::create_dir_all(&second).unwrap();
    fs::write(second.join("SKILL.md"), "second").unwrap();

    let out = ctx
        .command(&["new", "imported", "-H", "codex"])
        .env("LAZYAGENTS_TEST_EXIT_AFTER_SHARED_SKILL_STAGE", "1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(86));
    assert_eq!(
        fs::read_to_string(ctx.home.join("profiles/imported/skills/global/SKILL.md")).unwrap(),
        "global"
    );
    assert!(!shared.exists());
    assert!(
        second.exists(),
        "the process must stop after the first staged skill"
    );
    assert!(fs::read_dir(ctx.user_home.join(".agents/skills"))
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with(".lazyagents-import-")));

    let retry = ctx.run_cli(&["new", "after-crash"]);
    assert!(
        retry.status.success(),
        "retry failed: {}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert!(!fs::read_dir(ctx.user_home.join(".agents/skills"))
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with(".lazyagents-import-")));
    assert!(!second.exists());
}

#[test]
fn e2e_show_and_doctor_lock_profile_rollback_recovery() {
    use fs2::FileExt;

    let ctx = TestContext::new();
    assert!(ctx.run_cli(&["new", "work"]).status.success());
    let profile = ctx.home.join("profiles/work");
    let rollback = ctx.home.join("profiles/.work-rollback-crash");
    fs::rename(&profile, &rollback).unwrap();
    fs::write(
        ctx.home.join("profiles/.work-transaction.json"),
        r#"{"rollback":".work-rollback-crash","phase":"prepared"}"#,
    )
    .unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(ctx.home.join(".lock"))
        .unwrap();
    lock.try_lock_exclusive().unwrap();

    for args in [&["show", "work"][..], &["doctor"][..]] {
        let output = ctx.run_cli(args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("another lazyagents command is already running"));
        assert!(!profile.exists());
        assert!(rollback.is_dir());
    }
    FileExt::unlock(&lock).unwrap();

    let output = ctx.run_cli(&["show", "work"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(profile.is_dir());
    assert!(!rollback.exists());
}

#[test]
fn e2e_delete_removes_stale_rollback_and_profile_cannot_resurrect() {
    let ctx = TestContext::new();
    assert!(ctx.run_cli(&["new", "work"]).status.success());
    let profile = ctx.home.join("profiles/work");
    let rollback = ctx.home.join("profiles/.work-rollback-stale");
    copy_dir_for_test(&profile, &rollback);
    fs::write(
        ctx.home.join("profiles/.work-transaction.json"),
        r#"{"rollback":".work-rollback-stale","phase":"committed"}"#,
    )
    .unwrap();

    let deleted = ctx.run_cli(&["delete", "work", "--yes"]);
    assert!(
        deleted.status.success(),
        "{}",
        String::from_utf8_lossy(&deleted.stderr)
    );
    assert!(!profile.exists());
    assert!(!rollback.exists());
    assert!(!ctx.home.join("profiles/.work-transaction.json").exists());
    assert!(!ctx.run_cli(&["show", "work"]).status.success());
    assert!(!profile.exists());
    assert!(ctx.run_cli(&["new", "work"]).status.success());
    assert!(ctx.run_cli(&["show", "work"]).status.success());
}

fn copy_dir_for_test(source: &std::path::Path, target: &std::path::Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let destination = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_for_test(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).unwrap();
        }
    }
}

#[test]
fn e2e_new_from_harness_keeps_instructions_empty_when_harness_file_is_empty() {
    let ctx = TestContext::new();
    let codex_dir = ctx.user_home.join(".codex");
    fs::create_dir_all(&codex_dir).unwrap();
    fs::write(codex_dir.join("AGENTS.md"), "").unwrap();

    let out = ctx.run_cli(&["new", "imported-empty", "-H", "codex"]);
    assert!(
        out.status.success(),
        "new --harness failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let profile = ctx.home.join("profiles/imported-empty");
    let text = fs::read_to_string(profile.join("PROFILE.md")).unwrap();
    let (_, instructions) = text.split_once("\n---\n").unwrap();
    assert!(instructions.is_empty());
}

#[test]
fn e2e_rollback_on_failure() {
    let ctx = TestContext::new();

    ctx.run_cli(&["new", "fail_profile"]);
    let full_path = ctx.home.join("profiles/fail_profile");
    fs::create_dir_all(full_path.join("skills/fail-skill")).unwrap();
    fs::write(full_path.join("skills/fail-skill/SKILL.md"), "test").unwrap();

    let claude_dir = ctx.user_home.join(".claude");
    // Ensure Claude config dir doesn't exist to test removing originally absent paths

    // To trigger an apply failure, we make the destination mcp file a directory
    fs::create_dir_all(ctx.user_home.join(".claude.json")).unwrap();

    // Create unrelated native config file
    fs::create_dir_all(&claude_dir).unwrap();
    fs::write(claude_dir.join("settings.json"), r#"{"unrelated": true}"#).unwrap();

    let out = ctx.run_cli(&["use", "fail_profile", "--harness", "claude"]);
    assert!(!out.status.success(), "use should have failed");

    // Verify rollback restored previous managed surfaces, meaning it removed the newly created `skills` directory and `CLAUDE.md`
    assert!(
        !claude_dir.join("skills/fail-skill").exists(),
        "rollback failed to remove newly created skill dir"
    );
    assert!(
        !claude_dir.join("CLAUDE.md").exists(),
        "rollback failed to remove newly created CLAUDE.md"
    );

    // Verify unrelated native config was preserved
    let settings = fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    assert_eq!(
        settings, r#"{"unrelated": true}"#,
        "unrelated native config was not preserved"
    );

    // Verify State was NOT updated
    let state = fs::read_to_string(ctx.home.join("state.json")).unwrap_or_default();
    assert!(
        !state.contains(r#""claude": "fail_profile""#),
        "state incorrectly updated despite apply failure"
    );
}

#[test]
fn e2e_doctor_replaces_list_and_status() {
    let ctx = TestContext::new();

    let empty = ctx.run_cli(&["doctor"]);
    assert!(empty.status.success(), "doctor failed: {:?}", empty);
    assert!(String::from_utf8_lossy(&empty.stdout).contains(
        "[✓] Profiles\n   No profiles yet. Create one with: lazyagents new <profile-name>\n"
    ));

    assert!(ctx.run_cli(&["new", "work"]).status.success());
    assert!(ctx.run_cli(&["new", "playground"]).status.success());
    assert!(ctx
        .run_cli(&["use", "work", "--harness", "codex"])
        .status
        .success());

    let out = ctx.run_cli(&["doctor"]);
    assert!(out.status.success(), "doctor failed: {:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines();
    assert_eq!(lines.next(), Some("Doctor summary:"));
    assert_eq!(lines.next(), Some("[✓] LazyAgents (0.1.0)"));
    assert!(stdout.contains("Harnesses ("));
    assert!(stdout.contains("- codex (work)"));
    assert!(stdout.contains("[✓] Profiles"));
    assert!(stdout.contains("- work (ready)"));
    assert!(stdout.contains("- playground (ready)"));
    assert!(!stdout.contains("HARNESS\tPROFILE"));
    assert!(!stdout.contains("PROFILE\tCONFIG"));

    let list = ctx.run_cli(&["list"]);
    assert!(!list.status.success(), "list should be removed");
    let status = ctx.run_cli(&["status"]);
    assert!(!status.status.success(), "status should be removed");
}

#[test]
fn e2e_unset_leaves_harness_files_unchanged() {
    let ctx = TestContext::new();

    assert!(ctx.run_cli(&["new", "work"]).status.success());
    assert!(ctx
        .run_cli(&["use", "work", "--harness", "codex"])
        .status
        .success());

    let codex_dir = ctx.user_home.join(".codex");
    fs::write(codex_dir.join("AGENTS.md"), "changed instructions").unwrap();
    fs::create_dir_all(codex_dir.join("skills/local")).unwrap();
    fs::write(codex_dir.join("skills/local/SKILL.md"), "local skill").unwrap();
    fs::remove_file(ctx.temp.path().join("bin/codex")).unwrap();

    let out = ctx.run_cli(&["unset", "-H", "codex"]);
    assert!(
        out.status.success(),
        "unset failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Deactivated profile work for Codex; harness files were left unchanged\n"
    );
    assert_eq!(
        fs::read_to_string(codex_dir.join("AGENTS.md")).unwrap(),
        "changed instructions"
    );
    assert_eq!(
        fs::read_to_string(codex_dir.join("skills/local/SKILL.md")).unwrap(),
        "local skill"
    );
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(ctx.home.join("state.json")).unwrap()).unwrap();
    assert!(state["active_profiles"].get("codex").is_none());

    let again = ctx.run_cli(&["unset", "-H", "codex"]);
    assert!(again.status.success());
    assert_eq!(
        String::from_utf8_lossy(&again.stdout),
        "No active profile for Codex; harness files were left unchanged\n"
    );
}

#[test]
fn e2e_unset_all_deactivates_every_profile() {
    let ctx = TestContext::new();

    assert!(ctx.run_cli(&["new", "work"]).status.success());
    for harness in ["codex", "claude"] {
        assert!(ctx
            .run_cli(&["use", "work", "--harness", harness])
            .status
            .success());
    }

    let out = ctx.run_cli(&["unset", "--all"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Deactivated profile work for Codex; harness files were left unchanged")
    );
    assert!(stdout
        .contains("Deactivated profile work for Claude Code; harness files were left unchanged"));

    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(ctx.home.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["active_profiles"], serde_json::json!({}));
}

#[test]
fn e2e_delete_rejects_invalid_targets_before_confirmation() {
    let ctx = TestContext::new();

    let missing = ctx.run_cli(&["delete", "missing"]);
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("profile missing does not exist"));

    assert!(ctx.run_cli(&["new", "work"]).status.success());
    assert!(ctx
        .run_cli(&["use", "work", "--harness", "codex"])
        .status
        .success());

    let active = ctx.run_cli(&["delete", "work"]);
    assert!(!active.status.success());
    assert!(active.stdout.is_empty());
    assert!(String::from_utf8_lossy(&active.stderr).contains("cannot delete active profile work"));
    assert!(ctx.home.join("profiles/work").is_dir());
}

#[test]
fn e2e_settings_edit_prints_and_creates_settings_without_editor() {
    let ctx = TestContext::new();

    let out = ctx.run_cli(&["settings", "edit"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}\n", ctx.home.join("settings.json").display())
    );
    assert!(ctx.home.join("settings.json").is_file());
}

#[test]
fn e2e_doctor_uses_defaults_without_creating_settings() {
    let ctx = TestContext::new();

    let out = ctx.run_cli(&["doctor"]);

    assert!(out.status.success());
    assert!(!ctx.home.join("settings.json").exists());
}

#[test]
fn e2e_shared_config_dir_instances_share_active_state() {
    let ctx = TestContext::new();

    assert!(ctx.run_cli(&["new", "work"]).status.success());
    assert!(ctx.run_cli(&["settings", "edit"]).status.success());

    let settings_path = ctx.home.join("settings.json");
    let mut settings: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    settings["harnesses"]["codex-work"] = serde_json::json!({
        "type": "codex",
        "displayName": "Codex Work",
        "binary": "codex",
        "configDir": "~/.codex"
    });
    fs::write(
        &settings_path,
        format!("{}\n", serde_json::to_string_pretty(&settings).unwrap()),
    )
    .unwrap();

    let out = ctx.run_cli(&["use", "work", "--harness", "codex"]);
    assert!(
        out.status.success(),
        "use failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Also marked codex-work active"));

    let state = fs::read_to_string(ctx.home.join("state.json")).unwrap();
    assert!(state.contains(r#""codex": "work""#));
    assert!(state.contains(r#""codex-work": "work""#));

    let doctor = ctx.run_cli(&["doctor"]);
    assert!(doctor.status.success());
    let stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(stdout.contains("- codex-work (work, shares configDir with codex)"));

    fs::write(ctx.user_home.join(".codex/AGENTS.md"), "leave me alone").unwrap();
    let unset = ctx.run_cli(&["unset", "-H", "codex-work"]);
    assert!(unset.status.success());
    let stdout = String::from_utf8_lossy(&unset.stdout);
    assert!(stdout
        .contains("Deactivated profile work for Codex Work; harness files were left unchanged"));
    assert!(stdout.contains("Also deactivated codex because they share configDir with codex-work"));
    let state = fs::read_to_string(ctx.home.join("state.json")).unwrap();
    assert!(!state.contains(r#""codex": "work""#));
    assert!(!state.contains(r#""codex-work": "work""#));
    assert_eq!(
        fs::read_to_string(ctx.user_home.join(".codex/AGENTS.md")).unwrap(),
        "leave me alone"
    );
}

#[test]
fn e2e_doctor_reports_alias_conflicts_without_aborting() {
    let ctx = TestContext::new();
    assert!(ctx.run_cli(&["new", "work"]).status.success());
    assert!(ctx.run_cli(&["new", "personal"]).status.success());
    assert!(ctx.run_cli(&["settings", "edit"]).status.success());
    let settings_path = ctx.home.join("settings.json");
    let mut settings: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    settings["harnesses"]["codex-max"] = serde_json::json!({
        "type": "codex",
        "configDir": "~/.codex",
        "binary": "codex"
    });
    fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();
    fs::write(
        ctx.home.join("state.json"),
        r#"{"active_profiles":{"codex":"work","codex-max":"personal"}}"#,
    )
    .unwrap();

    let output = ctx.run_cli(&["doctor"]);
    assert!(output.status.success(), "doctor failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("codex=work"));
    assert!(stdout.contains("codex-max=personal"));
    assert!(stdout.contains("conflicting active profiles"));
}

#[test]
fn e2e_settings_reset_requires_confirmation_and_restores_defaults() {
    let ctx = TestContext::new();

    assert!(ctx.run_cli(&["settings", "edit"]).status.success());
    let settings_path = ctx.home.join("settings.json");
    let mut settings: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    settings["harnesses"]["codex-work"] = serde_json::json!({
        "type": "codex",
        "displayName": "Codex Work",
        "binary": "codex",
        "configDir": "~/.codex-work"
    });
    fs::write(
        &settings_path,
        format!("{}\n", serde_json::to_string_pretty(&settings).unwrap()),
    )
    .unwrap();

    let cancelled = ctx.run_cli(&["settings", "reset"]);
    assert!(cancelled.status.success());
    let stdout = String::from_utf8_lossy(&cancelled.stdout);
    assert!(stdout.contains("Settings reset cancelled"));
    let settings: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(settings["harnesses"].get("codex-work").is_some());

    let reset = ctx.run_cli(&["settings", "reset", "--yes"]);
    assert!(
        reset.status.success(),
        "settings reset failed: {}",
        String::from_utf8_lossy(&reset.stderr)
    );
    let stdout = String::from_utf8_lossy(&reset.stdout);
    assert!(stdout.contains("Reset settings at"));

    let settings: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(settings["harnesses"].get("codex-work").is_none());
    assert_eq!(settings["harnesses"]["codex"]["configDir"], "~/.codex");
    assert_eq!(
        settings["harnesses"]["opencode"]["configDir"],
        "~/.config/opencode"
    );
}
