//! Stages `editor_code`'s vendored Monaco build next to the `editor`
//! binary (`target/<profile>/monaco`), so a shipped executable plus that
//! folder runs with no source tree. `editor_code::monaco_dir` looks
//! there at runtime (see that crate's "Packaging" docs).
//!
//! Best-effort: any problem is a `cargo:warning`, not a build failure —
//! `cargo run` still finds the vendor dir via its crate path.

use std::path::{Path, PathBuf};
use std::{env, fs};

fn main() {
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
