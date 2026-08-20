use std::env;
use std::fs::{self, File, OpenOptions};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::project::{InvocationContext, Project, resolve_project};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Running,
    Exited,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SessionRecord {
    pub tiltfile: PathBuf,
    pub project_root: PathBuf,
    pub port: u16,
    pub runner_pid: u32,
    pub tilt_pid: u32,
    pub started_unix_ms: u64,
    pub phase: SessionPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

#[derive(Deserialize, Serialize)]
struct StartRequest {
    project_root: PathBuf,
    tiltfile: PathBuf,
}

pub fn run_from_env() -> Result<()> {
    let context_json =
        env::var("HERDR_PLUGIN_CONTEXT_JSON").context("HERDR_PLUGIN_CONTEXT_JSON is not set")?;
    let context = InvocationContext::from_json(&context_json)?;
    let state_dir = env::var("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .context("HERDR_PLUGIN_STATE_DIR is not set")?;
    let project = match take_start_request(&state_dir, context.workspace_id.as_deref())? {
        Some(project) => project,
        None => resolve_project(&context)?,
    };
    run_project(&project, &state_dir)
}

pub fn prepare_start_request(
    project: &Project,
    state_dir: &Path,
    workspace_id: Option<&str>,
) -> Result<Option<PathBuf>> {
    let Some(workspace_id) = workspace_id else {
        return Ok(None);
    };
    let tiltfile = project
        .tiltfile
        .as_ref()
        .context("No Tiltfile found in this workspace")?;
    let path = start_request_path(state_dir, workspace_id);
    let parent = path.parent().context("start request path has no parent")?;
    fs::create_dir_all(parent).context("create start request directory")?;
    let request = StartRequest {
        project_root: project.root.clone(),
        tiltfile: tiltfile.clone(),
    };
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    fs::write(&temporary, serde_json::to_vec(&request)?).context("write start request")?;
    fs::rename(&temporary, &path).context("commit start request")?;
    Ok(Some(path))
}

pub fn discard_start_request(path: Option<&Path>) {
    if let Some(path) = path {
        let _ = fs::remove_file(path);
    }
}

pub fn down_from_env() -> Result<()> {
    let context_json =
        env::var("HERDR_PLUGIN_CONTEXT_JSON").context("HERDR_PLUGIN_CONTEXT_JSON is not set")?;
    let context = InvocationContext::from_json(&context_json)?;
    let project = resolve_project(&context)?;
    let state_dir = env::var("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .context("HERDR_PLUGIN_STATE_DIR is not set")?;
    down_project(&project, &state_dir)
}

pub fn load_session(project: &Project, state_dir: &Path) -> Option<SessionRecord> {
    let tiltfile = project.tiltfile.as_ref()?;
    serde_json::from_slice(&fs::read(record_path(state_dir, tiltfile)).ok()?).ok()
}

pub fn record_path(state_dir: &Path, tiltfile: &Path) -> PathBuf {
    state_dir
        .join("sessions")
        .join(format!("{}.json", project_key(tiltfile)))
}

fn run_project(project: &Project, state_dir: &Path) -> Result<()> {
    let tiltfile = project
        .tiltfile
        .as_ref()
        .context("No Tiltfile found in this workspace")?;
    let sessions_dir = state_dir.join("sessions");
    let logs_dir = state_dir.join("logs");
    fs::create_dir_all(&sessions_dir).context("create session state directory")?;
    fs::create_dir_all(&logs_dir).context("create Tilt log directory")?;

    let key = project_key(tiltfile);
    let lock_path = sessions_dir.join(format!("{key}.lock"));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .context("open project session lock")?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(());
    }

    let port = available_port()?;
    let log = open_log(&logs_dir.join(format!("{key}.log")))?;
    let tilt = env::var("TILT_BIN_PATH").unwrap_or_else(|_| "tilt".to_owned());
    let mut child = Command::new(&tilt)
        .args([
            "up",
            "--stream",
            "--port",
            &port.to_string(),
            "-f",
            &tiltfile.display().to_string(),
        ])
        .current_dir(&project.root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().context("clone Tilt log")?))
        .stderr(Stdio::from(log))
        .spawn()
        .with_context(|| format!("start {tilt}"))?;

    let mut record = SessionRecord {
        tiltfile: tiltfile.clone(),
        project_root: project.root.clone(),
        port,
        runner_pid: std::process::id(),
        tilt_pid: child.id(),
        started_unix_ms: now_unix_ms(),
        phase: SessionPhase::Running,
        exit_code: None,
    };
    let path = record_path(state_dir, tiltfile);
    write_record(&path, &record)?;

    let status = child.wait().context("wait for Tilt")?;
    record.phase = SessionPhase::Exited;
    record.exit_code = status.code();
    write_record(&path, &record)?;
    Ok(())
}

fn take_start_request(state_dir: &Path, workspace_id: Option<&str>) -> Result<Option<Project>> {
    let Some(workspace_id) = workspace_id else {
        return Ok(None);
    };
    let path = start_request_path(state_dir, workspace_id);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read start request"),
    };
    fs::remove_file(&path).context("consume start request")?;
    let request: StartRequest = serde_json::from_slice(&bytes).context("parse start request")?;
    let project_root = request
        .project_root
        .canonicalize()
        .context("canonicalize requested project root")?;
    let tiltfile = request
        .tiltfile
        .canonicalize()
        .context("canonicalize requested Tiltfile")?;
    if !tiltfile.is_file() || tiltfile.parent() != Some(project_root.as_path()) {
        bail!("retained Tilt start request does not identify a project Tiltfile");
    }
    Ok(Some(Project {
        root: project_root,
        tiltfile: Some(tiltfile),
    }))
}

fn start_request_path(state_dir: &Path, workspace_id: &str) -> PathBuf {
    let digest = Sha256::digest(workspace_id.as_bytes());
    state_dir
        .join("start-requests")
        .join(format!("{digest:x}.json"))
}

fn down_project(project: &Project, state_dir: &Path) -> Result<()> {
    let tiltfile = project
        .tiltfile
        .as_ref()
        .context("No Tiltfile found in this workspace")?;
    let key = project_key(tiltfile);
    let lock_path = state_dir.join("sessions").join(format!("{key}.lock"));
    let record_path = record_path(state_dir, tiltfile);

    if project_lock_is_held(&lock_path)?
        && let Some(record) = load_session(project, state_dir)
        && record.phase == SessionPhase::Running
    {
        let pid = i32::try_from(record.tilt_pid).context("Tilt PID is out of range")?;
        kill(Pid::from_raw(pid), Signal::SIGTERM).context("signal retained Tilt process")?;
        wait_for_runner_exit(project, state_dir)?;
    }

    let tilt = env::var("TILT_BIN_PATH").unwrap_or_else(|_| "tilt".to_owned());
    let output = Command::new(&tilt)
        .args(["down", "-f", &tiltfile.display().to_string()])
        .current_dir(&project.root)
        .output()
        .with_context(|| format!("run {tilt} down"))?;
    if !output.status.success() {
        bail!(
            "Tilt down failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if record_path.exists() {
        fs::remove_file(record_path).context("clear stopped Tilt session state")?;
    }
    Ok(())
}

fn project_lock_is_held(path: &Path) -> Result<bool> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .context("open project session lock")?;
    match lock.try_lock_exclusive() {
        Ok(()) => {
            lock.unlock().context("unlock project session")?;
            Ok(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error).context("check project session lock"),
    }
}

fn wait_for_runner_exit(project: &Project, state_dir: &Path) -> Result<()> {
    for _ in 0..100 {
        if load_session(project, state_dir)
            .is_none_or(|record| record.phase != SessionPhase::Running)
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    bail!("Timed out waiting for retained Tilt process to stop")
}

fn available_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("allocate Tilt API port")?;
    let port = listener.local_addr()?.port();
    if port == 0 {
        bail!("operating system returned an invalid Tilt API port");
    }
    Ok(port)
}

fn open_log(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open Tilt log at {}", path.display()))
}

fn project_key(tiltfile: &Path) -> String {
    let digest = Sha256::digest(tiltfile.as_os_str().as_encoded_bytes());
    format!("{digest:x}")
}

fn write_record(path: &Path, record: &SessionRecord) -> Result<()> {
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    fs::write(&temporary, serde_json::to_vec_pretty(record)?).context("write session state")?;
    fs::rename(&temporary, path).context("commit session state")?;
    Ok(())
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
