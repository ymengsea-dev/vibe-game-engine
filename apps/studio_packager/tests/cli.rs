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
