//! Stages `editor_code`'s vendored Monaco build beside the editor binary
//! and embeds a versioned source SDK for new Rust projects.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn collect_files(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if kind.is_dir() {
            collect_files(root, &path, files)?;
        } else if kind.is_file() && path.strip_prefix(root).is_ok() {
            files.push(path);
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    stage_monaco();
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = vec![workspace.join("Cargo.toml"), workspace.join("Cargo.lock")];
    collect_files(&workspace, &workspace.join("crates"), &mut files)?;
    files.sort();

    let mut source = String::from("const SDK_FILES: &[(&str, &[u8])] = &[\n");
    let mut fingerprint = 0xcbf29ce484222325_u64;
    for file in files {
        let relative = file
            .strip_prefix(&workspace)?
            .to_string_lossy()
            .replace('\\', "/");
        for byte in relative.as_bytes().iter().chain(fs::read(&file)?.iter()) {
            fingerprint = (fingerprint ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
        }
        source.push_str(&format!(
            "    ({relative:?}, include_bytes!({:?})),\n",
            file.to_string_lossy()
        ));
    }
    source.push_str("];\n");
    source.push_str(&format!(
        "const SDK_VERSION: &str = {:?};\n",
        format!("{}-{fingerprint:016x}", env!("CARGO_PKG_VERSION"))
    ));
    fs::write(
        Path::new(&std::env::var("OUT_DIR")?).join("sdk_files.rs"),
        source,
    )?;
    println!("cargo:rerun-if-changed=../Cargo.toml");
    println!("cargo:rerun-if-changed=../Cargo.lock");
    println!("cargo:rerun-if-changed=../crates");
    Ok(())
}

fn stage_monaco() {
    let Some(manifest) = env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from) else {
        return;
    };
    let vendor = manifest.join("../crates/editor_code/vendor/monaco");
    println!("cargo:rerun-if-changed={}", vendor.display());

    if !vendor.join("index.html").is_file() {
        println!(
            "cargo:warning=editor_code vendor/monaco not found at {}; a packaged editor will need EDITOR_CODE_MONACO_DIR",
            vendor.display()
        );
        return;
    }

    let Some(out_dir) = env::var_os("OUT_DIR").map(PathBuf::from) else {
        return;
    };
    // OUT_DIR = <target>/<profile>/build/<pkg>-<hash>/out — the binary
    // sits three levels up, in <target>/<profile>.
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        return;
    };
    let dest = profile_dir.join("monaco");

    if let Err(err) = copy_dir(&vendor, &dest) {
        println!(
            "cargo:warning=failed to stage vendor/monaco into {}: {err}",
            dest.display()
        );
    }
}

/// Recursively copies `src` into `dst`, overwriting existing files.
fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}
