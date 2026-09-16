//! Local, bounded crash reports for the editor.

use std::path::{Path, PathBuf};

const MAX_REPORT_BYTES: usize = 64 * 1024;

/// Installs a panic hook that writes a bounded local report and then invokes
/// the process's previous hook. The hook never panics or performs network I/O.
pub fn install_report_hook(project_root: &Path) {
    let path = project_root.join(".studio").join("crash.log");
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut report = format!(
            "RustyEngine Studio crash\nversion: {}\nos: {}\narch: {}\n\n{}\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            info,
        );
        report.truncate(MAX_REPORT_BYTES);
        if write_report(&path, &report).is_err() {
            // The original hook is still useful when the project is read-only.
        }
        previous(info);
    }));
}

fn write_report(path: &Path, report: &str) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp: PathBuf = path.with_extension("tmp");
    let mut bounded = report.to_owned();
    bounded.truncate(MAX_REPORT_BYTES);
    std::fs::write(&temp, bounded)?;
    std::fs::rename(temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_writer_creates_a_bounded_file() {
        let dir = std::env::temp_dir().join(format!("vge-crash-test-{}", std::process::id()));
        let path = dir.join("crash.log");
        write_report(&path, &"x".repeat(MAX_REPORT_BYTES + 10)).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            MAX_REPORT_BYTES as u64
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
