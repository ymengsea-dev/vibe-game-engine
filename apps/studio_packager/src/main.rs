//! Packages the editor, Lua player, and code editor resources together.

use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};

use sha2::{Digest, Sha256};

const LUALS_VERSION: &str = "3.19.2";

fn luals_release() -> Result<(&'static str, &'static str, &'static str), String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok((
            "darwin-arm64",
            "tar.gz",
            "79149584320a23bd5fcd5bc31f47a1975acd8a4560a0e89036bb1016fa61687f",
        )),
        ("macos", "x86_64") => Ok((
            "darwin-x64",
            "tar.gz",
            "6f2174ece5ec63bcd9d5b22b2c902445c1cd9045fafe48c62602448ebf4c09cc",
        )),
        ("linux", "aarch64") => Ok((
            "linux-arm64",
            "tar.gz",
            "17b8e401a874440adeb649e5024c364f2bb9d15156c74a40e56d1ae9a1b2e4f2",
        )),
        ("linux", "x86_64") => Ok((
            "linux-x64",
            "tar.gz",
            "93ad326b10f6c00911dd8c450fa098332dbd8f9f45d1052f5636a2d54479b947",
        )),
        ("windows", "x86_64") => Ok((
            "win32-x64",
            "zip",
            "4a3dc851b3b31e7b75a23f645d1ba4a2137579a55f5fa4edbb17a3e2a6254044",
        )),
        ("windows", "x86") => Ok((
            "win32-ia32",
            "zip",
            "0f6a771fdfba39cdfa9953c0cec1d920f05ee1b9f248392d27524a135e22737a",
        )),
        (os, arch) => Err(format!(
            "LuaLS {LUALS_VERSION} has no pinned release for {os}/{arch}"
        )),
    }
}

fn verify_archive(path: &Path, expected_sha256: &str) -> Result<(), String> {
    let bytes = fs::read(path).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual == expected_sha256 {
        Ok(())
    } else {
        Err(format!(
            "LuaLS archive checksum mismatch: {}",
            path.display()
        ))
    }
}

fn download_luals(destination: &Path) -> Result<(), String> {
    let (platform, extension, sha256) = luals_release()?;
    let archive = destination.join(format!("lua-language-server.{extension}"));
    let url = format!(
        "https://github.com/LuaLS/lua-language-server/releases/download/{LUALS_VERSION}/lua-language-server-{LUALS_VERSION}-{platform}.{extension}"
    );
    let status = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--output",
        ])
        .arg(&archive)
        .arg(&url)
        .status()
        .map_err(|err| format!("cannot download LuaLS with curl: {err}"))?;
    if !status.success() {
        return Err(format!("LuaLS download failed: {url}"));
    }
    verify_archive(&archive, sha256)?;
    let extracted = destination.join("extracted");
    fs::create_dir(&extracted)
        .map_err(|err| format!("cannot create {}: {err}", extracted.display()))?;
    let status = Command::new("tar")
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(&extracted)
        .status()
        .map_err(|err| format!("cannot extract LuaLS with tar: {err}"))?;
    if !status.success() {
        return Err("LuaLS archive extraction failed".into());
    }
    Ok(())
}

fn regular_file(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(format!("expected a regular file: {}", path.display())),
        Err(err) => Err(format!("cannot read {}: {err}", path.display())),
    }
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), String> {
    fs::copy(source, destination).map(|_| ()).map_err(|err| {
        format!(
            "cannot copy {} to {}: {err}",
            source.display(),
            destination.display()
        )
    })
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir(destination)
        .map_err(|err| format!("cannot create {}: {err}", destination.display()))?;
    for entry in
        fs::read_dir(source).map_err(|err| format!("cannot read {}: {err}", source.display()))?
    {
        let entry = entry.map_err(|err| format!("cannot read {}: {err}", source.display()))?;
        let kind = entry
            .file_type()
            .map_err(|err| format!("cannot inspect {}: {err}", entry.path().display()))?;
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            copy_file(&entry.path(), &target)?;
        } else {
            return Err(format!(
                "unsupported packaged resource: {}",
                entry.path().display()
            ));
        }
    }
    Ok(())
}

fn validate_output_location(input: &Path, output: &Path) -> Result<(), String> {
    let source = fs::canonicalize(input)
        .map_err(|err| format!("cannot resolve {}: {err}", input.display()))?;
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent)
        .map_err(|err| format!("cannot resolve {}: {err}", parent.display()))?;
    if parent.starts_with(source) {
        return Err(format!(
            "output directory cannot be inside source: {}",
            input.display()
        ));
    }
    Ok(())
}

/// Refuses a binary holding the builder's home folder path, which Cargo
/// compiles in for dependencies unless the build remaps it.
fn refuse_home_path(binary: &Path, home: &str) -> Result<(), String> {
    if home.is_empty() {
        return Ok(());
    }
    let bytes =
        fs::read(binary).map_err(|err| format!("cannot read {}: {err}", binary.display()))?;
    if bytes
        .windows(home.len())
        .any(|part| part == home.as_bytes())
    {
        return Err(format!(
            "{} contains the home folder path {home}. Rebuild with: RUSTFLAGS=\"--remap-path-prefix=$HOME=/build\" cargo build -p editor -p player --release",
            binary.display()
        ));
    }
    Ok(())
}

fn stage_distribution(
    binaries: &Path,
    monaco: &Path,
    luals: &Path,
    output: &Path,
) -> Result<(), String> {
    let suffix = std::env::consts::EXE_SUFFIX;
    let editor = binaries.join(format!("editor{suffix}"));
    let player = binaries.join(format!("player{suffix}"));
    regular_file(&editor)?;
    regular_file(&player)?;
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    refuse_home_path(&editor, &home)?;
    refuse_home_path(&player, &home)?;
    regular_file(&monaco.join("index.html"))?;
    regular_file(&luals.join("main.lua"))?;
    regular_file(&luals.join("LICENSE"))?;
    regular_file(&luals.join(format!("bin/lua-language-server{suffix}")))?;
    validate_output_location(monaco, output)?;
    validate_output_location(luals, output)?;

    fs::create_dir(output).map_err(|err| format!("cannot create {}: {err}", output.display()))?;
    let (executables, resources) = if cfg!(target_os = "macos") {
        let contents = output.join("RustyEngine Studio.app/Contents");
        (contents.join("MacOS"), contents.join("Resources"))
    } else {
        (output.to_path_buf(), output.to_path_buf())
    };
    if executables != output {
        fs::create_dir_all(&executables)
            .map_err(|err| format!("cannot create {}: {err}", executables.display()))?;
        fs::create_dir_all(&resources)
            .map_err(|err| format!("cannot create {}: {err}", resources.display()))?;
        let contents = executables
            .parent()
            .ok_or("app bundle has no Contents directory")?;
        fs::write(
            contents.join("Info.plist"),
            include_str!("../../../editor/assets/Info.plist"),
        )
        .map_err(|err| format!("cannot write app manifest: {err}"))?;
        fs::write(
            resources.join("app_logo.icns"),
            include_bytes!("../../../editor/assets/app_logo.icns"),
        )
        .map_err(|err| format!("cannot write app icon: {err}"))?;
    }
    copy_file(&editor, &executables.join(format!("editor{suffix}")))?;
    copy_file(&player, &executables.join(format!("player{suffix}")))?;
    copy_tree(monaco, &resources.join("monaco"))?;
    copy_tree(luals, &resources.join("lua-language-server"))?;
    Ok(())
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let usage = "usage: studio_packager <built-binaries-directory> <new-output-directory> [extracted-luals-directory]";
    let result = (|| {
        let binaries = args.next().ok_or(usage)?;
        let output = args.next().ok_or(usage)?;
        let supplied_luals = args.next();
        if args.next().is_some() {
            return Err(usage.to_owned());
        }
        if fs::symlink_metadata(&output).is_ok() {
            return Err(format!(
                "refusing to replace existing output: {}",
                Path::new(&output).display()
            ));
        }
        let monaco =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/editor_code/vendor/monaco");
        if let Some(luals) = supplied_luals {
            return stage_distribution(
                Path::new(&binaries),
                &monaco,
                Path::new(&luals),
                Path::new(&output),
            );
        }
        let temporary = std::env::temp_dir().join(format!(
            "vge-luals-package-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|err| format!("cannot read system clock: {err}"))?
                .as_nanos()
        ));
        fs::create_dir(&temporary)
            .map_err(|err| format!("cannot create {}: {err}", temporary.display()))?;
        let staged = (|| {
            download_luals(&temporary)?;
            stage_distribution(
                Path::new(&binaries),
                &monaco,
                &temporary.join("extracted"),
                Path::new(&output),
            )
        })();
        if let Err(err) = fs::remove_dir_all(&temporary) {
            eprintln!("warning: cannot remove {}: {err}", temporary.display());
        }
        staged?;
        Ok::<(), String>(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{stage_distribution, verify_archive};

    #[test]
    fn output_inside_monaco_source_is_rejected_before_copying() {
        let root = std::env::temp_dir().join(format!(
            "vge-studio-package-recursion-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let monaco = root.join("monaco");
        fs::create_dir_all(&monaco).expect("monaco");
        let output = monaco.join("distribution");

        assert!(super::validate_output_location(&monaco, &output).is_err());
        assert!(!output.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn output_inside_luals_source_is_rejected_before_copying() {
        let root = std::env::temp_dir().join(format!(
            "vge-luals-package-recursion-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let luals = root.join("lua-language-server");
        let binaries = root.join("binaries");
        let monaco = root.join("monaco");
        fs::create_dir_all(luals.join("bin")).expect("LuaLS source");
        fs::create_dir_all(&binaries).expect("binaries");
        fs::create_dir_all(&monaco).expect("Monaco");
        fs::write(luals.join("main.lua"), b"return true").expect("LuaLS main");
        fs::write(luals.join("LICENSE"), b"MIT").expect("LuaLS license");
        fs::write(
            luals.join(format!(
                "bin/lua-language-server{}",
                std::env::consts::EXE_SUFFIX
            )),
            b"LuaLS",
        )
        .expect("LuaLS executable");
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
        fs::write(monaco.join("index.html"), b"Monaco").expect("Monaco index");
        let output = luals.join("distribution");

        assert!(stage_distribution(&binaries, &monaco, &luals, &output).is_err());
        assert!(!output.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_simple_relative_output_name_uses_the_current_directory() {
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/editor_code/vendor/monaco");
        assert!(super::validate_output_location(&source, Path::new("studio-dist")).is_ok());
    }

    #[test]
    fn a_binary_with_the_home_path_is_refused() {
        let binary =
            std::env::temp_dir().join(format!("vge-studio-home-path-{}", std::process::id()));
        fs::write(&binary, b"panic at /Users/someone/.cargo/registry/x.rs").expect("binary");
        let error = super::refuse_home_path(&binary, "/Users/someone").expect_err("refused");
        assert!(error.contains("--remap-path-prefix"), "{error}");
        super::refuse_home_path(&binary, "/Users/other").expect("other home");
        super::refuse_home_path(&binary, "").expect("no home known");
        fs::remove_file(&binary).expect("cleanup");
    }

    #[test]
    fn packaged_studio_contains_player_and_monaco_beside_the_editor() {
        let root = std::env::temp_dir().join(format!(
            "vge-studio-package-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let binaries = root.join("binaries");
        let monaco = root.join("monaco");
        let luals = root.join("lua-language-server");
        let output = root.join("dist");
        fs::create_dir_all(&binaries).expect("binaries");
        fs::create_dir_all(&monaco).expect("monaco");
        fs::create_dir_all(luals.join("bin")).expect("LuaLS");
        fs::write(luals.join("main.lua"), b"return true").expect("LuaLS main script");
        fs::write(luals.join("LICENSE"), b"MIT License").expect("LuaLS license");
        fs::write(
            luals.join(format!(
                "bin/lua-language-server{}",
                std::env::consts::EXE_SUFFIX
            )),
            b"LuaLS executable",
        )
        .expect("LuaLS executable");
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
        fs::write(monaco.join("index.html"), b"Monaco").expect("index");
        fs::create_dir_all(monaco.join("vs/editor")).expect("nested Monaco");
        fs::write(monaco.join("vs/editor/editor.main.js"), b"editor").expect("script");
        // A build folder can hold other workspace binaries and game files.
        fs::write(
            binaries.join(format!("island{}", std::env::consts::EXE_SUFFIX)),
            b"game",
        )
        .expect("game binary");
        fs::create_dir_all(binaries.join("games/island")).expect("game folder");
        fs::write(binaries.join("games/island/project.ron"), b"()").expect("game project");

        stage_distribution(&binaries, &monaco, &luals, &output).expect("package Studio");

        fn staged_paths(dir: &Path, found: &mut Vec<String>) {
            for entry in fs::read_dir(dir).expect("staged folder") {
                let path = entry.expect("entry").path();
                found.push(path.to_string_lossy().into_owned());
                if path.is_dir() {
                    staged_paths(&path, found);
                }
            }
        }
        let mut staged = Vec::new();
        staged_paths(&output, &mut staged);
        let prefix = output.to_string_lossy().into_owned();
        assert!(
            !staged.iter().any(|path| {
                let path = &path[prefix.len()..];
                path.contains("games") || path.contains("island")
            }),
            "a game was packaged: {staged:?}"
        );

        let app = if cfg!(target_os = "macos") {
            output.join("RustyEngine Studio.app/Contents")
        } else {
            output.clone()
        };
        let executable_dir = if cfg!(target_os = "macos") {
            app.join("MacOS")
        } else {
            app.clone()
        };
        let resources = if cfg!(target_os = "macos") {
            app.join("Resources")
        } else {
            app
        };
        assert_eq!(
            fs::read(executable_dir.join(format!("editor{}", std::env::consts::EXE_SUFFIX)))
                .expect("packaged editor"),
            b"editor"
        );
        assert_eq!(
            fs::read(executable_dir.join(format!("player{}", std::env::consts::EXE_SUFFIX)))
                .expect("packaged player"),
            b"player"
        );
        assert_eq!(
            fs::read(resources.join(Path::new("monaco/index.html"))).expect("packaged Monaco"),
            b"Monaco"
        );
        assert_eq!(
            fs::read(resources.join("monaco/vs/editor/editor.main.js")).expect("packaged script"),
            b"editor"
        );
        assert_eq!(
            fs::read(resources.join("lua-language-server/main.lua")).expect("packaged LuaLS"),
            b"return true"
        );
        assert_eq!(
            fs::read(resources.join(format!(
                "lua-language-server/bin/lua-language-server{}",
                std::env::consts::EXE_SUFFIX
            )))
            .expect("packaged LuaLS executable"),
            b"LuaLS executable"
        );
        assert_eq!(
            fs::read(resources.join("lua-language-server/LICENSE")).expect("LuaLS license"),
            b"MIT License"
        );
        if cfg!(target_os = "macos") {
            let plist =
                fs::read_to_string(output.join("RustyEngine Studio.app/Contents/Info.plist"))
                    .expect("app manifest");
            assert!(plist.contains("<string>editor</string>"));
            assert!(resources.join("app_logo.icns").is_file());
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn archive_checksum_must_match_before_extraction() {
        let root = std::env::temp_dir().join(format!(
            "vge-luals-checksum-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir(&root).expect("temp directory");
        let archive = root.join("archive");
        fs::write(&archive, b"abc").expect("archive bytes");
        assert!(
            verify_archive(
                &archive,
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            )
            .is_ok()
        );
        assert!(verify_archive(&archive, "00000000").is_err());
        let _ = fs::remove_dir_all(root);
    }
}
