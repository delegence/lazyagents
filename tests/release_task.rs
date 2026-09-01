#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

struct ReleaseFixture {
    temp: tempfile::TempDir,
    script: String,
}

impl ReleaseFixture {
    fn new() -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mise = fs::read_to_string(root.join("mise.toml")).unwrap();
        let script = mise
            .split("run = '''\n")
            .nth(1)
            .unwrap()
            .split("\n'''\n")
            .next()
            .unwrap()
            .to_string();
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        fs::create_dir(&bin).unwrap();
        write_executable(
            &bin.join("git"),
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$RELEASE_GIT_LOG"
printf 'git %s\n' "$*" >> "$RELEASE_ALL_LOG"
case "$1 $2" in
  "branch --show-current") printf '%s\n' "${RELEASE_BRANCH:-main}" ;;
  "status --porcelain=v1") [ -n "${RELEASE_DIRTY:-}" ] && echo dirty ;;
  "fetch --tags") [ -n "${RELEASE_FETCH_FAIL:-}" ] && exit 1 || exit 0 ;;
  "rev-parse HEAD") echo local ;;
  "rev-parse refs/remotes/origin/main") [ -n "${RELEASE_STALE:-}" ] && echo remote || echo local ;;
  "rev-parse --verify") [ -n "${RELEASE_LOCAL_TAG:-}" ] && exit 0 || exit 1 ;;
  "ls-remote --exit-code") exit "${RELEASE_REMOTE_STATUS:-2}" ;;
  "diff --quiet") [ -n "${RELEASE_VERSION_CHANGED:-}" ] && exit 1 || exit 0 ;;
  *) exit 0 ;;
esac
"#,
        );
        write_executable(
            &bin.join("cargo"),
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$RELEASE_CARGO_LOG"
printf 'cargo %s\n' "$*" >> "$RELEASE_ALL_LOG"
case "$1" in
  metadata) case "$usage_version" in *..*) exit 1 ;; esac; printf '{"version":"%s"}\n' "$usage_version" ;;
  fmt) [ "${RELEASE_FAIL_STAGE:-}" = fmt ] && exit 1 || exit 0 ;;
  clippy) [ "${RELEASE_FAIL_STAGE:-}" = clippy ] && exit 1 || exit 0 ;;
  test) [ "${RELEASE_FAIL_STAGE:-}" = test ] && exit 1 || exit 0 ;;
esac
"#,
        );
        write_executable(
            &bin.join("perl"),
            "#!/bin/sh\ntouch \"$RELEASE_EDIT_MARKER\"\n",
        );
        Self { temp, script }
    }

    fn run(&self, version: &str, envs: &[(&str, &str)]) -> Output {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let bin = self.temp.path().join("bin");
        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg(&self.script)
            .current_dir(root)
            .env("usage_version", version)
            .env("RELEASE_GIT_LOG", self.git_log())
            .env("RELEASE_CARGO_LOG", self.cargo_log())
            .env("RELEASE_EDIT_MARKER", self.edit_marker())
            .env("RELEASE_ALL_LOG", self.all_log())
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            );
        for (key, value) in envs {
            command.env(key, value);
        }
        command.output().unwrap()
    }

    fn git_log(&self) -> std::path::PathBuf {
        self.temp.path().join("git.log")
    }
    fn cargo_log(&self) -> std::path::PathBuf {
        self.temp.path().join("cargo.log")
    }
    fn edit_marker(&self) -> std::path::PathBuf {
        self.temp.path().join("edited")
    }
    fn all_log(&self) -> std::path::PathBuf {
        self.temp.path().join("all.log")
    }
    fn log(&self) -> String {
        fs::read_to_string(self.git_log()).unwrap_or_default()
    }
}

fn write_executable(path: &std::path::Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn release_guards_stop_before_the_version_edit() {
    let cases: &[(&str, &[(&str, &str)])] = &[
        ("1.2.3-..", &[]),
        ("1.2.3", &[("RELEASE_BRANCH", "feature")]),
        ("1.2.3", &[("RELEASE_DIRTY", "1")]),
        ("1.2.3", &[("RELEASE_FETCH_FAIL", "1")]),
        ("1.2.3", &[("RELEASE_STALE", "1")]),
        ("1.2.3", &[("RELEASE_LOCAL_TAG", "1")]),
        ("1.2.3", &[("RELEASE_REMOTE_STATUS", "0")]),
        ("1.2.3", &[("RELEASE_REMOTE_STATUS", "128")]),
    ];
    for (version, envs) in cases {
        let fixture = ReleaseFixture::new();
        let output = fixture.run(version, envs);
        assert!(!output.status.success(), "guard passed for {envs:?}");
        assert!(!fixture.edit_marker().exists(), "edit ran for {envs:?}");
    }
}

#[test]
fn release_validation_failures_do_not_publish() {
    for stage in ["fmt", "clippy", "test"] {
        let fixture = ReleaseFixture::new();
        let output = fixture.run(
            "1.2.3",
            &[
                ("RELEASE_FAIL_STAGE", stage),
                ("RELEASE_VERSION_CHANGED", "1"),
            ],
        );
        assert!(!output.status.success());
        let log = fixture.log();
        assert!(!log.lines().any(|line| line.starts_with("commit ")));
        assert!(!log.lines().any(|line| line.starts_with("tag ")));
        assert!(!log.lines().any(|line| line.starts_with("push ")));
    }
}

#[test]
fn release_commits_version_files_and_pushes_atomically() {
    let fixture = ReleaseFixture::new();
    let output = fixture.run("1.2.3", &[("RELEASE_VERSION_CHANGED", "1")]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = fixture.log();
    assert!(log.contains("status --porcelain=v1 --untracked-files=all --ignore-submodules=none"));
    assert!(log.contains("fetch --tags origin +refs/heads/main:refs/remotes/origin/main"));
    assert!(log.contains("commit -m chore(release): v1.2.3 -- Cargo.toml Cargo.lock"));
    assert!(log.contains("tag -a v1.2.3 -m v1.2.3"));
    let pushes = log
        .lines()
        .filter(|line| line.starts_with("push "))
        .collect::<Vec<_>>();
    assert_eq!(
        pushes,
        vec!["push --atomic origin HEAD:main refs/tags/v1.2.3"]
    );
    let cargo = fs::read_to_string(fixture.cargo_log()).unwrap();
    let checks = cargo
        .lines()
        .filter(|line| {
            matches!(
                line.split_whitespace().next(),
                Some("fmt" | "clippy" | "test")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        checks,
        vec![
            "fmt --all -- --check",
            "clippy --all-targets --all-features --locked -- -D warnings",
            "test --all-targets --locked",
        ]
    );
    let all = fs::read_to_string(fixture.all_log()).unwrap();
    let test_position = all.find("cargo test --all-targets --locked").unwrap();
    let commit_position = all.find("git commit -m chore(release): v1.2.3").unwrap();
    assert!(test_position < commit_position);
}
