#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn output(command: &str, args: &[&str]) -> String {
    String::from_utf8(Command::new(command).args(args).output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_string()
}

#[test]
fn installer_is_executable() {
    let installer = concat!(env!("CARGO_MANIFEST_DIR"), "/install.sh");
    assert_ne!(
        fs::metadata(installer).unwrap().permissions().mode() & 0o111,
        0
    );
}

#[test]
fn installer_resolves_relative_destination_from_callers_directory() {
    let temp = tempfile::tempdir().unwrap();
    let caller = temp.path().join("caller");
    let fixtures = temp.path().join("fixtures");
    let fake_bin = temp.path().join("fake-bin");
    fs::create_dir_all(&caller).unwrap();
    fs::create_dir_all(&fixtures).unwrap();
    fs::create_dir_all(&fake_bin).unwrap();

    let os = match output("uname", &["-s"]).as_str() {
        "Darwin" => "apple-darwin",
        "Linux" => "unknown-linux-gnu",
        other => panic!("unsupported test OS {other}"),
    };
    let arch = match output("uname", &["-m"]).as_str() {
        "x86_64" | "amd64" => "x86_64",
        "arm64" | "aarch64" => "aarch64",
        other => panic!("unsupported test architecture {other}"),
    };
    let target = format!("{arch}-{os}");
    let package = fixtures.join(format!("lazyagents-{target}"));
    fs::create_dir_all(&package).unwrap();
    fs::write(package.join("lazyagents"), "binary").unwrap();
    let asset = format!("lazyagents-{target}.tar.gz");
    assert!(Command::new("tar")
        .current_dir(&fixtures)
        .args(["-czf", &asset, &format!("lazyagents-{target}")])
        .status()
        .unwrap()
        .success());
    let asset_path = fixtures.join(&asset);
    let checksum = if Command::new("sha256sum").arg(&asset_path).output().is_ok() {
        output("sha256sum", &[asset_path.to_str().unwrap()])
    } else {
        output("shasum", &["-a", "256", asset_path.to_str().unwrap()])
    };
    fs::write(
        fixtures.join("checksums.txt"),
        format!("{}  {asset}\n", checksum.split_whitespace().next().unwrap()),
    )
    .unwrap();

    let curl = fake_bin.join("curl");
    fs::write(
        &curl,
        r#"#!/bin/sh
for arg do
  if [ "$previous" = "-o" ]; then output=$arg; fi
  case "$arg" in http*) url=$arg ;; esac
  previous=$arg
done
cp "$INSTALL_FIXTURE_DIR/${url##*/}" "$output"
"#,
    )
    .unwrap();
    fs::set_permissions(&curl, fs::Permissions::from_mode(0o755)).unwrap();

    let path = format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap());
    let result = Command::new("sh")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/install.sh"))
        .current_dir(&caller)
        .env("PATH", path)
        .env("INSTALL_FIXTURE_DIR", &fixtures)
        .env("LAZYAGENTS_INSTALL_DIR", "bin")
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let installed = caller.join("bin/lazyagents");
    assert_eq!(fs::read_to_string(&installed).unwrap(), "binary");
    assert_ne!(
        fs::metadata(installed).unwrap().permissions().mode() & 0o111,
        0
    );
}

#[test]
fn installer_rejects_an_existing_file_destination_before_download() {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("not-a-directory");
    fs::write(&destination, "keep").unwrap();

    let result = Command::new("sh")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/install.sh"))
        .current_dir(temp.path())
        .env("LAZYAGENTS_INSTALL_DIR", "not-a-directory")
        .output()
        .unwrap();

    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("is not a directory"));
    assert_eq!(fs::read_to_string(destination).unwrap(), "keep");
}
