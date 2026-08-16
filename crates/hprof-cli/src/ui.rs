//! Launch the bundled Bun/React UI when the workspace dependencies are
//! available. The Rust CLI remains usable without Bun; this is a convenience
//! bridge for local profiling sessions.

use std::process::Command as ProcessCommand;

use crate::Args;

pub fn run(args: &Args) -> Result<(), String> {
    if args.files.is_empty() {
        return Err("ui requires at least one profile file".to_string());
    }
    let files = serde_json::to_string(&args.files).map_err(|e| e.to_string())?;
    let script = format!(
        "import {{ startServer }} from './packages/ui/src/server/index.ts'; await startServer({{ files: {files}, port: {}, open: {} }});",
        args.port, args.open
    );
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let status = ProcessCommand::new("bun")
        .arg("-e")
        .arg(script)
        .current_dir(workspace)
        .status()
        .map_err(|e| format!("failed to start bun: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("bun UI exited with {status}"))
    }
}
