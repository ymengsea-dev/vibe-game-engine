//! Player-to-Studio Lua diagnostic smoke test.

use std::process::Command;

use engine::project::Project;
use engine::scripting::{LUA_DIAGNOSTIC_PREFIX, LuaScriptDiagnostic};

#[test]
fn invalid_project_script_reports_a_source_problem_before_opening_a_window() {
    let root = std::env::temp_dir().join(format!(
        "vge-player-diagnostic-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let project = Project::create(&root, "Broken Lua").expect("project");
    std::fs::write(
        project.assets_dir().join("scripts/main.lua"),
        "return {\n  start = function(]\n}",
    )
    .expect("bad script");

    let result = Command::new(env!("CARGO_BIN_EXE_player"))
        .arg("--project")
        .arg(&root)
        .output()
        .expect("player process");
    assert!(!result.status.success());
    let stderr = String::from_utf8(result.stderr).expect("UTF-8 log");
    let payload = stderr
        .lines()
        .find_map(|line| line.strip_prefix(LUA_DIAGNOSTIC_PREFIX))
        .expect("structured Lua problem");
    let diagnostic: LuaScriptDiagnostic = serde_json::from_str(payload).expect("diagnostic JSON");
    assert_eq!(diagnostic.path, "assets/scripts/main.lua");
    assert_eq!(diagnostic.line, 2);
    assert!(stderr.contains("syntax error"));
    std::fs::remove_dir_all(&root).expect("cleanup");
}
