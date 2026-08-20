use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InvocationContext {
    pub workspace_id: Option<String>,
    pub workspace_cwd: Option<String>,
    pub focused_pane_id: Option<String>,
    pub focused_pane_cwd: Option<String>,
    pub worktree_checkout_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub root: PathBuf,
    pub tiltfile: Option<PathBuf>,
}

#[derive(Deserialize)]
struct RawInvocationContext {
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    workspace_cwd: Option<String>,
    #[serde(default)]
    focused_pane_id: Option<String>,
    #[serde(default)]
    focused_pane_cwd: Option<String>,
    #[serde(default)]
    worktree: Option<RawWorktree>,
}

#[derive(Deserialize)]
struct RawWorktree {
    checkout_path: String,
}

impl InvocationContext {
    pub fn from_json(json: &str) -> Result<Self> {
        let raw: RawInvocationContext =
            serde_json::from_str(json).context("parse HERDR_PLUGIN_CONTEXT_JSON")?;
        Ok(Self {
            workspace_id: raw.workspace_id,
            workspace_cwd: raw.workspace_cwd,
            focused_pane_id: raw.focused_pane_id,
            focused_pane_cwd: raw.focused_pane_cwd,
            worktree_checkout_path: raw.worktree.map(|worktree| worktree.checkout_path),
        })
    }
}

pub fn resolve_project(context: &InvocationContext) -> Result<Project> {
    let fallback = std::env::current_dir().context("resolve current directory")?;
    let boundary = first_existing_dir([
        context.worktree_checkout_path.as_deref(),
        context.workspace_cwd.as_deref(),
    ])
    .unwrap_or_else(|| fallback.clone());
    let mut current = first_existing_dir([context.focused_pane_cwd.as_deref()])
        .filter(|path| path.starts_with(&boundary))
        .unwrap_or_else(|| boundary.clone());

    loop {
        let candidate = current.join("Tiltfile");
        if candidate.is_file() {
            return Ok(Project {
                root: current,
                tiltfile: Some(candidate.canonicalize().context("canonicalize Tiltfile")?),
            });
        }
        if current == boundary || !current.pop() {
            break;
        }
    }

    Ok(Project {
        root: boundary,
        tiltfile: None,
    })
}

fn first_existing_dir<'a>(paths: impl IntoIterator<Item = Option<&'a str>>) -> Option<PathBuf> {
    paths
        .into_iter()
        .flatten()
        .map(Path::new)
        .filter(|path| path.is_dir())
        .find_map(|path| path.canonicalize().ok())
}
