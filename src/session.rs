use std::env;
use std::fs::{self, File, OpenOptions};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
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

pub fn run_from_env() -> Result<()> {
    let context_json =
        env::var("HERDR_PLUGIN_CONTEXT_JSON").context("HERDR_PLUGIN_CONTEXT_JSON is not set")?;
    let context = InvocationContext::from_json(&context_json)?;
    let project = resolve_project(&context)?;
    let state_dir = env::var("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .context("HERDR_PLUGIN_STATE_DIR is not set")?;
    run_project(&project, &state_dir)
}

pub fn load_session(project: &Project, state_dir: &Path) -> Option<SessionRecord> {
    let tiltfile = project.tiltfile.as_ref()?;
    serde_json::from_slice(&fs::read(session_path(state_dir, tiltfile)).ok()?).ok()
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
    let path = session_path(state_dir, tiltfile);
    write_record(&path, &record)?;

    let status = child.wait().context("wait for Tilt")?;
    record.phase = SessionPhase::Exited;
    record.exit_code = status.code();
    write_record(&path, &record)?;
    Ok(())
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

fn session_path(state_dir: &Path, tiltfile: &Path) -> PathBuf {
    state_dir
        .join("sessions")
        .join(format!("{}.json", project_key(tiltfile)))
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
