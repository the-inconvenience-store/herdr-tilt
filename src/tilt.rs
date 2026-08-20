use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

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
    pub disabled: bool,
    pub actions: Vec<ServiceAction>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceAction {
    Link {
        label: String,
        url: String,
    },
    Button {
        name: String,
        label: String,
        requires_confirmation: bool,
        inputs: Vec<UIButtonInputValue>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UIButtonInputValue {
    Text { name: String, value: String },
    Bool { name: String, value: bool },
    Hidden { name: String, value: String },
    Choice { name: String, value: String },
}

impl ServiceAction {
    pub fn label(&self) -> &str {
        action_label(self)
    }

    pub fn requires_confirmation(&self) -> bool {
        matches!(
            self,
            Self::Button {
                requires_confirmation: true,
                ..
            }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceGroup {
    pub name: String,
    pub status: CircleStatus,
    pub services: Vec<Service>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DashboardSnapshot {
    pub services: Vec<Service>,
    pub groups: Vec<ResourceGroup>,
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
    #[serde(default)]
    labels: BTreeMap<String, String>,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
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
    #[serde(default)]
    endpoint_links: Vec<EndpointLink>,
}

#[derive(Deserialize)]
struct EndpointLink {
    #[serde(default)]
    name: String,
    url: String,
}

#[derive(Deserialize)]
struct UIButtonList {
    #[serde(default)]
    items: Vec<UIButton>,
}

#[derive(Deserialize)]
struct UIButton {
    metadata: Metadata,
    #[serde(default)]
    spec: UIButtonSpec,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UIButtonSpec {
    #[serde(default)]
    disabled: bool,
    #[serde(default)]
    text: String,
    #[serde(default)]
    requires_confirmation: bool,
    #[serde(default)]
    location: UIButtonLocation,
    #[serde(default)]
    inputs: Vec<UIButtonInput>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UIButtonLocation {
    #[serde(default)]
    #[serde(rename = "componentID")]
    component_id: String,
    #[serde(default)]
    component_type: String,
}

#[derive(Deserialize)]
struct UIButtonInput {
    name: String,
    text: Option<TextInput>,
    #[serde(rename = "bool")]
    boolean: Option<BoolInput>,
    hidden: Option<HiddenInput>,
    choice: Option<ChoiceInput>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TextInput {
    #[serde(default)]
    default_value: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BoolInput {
    #[serde(default)]
    default_value: bool,
}

#[derive(Deserialize)]
struct HiddenInput {
    #[serde(default)]
    value: String,
}

#[derive(Deserialize)]
struct ChoiceInput {
    #[serde(default)]
    choices: Vec<String>,
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
    let mut grouped = BTreeMap::<String, Vec<(i32, Service)>>::new();
    let mut ungrouped = Vec::new();
    let mut tiltfile_error = None;

    for resource in list.items {
        if resource.metadata.name == "(Tiltfile)" {
            tiltfile_error = latest_error(&resource.status).map(ToOwned::to_owned);
            continue;
        }
        let status = circle_status(&resource.status);
        let detail = status_detail(&resource.status).to_owned();
        let service = Service {
            name: resource.metadata.name,
            status,
            detail,
            disabled: resource
                .status
                .disable_status
                .state
                .eq_ignore_ascii_case("disabled"),
            actions: resource
                .status
                .endpoint_links
                .iter()
                .map(|link| ServiceAction::Link {
                    label: endpoint_label(link),
                    url: link.url.clone(),
                })
                .collect(),
        };
        let order = resource.status.order;
        if resource.metadata.labels.is_empty() {
            ungrouped.push((order, service.clone()));
        } else {
            for label in resource.metadata.labels.into_keys() {
                grouped
                    .entry(label)
                    .or_default()
                    .push((order, service.clone()));
            }
        }
        ordered.push((order, service));
    }
    ordered.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.name.cmp(&right.1.name))
    });

    let mut groups = grouped
        .into_iter()
        .map(|(name, mut services)| {
            sort_services(&mut services);
            let services = services
                .into_iter()
                .map(|(_, service)| service)
                .collect::<Vec<_>>();
            ResourceGroup {
                name,
                status: aggregate_status(&services),
                services,
            }
        })
        .collect::<Vec<_>>();
    if !ungrouped.is_empty() {
        sort_services(&mut ungrouped);
        let services = ungrouped
            .into_iter()
            .map(|(_, service)| service)
            .collect::<Vec<_>>();
        groups.push(ResourceGroup {
            name: "Ungrouped".to_owned(),
            status: aggregate_status(&services),
            services,
        });
    }

    Ok(DashboardSnapshot {
        services: ordered.into_iter().map(|(_, service)| service).collect(),
        groups,
        tiltfile_error,
    })
}

pub fn parse_ui_buttons(json: &str) -> Result<BTreeMap<String, Vec<ServiceAction>>> {
    let list: UIButtonList = serde_json::from_str(json).context("parse Tilt UIButton list")?;
    let mut result = BTreeMap::<String, Vec<ServiceAction>>::new();
    for button in list.items {
        let button_type = button
            .metadata
            .annotations
            .get("tilt.dev/uibutton-type")
            .map(String::as_str);
        if button.spec.disabled
            || button.spec.location.component_type != "Resource"
            || button.spec.location.component_id.is_empty()
            || matches!(button_type, Some("StopBuild" | "DisableToggle"))
        {
            continue;
        }
        let label = if button.spec.text.is_empty() {
            button.metadata.name.clone()
        } else {
            button.spec.text
        };
        let inputs = button
            .spec
            .inputs
            .into_iter()
            .filter_map(default_input_value)
            .collect();
        result
            .entry(button.spec.location.component_id)
            .or_default()
            .push(ServiceAction::Button {
                name: button.metadata.name,
                label,
                requires_confirmation: button.spec.requires_confirmation,
                inputs,
            });
    }
    for actions in result.values_mut() {
        actions.sort_by(|left, right| action_label(left).cmp(action_label(right)));
    }
    Ok(result)
}

pub fn attach_service_actions(
    snapshot: &mut DashboardSnapshot,
    actions_by_service: BTreeMap<String, Vec<ServiceAction>>,
) {
    for service in &mut snapshot.services {
        if let Some(actions) = actions_by_service.get(&service.name) {
            service.actions.extend(actions.clone());
        }
    }
    for group in &mut snapshot.groups {
        for service in &mut group.services {
            if let Some(actions) = actions_by_service.get(&service.name) {
                service.actions.extend(actions.clone());
            }
        }
    }
}

fn default_input_value(input: UIButtonInput) -> Option<UIButtonInputValue> {
    if let Some(spec) = input.text {
        Some(UIButtonInputValue::Text {
            name: input.name,
            value: spec.default_value,
        })
    } else if let Some(spec) = input.boolean {
        Some(UIButtonInputValue::Bool {
            name: input.name,
            value: spec.default_value,
        })
    } else if let Some(spec) = input.hidden {
        Some(UIButtonInputValue::Hidden {
            name: input.name,
            value: spec.value,
        })
    } else {
        input.choice.and_then(|spec| {
            spec.choices
                .into_iter()
                .next()
                .map(|value| UIButtonInputValue::Choice {
                    name: input.name,
                    value,
                })
        })
    }
}

fn action_label(action: &ServiceAction) -> &str {
    match action {
        ServiceAction::Link { label, .. } | ServiceAction::Button { label, .. } => label,
    }
}

fn endpoint_label(link: &EndpointLink) -> String {
    if !link.name.is_empty() {
        return link.name.clone();
    }
    link.url
        .strip_prefix("https://")
        .or_else(|| link.url.strip_prefix("http://"))
        .unwrap_or(&link.url)
        .trim_end_matches('/')
        .to_owned()
}

pub fn activate_service_action(action: &ServiceAction, port: u16) -> Result<()> {
    match action {
        ServiceAction::Link { url, .. } => open_url(url),
        ServiceAction::Button { .. } => {
            let timestamp = chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S%.6fZ")
                .to_string();
            put_button_status_at(action, port, &timestamp)
        }
    }
}

pub fn tilt_web_url(port: u16) -> String {
    format!("http://localhost:{port}/")
}

pub fn open_tilt_web_ui(port: u16) -> Result<()> {
    open_url(&tilt_web_url(port))
}

fn button_status_body(action: &ServiceAction, timestamp: &str) -> Result<Value> {
    let ServiceAction::Button { name, inputs, .. } = action else {
        bail!("only Tilt UI buttons have a status payload");
    };
    let inputs = inputs
        .iter()
        .map(|input| match input {
            UIButtonInputValue::Text { name, value } => {
                serde_json::json!({"name": name, "text": {"value": value}})
            }
            UIButtonInputValue::Bool { name, value } => {
                serde_json::json!({"name": name, "bool": {"value": value}})
            }
            UIButtonInputValue::Hidden { name, value } => {
                serde_json::json!({"name": name, "hidden": {"value": value}})
            }
            UIButtonInputValue::Choice { name, value } => {
                serde_json::json!({"name": name, "choice": {"value": value}})
            }
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "metadata": {"name": name},
        "status": {"lastClickedAt": timestamp, "inputs": inputs}
    }))
}

fn put_button_status_at(action: &ServiceAction, port: u16, timestamp: &str) -> Result<()> {
    let ServiceAction::Button { name, .. } = action else {
        bail!("only Tilt UI buttons can be activated through the Tilt API");
    };
    let body = serde_json::to_vec(&button_status_body(action, timestamp)?)?;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).context("connect to Tilt API")?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "PUT /proxy/apis/tilt.dev/v1alpha1/uibuttons/{name}/status HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    stream.shutdown(Shutdown::Write)?;
    let mut response = [0_u8; 1024];
    let size = stream.read(&mut response)?;
    let response = String::from_utf8_lossy(&response[..size]);
    let status = response.lines().next().unwrap_or_default();
    if !status.contains(" 200 ") && !status.contains(" 201 ") {
        bail!("Tilt rejected button {name}: {status}");
    }
    Ok(())
}

fn open_url(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    bail!("opening URLs is not supported on this platform");
    let status = command.arg(url).status().context("open service URL")?;
    if !status.success() {
        bail!("Could not open service URL: {url}");
    }
    Ok(())
}

fn aggregate_status(services: &[Service]) -> CircleStatus {
    if services
        .iter()
        .any(|service| service.status == CircleStatus::Red)
    {
        CircleStatus::Red
    } else if services
        .iter()
        .any(|service| service.status == CircleStatus::Orange)
    {
        CircleStatus::Orange
    } else if services
        .iter()
        .any(|service| service.status == CircleStatus::Green)
    {
        CircleStatus::Green
    } else {
        CircleStatus::Grey
    }
}

fn sort_services(services: &mut [(i32, Service)]) {
    services.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.name.cmp(&right.1.name))
    });
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
            .first()
            .is_some_and(|build| !build.warnings.is_empty())
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
    if status.current_build.is_some()
        || status.update_status == "in_progress"
        || status.update_status == "pending"
        || status.queued
    {
        return None;
    }
    status
        .build_history
        .first()
        .and_then(|build| (!build.error.is_empty()).then_some(build.error.as_str()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn resources_expose_named_and_unnamed_endpoint_links() {
        let mut snapshot = parse_ui_resources(
            r#"{"items":[{"metadata":{"name":"api"},"status":{"endpointLinks":[{"name":"Docs","url":"https://example.test/docs"},{"url":"https://example.test/health"}]}}]}"#,
        )
        .unwrap();

        assert_eq!(
            snapshot.services.remove(0).actions,
            vec![
                ServiceAction::Link {
                    label: "Docs".to_owned(),
                    url: "https://example.test/docs".to_owned(),
                },
                ServiceAction::Link {
                    label: "example.test/health".to_owned(),
                    url: "https://example.test/health".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn ui_buttons_attach_to_their_service_with_default_inputs() {
        let buttons = parse_ui_buttons(
            r#"{"items":[{"metadata":{"name":"api-seed"},"spec":{"text":"Seed data","requiresConfirmation":true,"location":{"componentID":"api","componentType":"Resource"},"inputs":[{"name":"force","bool":{"defaultValue":true}},{"name":"token","hidden":{"value":"abc"}},{"name":"environment","choice":{"choices":["dev","prod"]}},{"name":"message","text":{"defaultValue":"hello"}}]}}]}"#,
        )
        .unwrap();

        assert_eq!(
            buttons.get("api").unwrap(),
            &[ServiceAction::Button {
                name: "api-seed".to_owned(),
                label: "Seed data".to_owned(),
                requires_confirmation: true,
                inputs: vec![
                    UIButtonInputValue::Bool {
                        name: "force".to_owned(),
                        value: true
                    },
                    UIButtonInputValue::Hidden {
                        name: "token".to_owned(),
                        value: "abc".to_owned()
                    },
                    UIButtonInputValue::Choice {
                        name: "environment".to_owned(),
                        value: "dev".to_owned()
                    },
                    UIButtonInputValue::Text {
                        name: "message".to_owned(),
                        value: "hello".to_owned()
                    },
                ],
            }]
        );
    }

    #[test]
    fn attaching_buttons_updates_unique_and_grouped_service_copies() {
        let mut snapshot = parse_ui_resources(
            r#"{"items":[{"metadata":{"name":"api","labels":{"apps":""}},"status":{}}]}"#,
        )
        .unwrap();
        let actions = BTreeMap::from([(
            "api".to_owned(),
            vec![ServiceAction::Button {
                name: "api-seed".to_owned(),
                label: "Seed".to_owned(),
                requires_confirmation: false,
                inputs: vec![],
            }],
        )]);

        attach_service_actions(&mut snapshot, actions);

        assert_eq!(snapshot.services[0].actions.len(), 1);
        assert_eq!(snapshot.groups[0].services[0].actions.len(), 1);
    }

    #[test]
    fn built_in_and_global_buttons_are_not_service_actions() {
        let buttons = parse_ui_buttons(
            r#"{"items":[{"metadata":{"name":"stop","annotations":{"tilt.dev/uibutton-type":"StopBuild"}},"spec":{"text":"Stop","location":{"componentID":"api","componentType":"Resource"}}},{"metadata":{"name":"global"},"spec":{"text":"Global","location":{"componentID":"nav","componentType":"Global"}}}]}"#,
        )
        .unwrap();

        assert!(buttons.is_empty());
    }

    #[test]
    fn button_status_matches_tilts_status_subresource_payload() {
        let action = ServiceAction::Button {
            name: "api-seed".to_owned(),
            label: "Seed".to_owned(),
            requires_confirmation: false,
            inputs: vec![
                UIButtonInputValue::Bool {
                    name: "force".to_owned(),
                    value: true,
                },
                UIButtonInputValue::Hidden {
                    name: "token".to_owned(),
                    value: "abc".to_owned(),
                },
            ],
        };

        let body = button_status_body(&action, "2026-08-20T02:30:00.000000Z").unwrap();

        assert_eq!(
            body,
            serde_json::json!({
                "metadata": {"name": "api-seed"},
                "status": {
                    "lastClickedAt": "2026-08-20T02:30:00.000000Z",
                    "inputs": [
                        {"name": "force", "bool": {"value": true}},
                        {"name": "token", "hidden": {"value": "abc"}}
                    ]
                }
            })
        );
    }

    #[test]
    fn button_activation_puts_status_to_the_tilt_api() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).unwrap();
            let request = String::from_utf8_lossy(&bytes).to_string();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                .unwrap();
            request
        });
        let action = ServiceAction::Button {
            name: "api-seed".to_owned(),
            label: "Seed".to_owned(),
            requires_confirmation: false,
            inputs: vec![],
        };

        put_button_status_at(&action, port, "2026-08-20T02:30:00.000000Z").unwrap();
        let request = server.join().unwrap();

        assert!(request.starts_with(
            "PUT /proxy/apis/tilt.dev/v1alpha1/uibuttons/api-seed/status HTTP/1.1\r\n"
        ));
        assert!(request.contains("Content-Type: application/json\r\n"));
        assert!(request.contains(r#""lastClickedAt":"2026-08-20T02:30:00.000000Z""#));
    }

    #[test]
    fn tilt_web_url_uses_the_active_api_port() {
        assert_eq!(tilt_web_url(10350), "http://localhost:10350/");
        assert_eq!(tilt_web_url(41234), "http://localhost:41234/");
    }
}
