//! Local macOS app wrapper so `cargo run` has a real Dock identity.

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const BUNDLE_NAME: &str = "RustyEngine Studio.app";
const BUNDLED_MARKER: &str = "VGE_BUNDLED_LAUNCH";
const ORIGINAL_CWD: &str = "VGE_ORIGINAL_CWD";
const ICON: &[u8] = include_bytes!("../assets/app_logo.icns");
const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleDevelopmentRegion</key><string>en</string>
<key>CFBundleExecutable</key><string>editor</string>
<key>CFBundleIconFile</key><string>app_logo.icns</string>
<key>CFBundleIdentifier</key><string>dev.vge.RustyEngineStudio</string>
<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
<key>CFBundleName</key><string>RustyEngine Studio</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
"#;

fn bundled_executable(path: &Path) -> bool {
    let components: Vec<_> = path.components().collect();
    components.windows(3).any(|parts| {
        matches!(parts, [Component::Normal(bundle), Component::Normal(contents), Component::Normal(macos)]
            if bundle.to_string_lossy().ends_with(".app") && contents == &OsStr::new("Contents") && macos == &OsStr::new("MacOS"))
    })
}

fn bundle_path(binary: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let parent = binary
        .parent()
        .ok_or("editor binary has no parent directory")?;
    Ok(parent.join(BUNDLE_NAME))
}

fn ensure_directory(path: &Path) -> Result<(), Box<dyn Error>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(format!("bundle path is not a directory: {}", path.display()).into()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}

fn ensure_managed_file(
    path: &Path,
    bytes: &[u8],
    previous: Option<&[u8]>,
) -> Result<(), Box<dyn Error>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            let current = fs::read(path)?;
            if current == bytes {
                Ok(())
            } else if previous.is_some_and(|old| current == old) {
                fs::write(path, bytes)?;
                Ok(())
            } else {
                Err(format!(
                    "refusing to replace an unexpected bundle file: {}",
                    path.display()
                )
                .into())
            }
        }
        Ok(_) => Err(format!(
            "refusing to replace an unexpected bundle file: {}",
            path.display()
        )
        .into()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::write(path, bytes)?;
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}

fn stage_bundle(binary: &Path) -> Result<PathBuf, Box<dyn Error>> {
    if !binary.is_file() {
        return Err(format!("editor executable is missing: {}", binary.display()).into());
    }
    let bundle = bundle_path(binary)?;
    let contents = bundle.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    for path in [&bundle, &contents, &macos, &resources] {
        ensure_directory(path)?;
    }
    let previous_plist = PLIST.replace("app_logo.icns", "icon.icns");
    ensure_managed_file(
        &contents.join("Info.plist"),
        PLIST.as_bytes(),
        Some(previous_plist.as_bytes()),
    )?;
    ensure_managed_file(&resources.join("app_logo.icns"), ICON, None)?;
    let old_icon = resources.join("icon.icns");
    match fs::symlink_metadata(&old_icon) {
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && fs::read(&old_icon)? == ICON =>
        {
            fs::remove_file(&old_icon)?;
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }

    let linked = macos.join("editor");
    let marker = resources.join("editor.link-id");
    let previous_link = match fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            Some(fs::read_to_string(&marker)?)
        }
        Ok(_) => {
            return Err(format!(
                "refusing to replace an unexpected bundle marker: {}",
                marker.display()
            )
            .into());
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err.into()),
    };
    let source = fs::metadata(binary)?;
    match fs::symlink_metadata(&linked) {
        Ok(metadata) if metadata.file_type().is_symlink() && fs::read_link(&linked)? == binary => {
            fs::remove_file(&linked)?;
            fs::hard_link(binary, &linked)?;
        }
        Ok(metadata)
            if metadata.is_file()
                && metadata.dev() == source.dev()
                && metadata.ino() == source.ino() => {}
        Ok(metadata)
            if metadata.is_file()
                && previous_link.as_deref()
                    == Some(&format!("{}:{}", metadata.dev(), metadata.ino())) =>
        {
            fs::remove_file(&linked)?;
            fs::hard_link(binary, &linked)?;
        }
        Ok(_) => {
            return Err(format!(
                "refusing to replace an unexpected bundle executable: {}",
                linked.display()
            )
            .into());
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => fs::hard_link(binary, &linked)?,
        Err(err) => return Err(err.into()),
    }
    let linked_metadata = fs::metadata(&linked)?;
    fs::write(
        &marker,
        format!("{}:{}", linked_metadata.dev(), linked_metadata.ino()),
    )?;
    Ok(bundle)
}

fn launch_options(cwd: &Path, args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut cwd_var = OsString::from(format!("{ORIGINAL_CWD}="));
    cwd_var.push(cwd.as_os_str());
    let mut options = vec![
        OsString::from("-W"),
        OsString::from("-n"),
        OsString::from("-a"),
        OsString::from("--env"),
        OsString::from(format!("{BUNDLED_MARKER}=1")),
        OsString::from("--env"),
        cwd_var,
        OsString::from("--args"),
    ];
    options.extend(args);
    options
}

/// Stage and launch the local bundle if this is a raw Cargo executable.
/// Returns true after the bundled app exits, so the raw process can return.
pub fn relaunch_from_cargo_if_needed() -> Result<bool, Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    if bundled_executable(&executable) || std::env::var_os(BUNDLED_MARKER).is_some() {
        if let Some(original) = std::env::var_os(ORIGINAL_CWD) {
            std::env::set_current_dir(PathBuf::from(original))?;
        }
        return Ok(false);
    }
    let cwd = std::env::current_dir()?;
    let bundle = stage_bundle(&executable)?;
    let registration = Command::new("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister")
        .arg("-f")
        .arg(&bundle)
        .status();
    if !registration.is_ok_and(|status| status.success()) {
        eprintln!("warning: could not refresh the editor's macOS Dock icon registration");
    }
    let mut command = Command::new("open");
    let options = launch_options(&cwd, std::env::args_os().skip(1));
    command.args(&options[..3]).arg(&bundle).args(&options[3..]);
    let status = command.status()?;
    if !status.success() {
        return Err(format!("macOS could not launch {}: {status}", bundle.display()).into());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_context_keeps_spaces_and_relative_project_path() {
        let cwd = Path::new("/tmp/project workspace");
        let args = vec![OsString::from("my game/project")];
        let options = launch_options(cwd, args.clone());
        assert_eq!(
            options[6],
            OsString::from("VGE_ORIGINAL_CWD=/tmp/project workspace")
        );
        assert_eq!(options[8], args[0]);
    }

    #[test]
    fn bundled_executable_detection_does_not_relaunch_direct_open() {
        assert!(bundled_executable(Path::new(
            "/tmp/debug/Studio.app/Contents/MacOS/editor"
        )));
        assert!(!bundled_executable(Path::new("/tmp/debug/editor")));
    }

    #[test]
    fn bundle_stays_under_the_binary_profile_directory() {
        assert_eq!(
            bundle_path(Path::new("/tmp/target/debug/editor")).expect("path"),
            PathBuf::from("/tmp/target/debug/RustyEngine Studio.app")
        );
    }

    #[test]
    fn existing_regular_executable_is_never_replaced() {
        let root = std::env::temp_dir().join(format!(
            "vge_bundle_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir(&root).expect("temp root");
        let binary = root.join("editor");
        fs::write(&binary, b"binary").expect("binary");
        let executable = root.join(BUNDLE_NAME).join("Contents/MacOS/editor");
        fs::create_dir_all(executable.parent().expect("parent")).expect("bundle dirs");
        fs::write(&executable, b"other app").expect("collision");
        assert!(stage_bundle(&binary).is_err());
        assert_eq!(
            fs::read(&executable).expect("collision intact"),
            b"other app"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn managed_hardlink_refreshes_after_a_rebuild() {
        let root = std::env::temp_dir().join(format!(
            "vge_bundle_rebuild_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir(&root).expect("temp root");
        let binary = root.join("editor");
        fs::write(&binary, b"old binary").expect("binary");
        let bundle = stage_bundle(&binary).expect("first stage");
        let replacement = root.join("next-editor");
        fs::write(&replacement, b"new binary").expect("replacement");
        fs::rename(&replacement, &binary).expect("simulate cargo rebuild");
        stage_bundle(&binary).expect("refresh managed bundle");
        assert_eq!(
            fs::read(bundle.join("Contents/MacOS/editor")).expect("linked binary"),
            b"new binary"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn existing_bundle_switches_to_the_new_icon_resource() {
        let root = std::env::temp_dir().join(format!(
            "vge_bundle_logo_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir(&root).expect("temp root");
        let binary = root.join("editor");
        fs::write(&binary, b"binary").expect("binary");
        let bundle = stage_bundle(&binary).expect("initial bundle");
        let resources = bundle.join("Contents/Resources");
        let old_icon = resources.join("icon.icns");
        let new_icon = resources.join("app_logo.icns");
        let legacy_plist = PLIST.replace("app_logo.icns", "icon.icns");
        fs::write(bundle.join("Contents/Info.plist"), legacy_plist).expect("old plist");
        fs::write(&old_icon, ICON).expect("old resource name");
        if new_icon.exists() {
            fs::remove_file(&new_icon).expect("remove new resource for migration");
        }
        stage_bundle(&binary).expect("refresh bundle icon");
        let installed = fs::read(&new_icon).expect("new icon resource");
        let plist = fs::read_to_string(bundle.join("Contents/Info.plist")).expect("updated plist");
        let old_icon_remains = old_icon.exists();
        let _ = fs::remove_dir_all(root);
        assert!(
            installed == include_bytes!("../assets/app_logo.icns"),
            "staged icon does not match app_logo.icns"
        );
        assert!(plist.contains("<string>app_logo.icns</string>"));
        assert!(!old_icon_remains, "managed old icon resource remains");
    }

    #[test]
    fn unused_custom_icon_is_preserved_and_symlink_marker_is_rejected() {
        let root = std::env::temp_dir().join(format!(
            "vge_bundle_collision_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir(&root).expect("temp root");
        let binary = root.join("editor");
        fs::write(&binary, b"binary").expect("binary");
        let resources = root.join(BUNDLE_NAME).join("Contents/Resources");
        fs::create_dir_all(&resources).expect("resources");
        let icon = resources.join("icon.icns");
        fs::write(&icon, b"other icon").expect("icon");
        stage_bundle(&binary).expect("custom legacy resource is unused");
        assert_eq!(fs::read(&icon).expect("icon intact"), b"other icon");

        fs::remove_file(resources.join("editor.link-id")).expect("remove managed marker");
        let outside = root.join("outside");
        std::os::unix::fs::symlink(&outside, resources.join("editor.link-id"))
            .expect("symlink marker");
        assert!(stage_bundle(&binary).is_err());
        assert!(!outside.exists());
        let _ = fs::remove_dir_all(root);
    }
}
