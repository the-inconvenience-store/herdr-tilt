use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::project::{InvocationContext, resolve_project};

pub fn open_panel_from_env() -> Result<()> {
    let context_json =
        env::var("HERDR_PLUGIN_CONTEXT_JSON").context("HERDR_PLUGIN_CONTEXT_JSON is not set")?;
    let context = InvocationContext::from_json(&context_json)?;
    let project = resolve_project(&context)?;
    let herdr = env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_owned());
    let state_dir = env::var("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .context("HERDR_PLUGIN_STATE_DIR is not set")?;
    let panel_path = panel_state_path(&state_dir, &project.root);

    if let Some(panel) = read_panel_state(&panel_path)
        && is_safe_id(&panel.pane_id)
        && Command::new(&herdr)
            .args(["plugin", "pane", "focus", &panel.pane_id])
            .status()
            .is_ok_and(|status| status.success())
    {
        return Ok(());
    }

    let mut args = vec![
        "plugin".to_owned(),
        "pane".to_owned(),
        "open".to_owned(),
        "--plugin".to_owned(),
        "herdr.tilt".to_owned(),
        "--entrypoint".to_owned(),
        "status".to_owned(),
        "--placement".to_owned(),
        "split".to_owned(),
        "--direction".to_owned(),
        "right".to_owned(),
    ];
    if let Some(workspace_id) = context.workspace_id {
        args.extend(["--workspace".to_owned(), workspace_id]);
    }
    if let Some(pane_id) = context.focused_pane_id {
        args.extend(["--target-pane".to_owned(), pane_id]);
    }
    args.extend([
        "--cwd".to_owned(),
        project.root.display().to_string(),
        "--focus".to_owned(),
    ]);

    let output = Command::new(&herdr)
        .args(&args)
        .output()
        .with_context(|| format!("run {herdr}"))?;
    if !output.status.success() {
        bail!(
            "Herdr could not open the Tilt pane: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let opened: PaneOpenResponse =
        serde_json::from_slice(&output.stdout).context("parse Herdr plugin pane response")?;
    write_panel_state(
        &panel_path,
        &PanelState {
            pane_id: opened.result.plugin_pane.pane.pane_id,
        },
    )?;
    Ok(())
}

#[derive(Deserialize)]
struct PaneOpenResponse {
    result: PaneOpenResult,
}

#[derive(Deserialize)]
struct PaneOpenResult {
    plugin_pane: PluginPane,
}

#[derive(Deserialize)]
struct PluginPane {
    pane: OpenedPane,
}

#[derive(Deserialize)]
struct OpenedPane {
    pane_id: String,
}

#[derive(Deserialize, Serialize)]
struct PanelState {
    pane_id: String,
}

fn panel_state_path(state_dir: &Path, project_root: &Path) -> PathBuf {
    let digest = Sha256::digest(project_root.as_os_str().as_encoded_bytes());
    state_dir.join("panels").join(format!("{digest:x}.json"))
}

fn read_panel_state(path: &Path) -> Option<PanelState> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn write_panel_state(path: &Path, state: &PanelState) -> Result<()> {
    let parent = path.parent().context("panel state path has no parent")?;
    fs::create_dir_all(parent).context("create panel state directory")?;
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    fs::write(&temporary, serde_json::to_vec(state)?).context("write panel state")?;
    fs::rename(&temporary, path).context("commit panel state")?;
    Ok(())
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':' | b'.'))
}
