use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircleStatus {
    Green,
    Orange,
    Red,
    Grey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    pub status: CircleStatus,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DashboardSnapshot {
    pub services: Vec<Service>,
    pub tiltfile_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionIdentity {
    pub tiltfile: PathBuf,
    pub pid: u32,
    pub start_time: String,
}

#[derive(Deserialize)]
struct UIResourceList {
    #[serde(default)]
    items: Vec<UIResource>,
}

#[derive(Deserialize)]
struct UIResource {
    metadata: Metadata,
    #[serde(default)]
    status: ResourceStatus,
}

#[derive(Deserialize)]
struct Metadata {
    name: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceStatus {
    #[serde(default)]
    order: i32,
    #[serde(default)]
    update_status: String,
    #[serde(default)]
    runtime_status: String,
    #[serde(default)]
    disable_status: DisableStatus,
    #[serde(default)]
    current_build: Option<Value>,
    #[serde(default)]
    queued: bool,
    #[serde(default)]
    build_history: Vec<BuildRecord>,
}

#[derive(Default, Deserialize)]
struct DisableStatus {
    #[serde(default)]
    state: String,
}

#[derive(Default, Deserialize)]
struct BuildRecord {
    #[serde(default)]
    error: String,
    #[serde(default)]
    warnings: Vec<String>,
}

#[derive(Deserialize)]
struct SessionList {
    #[serde(default)]
    items: Vec<TiltSession>,
}

#[derive(Deserialize)]
struct TiltSession {
    spec: SessionSpec,
    status: SessionStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionSpec {
    tiltfile_path: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionStatus {
    pid: u32,
    start_time: String,
}

pub fn parse_ui_resources(json: &str) -> Result<DashboardSnapshot> {
    let list: UIResourceList = serde_json::from_str(json).context("parse Tilt UIResource list")?;
    let mut ordered = Vec::new();
    let mut tiltfile_error = None;

    for resource in list.items {
        if resource.metadata.name == "(Tiltfile)" {
            tiltfile_error = latest_error(&resource.status).map(ToOwned::to_owned);
            continue;
        }
        let status = circle_status(&resource.status);
        let detail = status_detail(&resource.status).to_owned();
        ordered.push((
            resource.status.order,
            Service {
                name: resource.metadata.name,
                status,
                detail,
            },
        ));
    }
    ordered.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.name.cmp(&right.1.name))
    });

    Ok(DashboardSnapshot {
        services: ordered.into_iter().map(|(_, service)| service).collect(),
        tiltfile_error,
    })
}

pub fn parse_session_identity(json: &str) -> Result<SessionIdentity> {
    let list: SessionList = serde_json::from_str(json).context("parse Tilt Session list")?;
    let session = list
        .items
        .into_iter()
        .next()
        .context("Tilt API returned no Session")?;
    Ok(SessionIdentity {
        tiltfile: session.spec.tiltfile_path,
        pid: session.status.pid,
        start_time: session.status.start_time,
    })
}

fn circle_status(status: &ResourceStatus) -> CircleStatus {
    if status.disable_status.state.eq_ignore_ascii_case("disabled") {
        return CircleStatus::Grey;
    }
    if status.update_status == "error"
        || status.runtime_status == "error"
        || latest_error(status).is_some()
    {
        return CircleStatus::Red;
    }
    if status.update_status == "in_progress"
        || status.update_status == "pending"
        || status.runtime_status == "pending"
        || status.current_build.is_some()
        || status.queued
        || status
            .build_history
            .iter()
            .any(|build| !build.warnings.is_empty())
    {
        return CircleStatus::Orange;
    }
    if status.runtime_status == "ok"
        || (status.update_status == "ok" && status.runtime_status == "not_applicable")
    {
        return CircleStatus::Green;
    }
    CircleStatus::Grey
}

fn latest_error(status: &ResourceStatus) -> Option<&str> {
    status
        .build_history
        .iter()
        .find_map(|build| (!build.error.is_empty()).then_some(build.error.as_str()))
}

fn status_detail(status: &ResourceStatus) -> &str {
    if status.disable_status.state.eq_ignore_ascii_case("disabled") {
        "disabled"
    } else if let Some(error) = latest_error(status) {
        error
    } else if status.update_status == "error" || status.runtime_status == "error" {
        "error"
    } else if status.update_status == "in_progress" || status.current_build.is_some() {
        "building"
    } else if status.queued || status.update_status == "pending" {
        "queued"
    } else if status.runtime_status == "pending" {
        "pending"
    } else if status.runtime_status == "ok" || status.update_status == "ok" {
        "healthy"
    } else {
        "inactive"
    }
}
