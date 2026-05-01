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
        for bin in &["claude", "codex", "opencode", "pi"] {
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
        let bin_path = env!("CARGO_BIN_EXE_lazyagents");

        let path_env = env::var_os("PATH").unwrap_or_default();
        let mut new_path = env::join_paths(vec![self.temp.path().join("bin")]).unwrap();
        new_path.push(":");
        new_path.push(&path_env);

        Command::new(bin_path)
            .args(args)
            .env("LAZYAGENTS_HOME", &self.home)
            .env("HOME", &self.user_home)
            .env("PATH", new_path)
            .output()
            .unwrap()
    }
}

#[test]
fn e2e_profile_switching_isolation() {
    let ctx = TestContext::new();

    // Create profile 'full'
    let out = ctx.run_cli(&["create", "full"]);
    assert!(out.status.success(), "create full failed: {:?}", out);

    // Create profile 'empty'
    let out = ctx.run_cli(&["create", "empty"]);
    assert!(out.status.success(), "create empty failed");

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
    ctx.run_cli(&["create", "work"]);
    ctx.run_cli(&["create", "home"]);

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
fn e2e_create_from_imports_and_removes_shared_agent_skills() {
    let ctx = TestContext::new();
    let codex_dir = ctx.user_home.join(".codex");
    let shared_dir = ctx.user_home.join(".agents/skills");

    fs::create_dir_all(codex_dir.join("skills/native")).unwrap();
    fs::write(codex_dir.join("skills/native/SKILL.md"), "native").unwrap();
    fs::create_dir_all(codex_dir.join("skills/shared")).unwrap();
    fs::write(codex_dir.join("skills/shared/SKILL.md"), "native wins").unwrap();

    fs::create_dir_all(shared_dir.join("shared")).unwrap();
    fs::write(shared_dir.join("shared/SKILL.md"), "shared shadowed").unwrap();
    fs::create_dir_all(shared_dir.join("global")).unwrap();
    fs::write(shared_dir.join("global/SKILL.md"), "global").unwrap();
    fs::create_dir_all(shared_dir.join("broken")).unwrap();
    fs::write(shared_dir.join(".DS_Store"), "").unwrap();

    let out = ctx.run_cli(&["create", "imported", "--from", "codex"]);
    assert!(
        out.status.success(),
        "create --from failed: {}",
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

    assert!(!shared_dir.join("global").exists());
    assert!(
        shared_dir.join("shared").exists(),
        "shadowed shared skill should be left alone"
    );
    assert!(shared_dir.join("broken").exists());
    assert!(shared_dir.join(".DS_Store").exists());
}

#[test]
fn e2e_rollback_on_failure() {
    let ctx = TestContext::new();

    ctx.run_cli(&["create", "fail_profile"]);
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

    assert!(ctx.run_cli(&["create", "work"]).status.success());
    assert!(ctx.run_cli(&["create", "playground"]).status.success());
    assert!(ctx
        .run_cli(&["use", "work", "--harness", "codex"])
        .status
        .success());

    let out = ctx.run_cli(&["doctor"]);
    assert!(out.status.success(), "doctor failed: {:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[✓] Harnesses (4 available: codex, claude, opencode, pi)"));
    assert!(stdout.contains("[✓] Profiles"));
    assert!(stdout.contains("- work (used by codex)"));
    assert!(stdout.contains("- playground (unused)"));
    assert!(!stdout.contains("HARNESS\tPROFILE"));
    assert!(!stdout.contains("PROFILE\tCONFIG"));

    let list = ctx.run_cli(&["list"]);
    assert!(!list.status.success(), "list should be removed");
    let status = ctx.run_cli(&["status"]);
    assert!(!status.status.success(), "status should be removed");
}
