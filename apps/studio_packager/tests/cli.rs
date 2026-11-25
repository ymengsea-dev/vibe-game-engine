//! Exercises the package command and its refusal to overwrite user data.

use std::fs;
use std::process::Command;

#[test]
fn command_packages_built_binaries_with_vendored_monaco() {
    let root = std::env::temp_dir().join(format!(
        "vge-studio-package-cli-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let binaries = root.join("binaries");
    let luals = root.join("luals");
    let output = root.join("distribution");
    fs::create_dir_all(&binaries).expect("binaries");
    fs::create_dir_all(luals.join("bin")).expect("LuaLS");
    fs::write(luals.join("main.lua"), b"return true").expect("LuaLS main");
    fs::write(luals.join("LICENSE"), b"MIT License").expect("LuaLS license");
    fs::write(
        luals.join(format!(
            "bin/lua-language-server{}",
            std::env::consts::EXE_SUFFIX
        )),
        b"LuaLS binary",
    )
    .expect("LuaLS binary");
    fs::write(
        binaries.join(format!("editor{}", std::env::consts::EXE_SUFFIX)),
        b"editor",
    )
    .expect("editor");
    fs::write(
        binaries.join(format!("player{}", std::env::consts::EXE_SUFFIX)),
        b"player",
    )
    .expect("player");

    let status = Command::new(env!("CARGO_BIN_EXE_studio_packager"))
        .args([&binaries, &output, &luals])
        .status()
        .expect("packager process");
    assert!(status.success());
    let app = if cfg!(target_os = "macos") {
        output.join("RustyEngine Studio.app/Contents")
    } else {
        output.clone()
    };
    let executables = if cfg!(target_os = "macos") {
        app.join("MacOS")
    } else {
        app.clone()
    };
    let resources = if cfg!(target_os = "macos") {
        app.join("Resources")
    } else {
        app
    };
    assert!(
        executables
            .join(format!("player{}", std::env::consts::EXE_SUFFIX))
            .is_file()
    );
    assert!(resources.join("monaco/index.html").is_file());
    assert!(resources.join("lua-language-server/main.lua").is_file());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn command_refuses_to_replace_an_existing_distribution() {
    let root = std::env::temp_dir().join(format!(
        "vge-studio-package-existing-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let binaries = root.join("binaries");
    let output = root.join("distribution");
    fs::create_dir_all(&binaries).expect("binaries");
    fs::create_dir(&output).expect("existing output");
    fs::write(output.join("keep.txt"), b"do not replace").expect("sentinel");
    for name in ["editor", "player"] {
        fs::write(
            binaries.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)),
            name.as_bytes(),
        )
        .expect("binary");
    }

    let status = Command::new(env!("CARGO_BIN_EXE_studio_packager"))
        .args([&binaries, &output])
        .status()
        .expect("packager process");
    assert!(!status.success());
    assert_eq!(
        fs::read(output.join("keep.txt")).expect("sentinel"),
        b"do not replace"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn dry_run_prints_the_signing_and_archive_commands_without_running_them() {
    let root = std::env::temp_dir().join(format!(
        "vge-studio-package-dry-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let binaries = root.join("binaries");
    let luals = root.join("luals");
    let output = root.join("Studio");
    fs::create_dir_all(&binaries).expect("binaries");
    fs::create_dir_all(luals.join("bin")).expect("LuaLS");
    fs::write(luals.join("main.lua"), b"return true").expect("LuaLS main");
    fs::write(luals.join("LICENSE"), b"MIT License").expect("LuaLS license");
    let suffix = std::env::consts::EXE_SUFFIX;
    fs::write(
        luals.join(format!("bin/lua-language-server{suffix}")),
        b"LuaLS",
    )
    .expect("LuaLS");
    fs::write(binaries.join(format!("editor{suffix}")), b"editor").expect("editor");
    fs::write(binaries.join(format!("player{suffix}")), b"player").expect("player");

    let result = Command::new(env!("CARGO_BIN_EXE_studio_packager"))
        .arg(&binaries)
        .arg("--archive")
        .arg(&output)
        .arg(&luals)
        .arg("--dry-run")
        .output()
        .expect("packager process");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let printed = String::from_utf8_lossy(&result.stdout);
    let (tool, archive) = match std::env::consts::OS {
        "macos" => ("hdiutil create", "Studio.dmg"),
        "windows" => ("tar -acf", "Studio.zip"),
        _ => ("tar -zcf", "Studio.tar.gz"),
    };
    assert!(printed.contains(tool), "{printed}");
    assert!(printed.contains(archive), "{printed}");
    assert!(!root.join(archive).exists(), "a dry run makes no archive");
    assert!(output.is_dir(), "the package itself is staged");

    let refused = Command::new(env!("CARGO_BIN_EXE_studio_packager"))
        .arg(&binaries)
        .arg(root.join("Other"))
        .arg(&luals)
        .args(["--notarize-profile", "studio"])
        .output()
        .expect("packager process");
    assert!(
        !refused.status.success(),
        "notarizing needs signing and an archive"
    );
    assert!(!root.join("Other").exists(), "refused before staging");
    let _ = fs::remove_dir_all(root);
}
