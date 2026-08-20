use std::fs;

use herdr_tilt::project::{InvocationContext, resolve_project};

#[test]
fn nested_pane_resolves_workspace_tiltfile() {
    let workspace = tempfile::tempdir().unwrap();
    let nested = workspace.path().join("services/api");
    fs::create_dir_all(&nested).unwrap();
    fs::write(workspace.path().join("Tiltfile"), "").unwrap();

    let context = InvocationContext {
        workspace_cwd: Some(workspace.path().display().to_string()),
        focused_pane_cwd: Some(nested.display().to_string()),
        worktree_checkout_path: Some(workspace.path().display().to_string()),
        ..InvocationContext::default()
    };

    let project = resolve_project(&context).unwrap();

    assert_eq!(project.root, workspace.path().canonicalize().unwrap());
    assert_eq!(
        project.tiltfile,
        Some(workspace.path().join("Tiltfile").canonicalize().unwrap())
    );
}
