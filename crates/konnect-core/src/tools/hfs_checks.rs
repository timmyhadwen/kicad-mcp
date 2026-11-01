//! `hfs_checks` toolset — run a board project's own Python checks under KiCAD's bundled Python.
//!
//! Many boards keep their engineering checks as pcbnew scripts in the project (power-path
//! resistance, pour audits, a project router). These tools run them headless and return the
//! output, so an agent can check a board between live IPC edits without leaving MCP.

use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::{require_str, ToolContext, ToolDef};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

/// KiCAD's bundled interpreter (has `pcbnew`). Override with `KICAD_PYTHON`.
fn kicad_python() -> String {
    if let Ok(p) = std::env::var("KICAD_PYTHON") {
        return p;
    }
    if cfg!(target_os = "macos") {
        "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3".into()
    } else if cfg!(target_os = "windows") {
        r"C:\Program Files\KiCad\10.0\bin\python.exe".into()
    } else {
        "python3".into()
    }
}

/// Keep the end of long outputs: checks print their summary last.
fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("[… {} bytes cut …]\n{}", start, &s[start..])
}

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool!(
            "run_kicad_python",
            "Run a Python script from a board project under KiCAD's bundled Python (pcbnew available), \
             with the project directory as the working directory. Use for project checks such as \
             tools/pdn.py, tools/layout.py fill or tools/router.py. Note: scripts that save the board \
             overwrite the .kicad_pcb on disk; reload it in KiCAD afterwards.",
            json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Absolute path of the KiCAD project directory" },
                    "script": { "type": "string", "description": "Script path, relative to project_dir (e.g. tools/pdn.py)" },
                    "args": { "type": "array", "items": { "type": "string" }, "description": "Arguments passed to the script" },
                    "env": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Extra environment variables" },
                    "timeout_s": { "type": "number", "description": "Timeout in seconds (default 600)" }
                },
                "required": ["project_dir", "script"]
            }),
            |args, ctx| async move { handle_run_kicad_python(args, ctx).await }
        ),
        tool!(
            "power_path_check",
            "Run the project's power-path resistance check (tools/pdn.py): mOhm, mV and W at 20 A for \
             every high-current path, peak current density and neck locations.",
            json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Absolute path of the KiCAD project directory" },
                    "resolution_mm": { "type": "number", "description": "Grid resolution in mm (default 0.2)" }
                },
                "required": ["project_dir"]
            }),
            |args, ctx| async move { handle_power_path_check(args, ctx).await }
        ),
    ]
}

async fn run_script(project_dir: &str, script: &str, args: &[String], env: &[(String, String)], timeout_s: f64) -> CallToolResult {
    let dir = PathBuf::from(project_dir);
    if !dir.is_dir() {
        return CallToolResult::error(format!("project_dir is not a directory: {}", project_dir));
    }
    let script_path = dir.join(script);
    if !script_path.is_file() {
        return CallToolResult::error(format!("script not found: {}", script_path.display()));
    }
    let mut cmd = tokio::process::Command::new(kicad_python());
    cmd.arg(&script_path).args(args).current_dir(&dir).kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = match tokio::time::timeout(Duration::from_secs_f64(timeout_s.max(1.0)), cmd.output()).await {
        Err(_) => return CallToolResult::error(format!("timed out after {} s: {}", timeout_s, script)),
        Ok(Err(e)) => return CallToolResult::error(format!("could not start KiCAD Python ({}): {}", kicad_python(), e)),
        Ok(Ok(o)) => o,
    };
    // KiCAD's Python prints wx/fontconfig noise on stderr; keep it, but after stdout.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let code = out.status.code().unwrap_or(-1);
    let body = format!("exit {}\n--- stdout ---\n{}\n--- stderr ---\n{}", code, tail(&stdout, 20_000), tail(&stderr, 4_000));
    if out.status.success() {
        CallToolResult::text(body)
    } else {
        CallToolResult::error(body)
    }
}

async fn handle_run_kicad_python(args: &Value, _ctx: &ToolContext) -> anyhow::Result<CallToolResult> {
    let project_dir = match require_str(args, "project_dir") { Ok(s) => s, Err(e) => return Ok(e) };
    let script = match require_str(args, "script") { Ok(s) => s, Err(e) => return Ok(e) };
    let argv: Vec<String> = args["args"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let env: Vec<(String, String)> = args["env"].as_object()
        .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
        .unwrap_or_default();
    let timeout_s = args["timeout_s"].as_f64().unwrap_or(600.0);
    Ok(run_script(project_dir, script, &argv, &env, timeout_s).await)
}

async fn handle_power_path_check(args: &Value, _ctx: &ToolContext) -> anyhow::Result<CallToolResult> {
    let project_dir = match require_str(args, "project_dir") { Ok(s) => s, Err(e) => return Ok(e) };
    let res = args["resolution_mm"].as_f64().unwrap_or(0.2);
    Ok(run_script(project_dir, "tools/pdn.py", &[], &[("ROUTER_RES".into(), res.to_string())], 900.0).await)
}
