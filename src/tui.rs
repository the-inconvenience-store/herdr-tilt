use std::collections::BTreeSet;
use std::env;
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::logs::{LogBuffer, LogNavigation, TiltLogStream};
use crate::project::Project;
use crate::project::{InvocationContext, resolve_project};
use crate::session::{SessionPhase, load_session};
use crate::tilt::{
    CircleStatus, ResourceGroup, Service, ServiceAction, activate_service_action,
    attach_service_actions, parse_session_identity, parse_ui_buttons, parse_ui_resources,
};
use anyhow::{Context, Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

const DEFAULT_TILT_PORT: u16 = 10350;
const SERVICE_PAGE_SIZE: usize = 10;
const SERVICE_SELECTION_BG: Color = Color::Rgb(58, 58, 58);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServiceNavigation {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectedServiceAction {
    Trigger,
    ToggleEnabled,
    Logs,
    Actions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ActionPickerEvent {
    None,
    Back,
    Activate(ServiceAction),
}

#[derive(Debug)]
struct ActionPicker {
    service_name: String,
    actions: Vec<ServiceAction>,
    state: ListState,
}

impl ActionPicker {
    fn new(service_name: String, actions: Vec<ServiceAction>) -> Self {
        let mut state = ListState::default();
        if !actions.is_empty() {
            state.select(Some(0));
        }
        Self {
            service_name,
            actions,
            state,
        }
    }

    fn selected(&self) -> Option<&ServiceAction> {
        self.actions.get(self.state.selected()?)
    }

    fn handle_key(&mut self, key: KeyCode) -> ActionPickerEvent {
        let selected = self.state.selected().unwrap_or_default();
        match key {
            KeyCode::Char('q') | KeyCode::Esc => ActionPickerEvent::Back,
            KeyCode::Up | KeyCode::Char('k') => {
                self.state.select(Some(selected.saturating_sub(1)));
                ActionPickerEvent::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.state.select(Some(
                    (selected + 1).min(self.actions.len().saturating_sub(1)),
                ));
                ActionPickerEvent::None
            }
            KeyCode::Enter | KeyCode::Char(' ') => self
                .selected()
                .cloned()
                .map_or(ActionPickerEvent::None, ActionPickerEvent::Activate),
            _ => ActionPickerEvent::None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DownConfirmationDecision {
    Confirm,
    Cancel,
}

fn down_confirmation_decision(key: KeyCode) -> Option<DownConfirmationDecision> {
    match key {
        KeyCode::Char('y' | 'Y') => Some(DownConfirmationDecision::Confirm),
        KeyCode::Char('n' | 'N') => Some(DownConfirmationDecision::Cancel),
        _ => None,
    }
}

fn toggle_help_for_key(key: KeyCode, show_help: &mut bool) -> bool {
    if key != KeyCode::Char('?') {
        return false;
    }
    *show_help = !*show_help;
    true
}

#[derive(Debug, Default)]
struct ServiceListState {
    inner: ListState,
    collapsed: BTreeSet<String>,
}

impl ServiceListState {
    fn visible_rows<'a>(&self, groups: &'a [ResourceGroup]) -> Vec<ServiceListRow<'a>> {
        let mut rows = Vec::new();
        for group in groups {
            rows.push(ServiceListRow::Group(group));
            if !self.collapsed.contains(&group.name) {
                rows.extend(group.services.iter().map(ServiceListRow::Service));
            }
        }
        rows
    }

    fn sync(&mut self, groups: &[ResourceGroup]) {
        let row_count = self.visible_rows(groups).len();
        if row_count == 0 {
            self.inner.select(None);
        } else {
            let selected = self.inner.selected().unwrap_or_default().min(row_count - 1);
            self.inner.select(Some(selected));
        }
    }

    fn navigate(&mut self, navigation: ServiceNavigation, groups: &[ResourceGroup]) {
        let row_count = self.visible_rows(groups).len();
        if row_count == 0 {
            self.inner.select(None);
            return;
        }
        let selected = self.inner.selected().unwrap_or_default().min(row_count - 1);
        let selected = match navigation {
            ServiceNavigation::Up => selected.saturating_sub(1),
            ServiceNavigation::Down => (selected + 1).min(row_count - 1),
            ServiceNavigation::PageUp => selected.saturating_sub(SERVICE_PAGE_SIZE),
            ServiceNavigation::PageDown => (selected + SERVICE_PAGE_SIZE).min(row_count - 1),
            ServiceNavigation::Home => 0,
            ServiceNavigation::End => row_count - 1,
        };
        self.inner.select(Some(selected));
    }

    fn selected_group_name(&self, groups: &[ResourceGroup]) -> Option<String> {
        let selected = self.inner.selected()?;
        match self.visible_rows(groups).get(selected)? {
            ServiceListRow::Group(group) => Some(group.name.clone()),
            ServiceListRow::Service(_) => None,
        }
    }

    fn toggle_selected_group(&mut self, groups: &[ResourceGroup]) {
        let Some(group) = self.selected_group_name(groups) else {
            return;
        };
        if !self.collapsed.remove(&group) {
            self.collapsed.insert(group);
        }
        self.sync(groups);
    }

    fn handle_key(&mut self, key: KeyCode, groups: &[ResourceGroup]) -> bool {
        match key {
            KeyCode::Up | KeyCode::Char('k') => self.navigate(ServiceNavigation::Up, groups),
            KeyCode::Down | KeyCode::Char('j') => self.navigate(ServiceNavigation::Down, groups),
            KeyCode::PageUp => self.navigate(ServiceNavigation::PageUp, groups),
            KeyCode::PageDown => self.navigate(ServiceNavigation::PageDown, groups),
            KeyCode::Home => self.navigate(ServiceNavigation::Home, groups),
            KeyCode::End => self.navigate(ServiceNavigation::End, groups),
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle_selected_group(groups),
            _ => return false,
        }
        true
    }

    fn selected_service_action<'a>(
        &self,
        key: KeyCode,
        groups: &'a [ResourceGroup],
    ) -> Option<(SelectedServiceAction, &'a Service)> {
        let action = match key {
            KeyCode::Char('t') => SelectedServiceAction::Trigger,
            KeyCode::Char('e') => SelectedServiceAction::ToggleEnabled,
            KeyCode::Char('l') => SelectedServiceAction::Logs,
            KeyCode::Char('a') => SelectedServiceAction::Actions,
            _ => return None,
        };
        let selected = self.inner.selected()?;
        match self.visible_rows(groups).get(selected)? {
            ServiceListRow::Service(service) => Some((action, service)),
            ServiceListRow::Group(_) => None,
        }
    }
}

enum ServiceListRow<'a> {
    Group(&'a ResourceGroup),
    Service(&'a Service),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverallStatus {
    Unavailable,
    Stopped,
    Starting,
    Running,
    Failed,
}

#[derive(Debug)]
pub struct DashboardModel {
    pub project: Project,
    pub state_dir: PathBuf,
    pub services: Vec<Service>,
    pub groups: Vec<ResourceGroup>,
    overall_status: OverallStatus,
    warning: Option<String>,
}

impl DashboardModel {
    pub fn new(project: Project, state_dir: PathBuf) -> Self {
        let (overall_status, warning) = if project.tiltfile.is_some() {
            (OverallStatus::Stopped, None)
        } else {
            (
                OverallStatus::Unavailable,
                Some("No Tiltfile found in this workspace".to_owned()),
            )
        };
        Self {
            project,
            state_dir,
            services: Vec::new(),
            groups: Vec::new(),
            overall_status,
            warning,
        }
    }

    pub fn overall_status(&self) -> OverallStatus {
        self.overall_status
    }

    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    pub fn can_start(&self) -> bool {
        self.project.tiltfile.is_some()
            && matches!(
                self.overall_status,
                OverallStatus::Stopped | OverallStatus::Failed
            )
    }

    pub fn can_stop(&self) -> bool {
        self.project.tiltfile.is_some()
            && matches!(
                self.overall_status,
                OverallStatus::Starting | OverallStatus::Running | OverallStatus::Failed
            )
    }

    fn active_port(&self) -> u16 {
        load_session(&self.project, &self.state_dir)
            .filter(|session| session.phase == SessionPhase::Running)
            .map_or(DEFAULT_TILT_PORT, |session| session.port)
    }

    pub fn refresh_with_tilt(&mut self, tilt: impl AsRef<std::ffi::OsStr>) -> Result<()> {
        let Some(project_tiltfile) = self.project.tiltfile.as_ref() else {
            return Ok(());
        };
        let session = load_session(&self.project, &self.state_dir);
        if let Some(session) = session.as_ref()
            && session.phase == SessionPhase::Exited
        {
            self.services.clear();
            self.groups.clear();
            self.overall_status = if session.exit_code.is_some_and(|code| code != 0) {
                OverallStatus::Failed
            } else {
                OverallStatus::Stopped
            };
            self.warning = session
                .exit_code
                .filter(|code| *code != 0)
                .map(|code| format!("Tilt exited with status {code}"));
            return Ok(());
        }

        let (port, expected_pid, managed) = session
            .as_ref()
            .map_or((DEFAULT_TILT_PORT, None, false), |session| {
                (session.port, Some(session.tilt_pid), true)
            });
        let expected_tiltfile = session
            .as_ref()
            .map_or(project_tiltfile, |session| &session.tiltfile);

        let identity_output = Command::new(&tilt)
            .args(["get", "sessions", "-o", "json", "--port", &port.to_string()])
            .output()
            .context("query Tilt Session")?;
        if !identity_output.status.success() {
            if !managed {
                self.overall_status = OverallStatus::Stopped;
                self.warning = None;
                self.services.clear();
                self.groups.clear();
                return Ok(());
            }
            self.overall_status = OverallStatus::Starting;
            self.warning = Some("Waiting for the Tilt API".to_owned());
            bail!(
                "Tilt API is not ready: {}",
                String::from_utf8_lossy(&identity_output.stderr).trim()
            );
        }
        let identity = parse_session_identity(&String::from_utf8_lossy(&identity_output.stdout))?;
        let reported_tiltfile = identity
            .tiltfile
            .canonicalize()
            .unwrap_or(identity.tiltfile);
        if reported_tiltfile != *expected_tiltfile
            || expected_pid.is_some_and(|pid| identity.pid != pid)
        {
            self.overall_status = if managed {
                OverallStatus::Failed
            } else {
                OverallStatus::Stopped
            };
            self.services.clear();
            self.groups.clear();
            self.warning = Some(if managed {
                "Tilt API belongs to another Tiltfile or process".to_owned()
            } else {
                "Tilt's default port belongs to another workspace".to_owned()
            });
            bail!("Tilt API port belongs to another Tiltfile or process");
        }

        let output = Command::new(&tilt)
            .args([
                "get",
                "uiresources",
                "-o",
                "json",
                "--port",
                &port.to_string(),
            ])
            .output()
            .context("query Tilt UIResources")?;
        if !output.status.success() {
            self.overall_status = OverallStatus::Starting;
            self.warning = Some("Waiting for the Tilt API".to_owned());
            bail!(
                "Tilt API is not ready: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let mut snapshot = parse_ui_resources(&String::from_utf8_lossy(&output.stdout))?;
        let buttons = Command::new(tilt.as_ref())
            .args([
                "get",
                "uibuttons",
                "-o",
                "json",
                "--port",
                &port.to_string(),
            ])
            .output()
            .context("query Tilt UIButtons")?;
        if buttons.status.success() {
            let actions = parse_ui_buttons(&String::from_utf8_lossy(&buttons.stdout))?;
            attach_service_actions(&mut snapshot, actions);
        }
        self.services = snapshot.services;
        self.groups = snapshot.groups;
        self.warning = snapshot.tiltfile_error;
        self.overall_status = OverallStatus::Running;
        Ok(())
    }

    pub fn start_with_herdr(&mut self, herdr: impl AsRef<std::ffi::OsStr>) -> Result<()> {
        if !self.can_start() {
            bail!("Tilt cannot be started in the current dashboard state");
        }
        let output = Command::new(herdr)
            .args(["plugin", "action", "invoke", "herdr.tilt.run"])
            .output()
            .context("invoke retained Tilt action")?;
        if !output.status.success() {
            bail!(
                "Herdr could not start Tilt: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        self.overall_status = OverallStatus::Starting;
        self.warning = None;
        Ok(())
    }

    pub fn stop_with_binary(&mut self, binary: impl AsRef<std::ffi::OsStr>) -> Result<()> {
        if !self.can_stop() {
            bail!("Tilt cannot be stopped in the current dashboard state");
        }
        let output = Command::new(binary)
            .arg("down")
            .output()
            .context("stop retained Tilt session")?;
        if !output.status.success() {
            bail!(
                "Could not stop Tilt: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        self.overall_status = OverallStatus::Stopped;
        self.services.clear();
        self.groups.clear();
        self.warning = None;
        Ok(())
    }

    pub fn trigger_service_with_tilt(
        &self,
        tilt: impl AsRef<std::ffi::OsStr>,
        service_name: &str,
    ) -> Result<()> {
        if self.overall_status != OverallStatus::Running
            || !self
                .services
                .iter()
                .any(|service| service.name == service_name)
        {
            bail!("Tilt service cannot be triggered in the current dashboard state");
        }
        let port = self.active_port();
        let output = Command::new(tilt)
            .args(["trigger", service_name, "--port", &port.to_string()])
            .output()
            .with_context(|| format!("trigger Tilt service {service_name}"))?;
        if !output.status.success() {
            bail!(
                "Could not trigger {service_name}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    pub fn toggle_service_with_tilt(
        &self,
        tilt: impl AsRef<std::ffi::OsStr>,
        service: &Service,
    ) -> Result<()> {
        if self.overall_status != OverallStatus::Running
            || !self
                .services
                .iter()
                .any(|candidate| candidate.name == service.name)
        {
            bail!("Tilt service cannot be toggled in the current dashboard state");
        }
        let action = if service.disabled {
            "enable"
        } else {
            "disable"
        };
        let port = self.active_port();
        let output = Command::new(tilt)
            .args([action, &service.name, "--port", &port.to_string()])
            .output()
            .with_context(|| format!("{action} Tilt service {}", service.name))?;
        if !output.status.success() {
            bail!(
                "Could not {action} {}: {}",
                service.name,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    fn set_warning(&mut self, warning: impl Into<String>) {
        self.warning = Some(warning.into());
    }
}

struct LogView {
    service_name: String,
    buffer: LogBuffer,
    stream: TiltLogStream,
    running: bool,
}

impl LogView {
    fn open(tilt: impl AsRef<std::ffi::OsStr>, service_name: String, port: u16) -> Result<Self> {
        Ok(Self {
            stream: TiltLogStream::spawn(tilt, &service_name, port)?,
            service_name,
            buffer: LogBuffer::default(),
            running: true,
        })
    }

    fn poll(&mut self) {
        self.stream.poll_into(&mut self.buffer, 256);
        self.running = self.stream.is_running();
    }
}

pub fn run_from_env() -> Result<()> {
    let context_json =
        env::var("HERDR_PLUGIN_CONTEXT_JSON").context("HERDR_PLUGIN_CONTEXT_JSON is not set")?;
    let context = InvocationContext::from_json(&context_json)?;
    let project = resolve_project(&context)?;
    let state_dir = env::var("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .context("HERDR_PLUGIN_STATE_DIR is not set")?;
    let tilt = env::var("TILT_BIN_PATH").unwrap_or_else(|_| "tilt".to_owned());
    let herdr = env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_owned());
    let binary = env::current_exe().context("resolve herdr-tilt executable")?;
    let mut model = DashboardModel::new(project, state_dir);
    let mut terminal = TerminalGuard::enter()?;
    let mut last_refresh = Instant::now() - Duration::from_secs(2);
    let mut confirm_down = false;
    let mut show_help = false;
    let mut service_list = ServiceListState::default();
    let mut log_view: Option<LogView> = None;
    let mut action_picker: Option<ActionPicker> = None;
    let mut pending_action: Option<ServiceAction> = None;

    loop {
        if let Some(view) = log_view.as_mut() {
            view.poll();
            terminal.terminal.draw(|frame| render_logs(frame, view))?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            let page_size = usize::from(terminal.terminal.size()?.height.saturating_sub(3).max(1));
            if handle_log_key(&mut view.buffer, key.code, page_size) {
                log_view = None;
                last_refresh = Instant::now() - Duration::from_secs(2);
            }
            continue;
        }
        if let Some(picker) = action_picker.as_mut() {
            terminal
                .terminal
                .draw(|frame| render_action_picker(frame, picker, pending_action.is_some()))?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if pending_action.is_some() {
                match down_confirmation_decision(key.code) {
                    Some(DownConfirmationDecision::Confirm) => {
                        let action = pending_action.take().expect("pending action exists");
                        if let Err(error) = activate_service_action(&action, model.active_port()) {
                            model.set_warning(error.to_string());
                        }
                        last_refresh = Instant::now() - Duration::from_secs(2);
                    }
                    Some(DownConfirmationDecision::Cancel) => pending_action = None,
                    None => {}
                }
                continue;
            }
            match picker.handle_key(key.code) {
                ActionPickerEvent::Back => action_picker = None,
                ActionPickerEvent::Activate(action) if action.requires_confirmation() => {
                    pending_action = Some(action)
                }
                ActionPickerEvent::Activate(action) => {
                    if let Err(error) = activate_service_action(&action, model.active_port()) {
                        model.set_warning(error.to_string());
                    }
                    last_refresh = Instant::now() - Duration::from_secs(2);
                }
                ActionPickerEvent::None => {}
            }
            continue;
        }
        if last_refresh.elapsed() >= Duration::from_secs(1) {
            if let Err(error) = model.refresh_with_tilt(&tilt)
                && model.overall_status() != OverallStatus::Starting
            {
                model.set_warning(error.to_string());
            }
            last_refresh = Instant::now();
        }
        service_list.sync(&model.groups);
        terminal
            .terminal
            .draw(|frame| render(frame, &model, confirm_down, show_help, &mut service_list))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if confirm_down {
            match down_confirmation_decision(key.code) {
                Some(DownConfirmationDecision::Confirm) => {
                    confirm_down = false;
                    if let Err(error) = model.stop_with_binary(&binary) {
                        model.set_warning(error.to_string());
                    }
                    last_refresh = Instant::now() - Duration::from_secs(2);
                }
                Some(DownConfirmationDecision::Cancel) => confirm_down = false,
                None => {}
            }
            continue;
        }
        if toggle_help_for_key(key.code, &mut show_help) {
            continue;
        }
        if service_list.handle_key(key.code, &model.groups) {
            continue;
        }
        if let Some((action, service)) =
            service_list.selected_service_action(key.code, &model.groups)
        {
            let service = service.clone();
            confirm_down = false;
            let result = match action {
                SelectedServiceAction::Trigger => {
                    model.trigger_service_with_tilt(&tilt, &service.name)
                }
                SelectedServiceAction::ToggleEnabled => {
                    model.toggle_service_with_tilt(&tilt, &service)
                }
                SelectedServiceAction::Logs => {
                    match LogView::open(&tilt, service.name.clone(), model.active_port()) {
                        Ok(view) => log_view = Some(view),
                        Err(error) => model.set_warning(error.to_string()),
                    }
                    continue;
                }
                SelectedServiceAction::Actions => {
                    match service.actions.as_slice() {
                        [] => model.set_warning(format!("{} has no actions", service.name)),
                        [action] if !action.requires_confirmation() => {
                            if let Err(error) = activate_service_action(action, model.active_port())
                            {
                                model.set_warning(error.to_string());
                            }
                        }
                        actions => {
                            action_picker =
                                Some(ActionPicker::new(service.name.clone(), actions.to_vec()));
                        }
                    }
                    last_refresh = Instant::now() - Duration::from_secs(2);
                    continue;
                }
            };
            if let Err(error) = result {
                model.set_warning(error.to_string());
            }
            last_refresh = Instant::now() - Duration::from_secs(2);
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Char('u') if model.can_start() => {
                confirm_down = false;
                if let Err(error) = model.start_with_herdr(&herdr) {
                    model.set_warning(error.to_string());
                }
            }
            KeyCode::Char('d') if model.can_stop() => confirm_down = true,
            KeyCode::Char('r') => {
                confirm_down = false;
                last_refresh = Instant::now() - Duration::from_secs(2);
            }
            _ => confirm_down = false,
        }
    }
    Ok(())
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enable terminal raw mode")?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error).context("enter alternate screen");
        }
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

fn handle_log_key(buffer: &mut LogBuffer, key: KeyCode, page_size: usize) -> bool {
    match key {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Up | KeyCode::Char('k') => buffer.navigate(LogNavigation::Up, page_size),
        KeyCode::Down | KeyCode::Char('j') => buffer.navigate(LogNavigation::Down, page_size),
        KeyCode::PageUp => buffer.navigate(LogNavigation::PageUp, page_size),
        KeyCode::PageDown => buffer.navigate(LogNavigation::PageDown, page_size),
        KeyCode::Home | KeyCode::Char('g') => buffer.navigate(LogNavigation::Home, page_size),
        KeyCode::End | KeyCode::Char('G') => buffer.navigate(LogNavigation::End, page_size),
        KeyCode::Left | KeyCode::Char('h') => buffer.navigate(LogNavigation::Left, page_size),
        KeyCode::Right | KeyCode::Char('l') => buffer.navigate(LogNavigation::Right, page_size),
        KeyCode::Char('f') => buffer.toggle_follow(),
        KeyCode::Char('w') => buffer.toggle_wrap(),
        KeyCode::Char('c') => buffer.clear(),
        _ => {}
    }
    false
}

fn render_logs(frame: &mut ratatui::Frame<'_>, view: &LogView) {
    let footer_lines = log_shortcut_legend(frame.area().width, view.buffer.is_wrapping());
    let footer_height = u16::try_from(footer_lines.len()).unwrap_or(u16::MAX).max(1);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(footer_height),
        ])
        .split(frame.area());

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("● ", Style::default().fg(Color::Cyan)),
            Span::styled(
                view.service_name.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" logs", Style::default().fg(Color::DarkGray)),
        ])),
        chunks[0],
    );
    let stream_label = if view.running { "live" } else { "ended" };
    let stream_color = if view.running {
        Color::Green
    } else {
        Color::Red
    };
    let follow_label = if view.buffer.is_following() {
        "follow"
    } else {
        "paused"
    };
    let wrap_label = if view.buffer.is_wrapping() {
        "wrap"
    } else {
        "nowrap"
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(stream_label, Style::default().fg(stream_color)),
            Span::styled(" │ ", Style::default().fg(Color::Rgb(72, 72, 72))),
            Span::styled(follow_label, Style::default().fg(Color::Gray)),
            Span::styled(" │ ", Style::default().fg(Color::Rgb(72, 72, 72))),
            Span::styled(wrap_label, Style::default().fg(Color::Gray)),
            Span::styled(" │ ", Style::default().fg(Color::Rgb(72, 72, 72))),
            Span::styled(
                format!("{} lines", view.buffer.len()),
                Style::default().fg(Color::DarkGray),
            ),
        ]))
        .alignment(Alignment::Right),
        chunks[0],
    );

    let body_height = usize::from(chunks[1].height);
    let visible = view.buffer.visible_lines(body_height);
    if visible.is_empty() {
        let message = if view.running {
            "Waiting for logs…"
        } else {
            view.stream.last_error().unwrap_or("Log stream ended")
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(Color::DarkGray)),
            chunks[1],
        );
    } else {
        let mut lines = visible
            .into_iter()
            .flat_map(styled_log_lines)
            .collect::<Vec<_>>();
        if view.buffer.is_following() && lines.len() > body_height {
            lines.drain(..lines.len() - body_height);
        }
        let body_width = usize::from(chunks[1].width).max(1);
        let wrapped_height = lines
            .iter()
            .map(|line| line.width().max(1).div_ceil(body_width))
            .sum::<usize>();
        let mut logs = Paragraph::new(Text::from(lines));
        if view.buffer.is_wrapping() {
            logs = logs.wrap(Wrap { trim: false });
            if view.buffer.is_following() {
                let scroll = wrapped_height.saturating_sub(body_height);
                logs = logs.scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0));
            }
        } else {
            logs = logs.scroll((0, view.buffer.horizontal_offset()));
        }
        frame.render_widget(logs, chunks[1]);
    }

    frame.render_widget(
        Paragraph::new(Text::from(footer_lines)).wrap(Wrap { trim: true }),
        chunks[2],
    );
}

fn styled_log_lines(line: &str) -> Vec<Line<'static>> {
    let lowercase = line.to_ascii_lowercase();
    let style = if lowercase.contains("fatal")
        || lowercase.contains("panic")
        || lowercase.contains("error")
        || lowercase.contains("failed")
    {
        Style::default().fg(Color::Red)
    } else if lowercase.contains("warn") {
        Style::default().fg(Color::Yellow)
    } else if lowercase.contains("debug") || lowercase.contains("trace") {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::Gray)
    };
    let (prefix, message) = line
        .split_once(" │ ")
        .map_or((None, line), |(prefix, message)| (Some(prefix), message));
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(message.trim())
        && (json.is_object() || json.is_array())
        && let Ok(pretty) = serde_json::to_string_pretty(&json)
    {
        return pretty
            .lines()
            .enumerate()
            .map(|(index, json_line)| {
                let mut spans = Vec::new();
                if index == 0
                    && let Some(prefix) = prefix
                {
                    spans.push(Span::styled(
                        prefix.to_owned(),
                        Style::default().fg(Color::DarkGray),
                    ));
                    spans.push(Span::styled(
                        " │ ",
                        Style::default().fg(Color::Rgb(72, 72, 72)),
                    ));
                } else if prefix.is_some() {
                    spans.push(Span::raw("  "));
                }
                spans.push(Span::styled(json_line.to_owned(), style));
                Line::from(spans)
            })
            .collect();
    }
    if let Some(prefix) = prefix {
        vec![Line::from(vec![
            Span::styled(prefix.to_owned(), Style::default().fg(Color::DarkGray)),
            Span::styled(" │ ", Style::default().fg(Color::Rgb(72, 72, 72))),
            Span::styled(message.to_owned(), style),
        ])]
    } else {
        vec![Line::from(Span::styled(line.to_owned(), style))]
    }
}

fn log_shortcut_legend(width: u16, wrapping: bool) -> Vec<Line<'static>> {
    let entries = [
        ("↑/↓/j/k", "scroll", true),
        ("pg↑/pg↓", "page", true),
        ("home/end", "jump", true),
        ("f", "follow", true),
        ("w", "wrap", true),
        ("h/l", "horizontal", !wrapping),
        ("c", "clear", true),
        ("q/esc", "back", true),
    ];
    let separator = Span::styled(" │ ", Style::default().fg(Color::Rgb(72, 72, 72)));
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut line_width = 0;
    for (key, label, enabled) in entries {
        let key_color = if enabled {
            Color::Gray
        } else {
            Color::DarkGray
        };
        let entry = vec![
            Span::styled(key, Style::default().fg(key_color)),
            Span::styled(format!(" {label}"), Style::default().fg(Color::DarkGray)),
        ];
        let entry_width = Line::from(entry.clone()).width();
        let separator_width = if spans.is_empty() {
            0
        } else {
            separator.width()
        };
        if !spans.is_empty() && line_width + separator_width + entry_width > usize::from(width) {
            lines.push(Line::from(std::mem::take(&mut spans)));
            line_width = 0;
        }
        if !spans.is_empty() {
            spans.push(separator.clone());
            line_width += separator.width();
        }
        spans.extend(entry);
        line_width += entry_width;
    }
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
}

fn render_action_picker(
    frame: &mut ratatui::Frame<'_>,
    picker: &mut ActionPicker,
    confirming: bool,
) {
    let footer = if confirming {
        action_confirmation_footer(
            frame.area().width,
            picker
                .selected()
                .map(ServiceAction::label)
                .unwrap_or("action"),
        )
    } else {
        wrap_shortcuts(
            frame.area().width,
            &[
                ("↑/↓/j/k", "nav", true),
                ("↵/space", "open", true),
                ("q/esc", "back", true),
            ],
        )
    };
    let footer_height = u16::try_from(footer.len()).unwrap_or(u16::MAX).max(1);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(footer_height),
        ])
        .split(frame.area());
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                picker.service_name.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" actions", Style::default().fg(Color::DarkGray)),
        ])),
        chunks[0],
    );
    let items = picker.actions.iter().map(|action| {
        let (icon, detail) = match action {
            ServiceAction::Link { url, .. } => ("↗ ", url.as_str()),
            ServiceAction::Button {
                requires_confirmation,
                ..
            } => (
                "▶ ",
                if *requires_confirmation {
                    "confirmation required"
                } else {
                    "Tilt action"
                },
            ),
        };
        ListItem::new(Line::from(vec![
            Span::styled(icon, Style::default().fg(Color::Cyan)),
            Span::styled(
                action.label().to_owned(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {detail}"), Style::default().fg(Color::DarkGray)),
        ]))
    });
    frame.render_stateful_widget(
        List::new(items).highlight_style(Style::default().bg(SERVICE_SELECTION_BG)),
        chunks[1],
        &mut picker.state,
    );
    frame.render_widget(
        Paragraph::new(Text::from(footer)).wrap(Wrap { trim: true }),
        chunks[2],
    );
}

fn action_confirmation_footer(width: u16, label: &str) -> Vec<Line<'static>> {
    let prompt = format!("Run {label}?");
    let entries = [
        (prompt.as_str(), "", true),
        ("y", "yes", true),
        ("n", "no", true),
    ];
    wrap_shortcuts(width, &entries)
}

fn wrap_shortcuts(width: u16, entries: &[(&str, &str, bool)]) -> Vec<Line<'static>> {
    let separator = Span::styled(" │ ", Style::default().fg(Color::Rgb(72, 72, 72)));
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut line_width = 0;
    for (key, label, enabled) in entries {
        let entry = vec![
            Span::styled(
                (*key).to_owned(),
                Style::default().fg(if *enabled {
                    Color::Gray
                } else {
                    Color::DarkGray
                }),
            ),
            Span::styled(
                if label.is_empty() {
                    String::new()
                } else {
                    format!(" {label}")
                },
                Style::default().fg(Color::DarkGray),
            ),
        ];
        let entry_width = Line::from(entry.clone()).width();
        let separator_width = if spans.is_empty() {
            0
        } else {
            separator.width()
        };
        if !spans.is_empty() && line_width + separator_width + entry_width > usize::from(width) {
            lines.push(Line::from(std::mem::take(&mut spans)));
            line_width = 0;
        }
        if !spans.is_empty() {
            spans.push(separator.clone());
            line_width += separator.width();
        }
        spans.extend(entry);
        line_width += entry_width;
    }
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
}

fn render(
    frame: &mut ratatui::Frame<'_>,
    model: &DashboardModel,
    confirm_down: bool,
    show_help: bool,
    service_list: &mut ServiceListState,
) {
    let has_banner = model.warning().is_some();
    let metric_lines = service_metric_lines(model, frame.area().width);
    let metrics_height = u16::try_from(metric_lines.len()).unwrap_or(u16::MAX).max(1);
    let header_height = metrics_height;
    let metrics = Paragraph::new(Text::from(metric_lines));
    let footer_lines = if confirm_down {
        down_confirmation_footer(frame.area().width)
    } else {
        shortcut_legend(model, frame.area().width, show_help)
    };
    let footer_height = u16::try_from(footer_lines.len()).unwrap_or(u16::MAX).max(1);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if has_banner {
            vec![
                Constraint::Length(header_height),
                Constraint::Length(3),
                Constraint::Min(2),
                Constraint::Length(footer_height),
            ]
        } else {
            vec![
                Constraint::Length(header_height),
                Constraint::Length(0),
                Constraint::Min(2),
                Constraint::Length(footer_height),
            ]
        })
        .split(frame.area());

    frame.render_widget(metrics, chunks[0]);
    frame.render_widget(
        Paragraph::new(Span::styled(
            overall_label(model.overall_status()),
            Style::default().fg(overall_color(model.overall_status())),
        ))
        .alignment(Alignment::Right),
        Rect::new(chunks[0].x, chunks[0].y, chunks[0].width, 1),
    );

    if has_banner {
        frame.render_widget(
            Paragraph::new(model.warning().unwrap_or_default())
                .style(Style::default().fg(Color::Yellow))
                .wrap(Wrap { trim: true })
                .block(Block::default().borders(Borders::ALL).title("Warning")),
            chunks[1],
        );
    }

    let visible_rows = service_list.visible_rows(&model.groups);
    let items = if visible_rows.is_empty() {
        vec![ListItem::new(empty_message(model.overall_status()))]
    } else {
        visible_rows
            .iter()
            .enumerate()
            .map(|(index, row)| match row {
                ServiceListRow::Group(group) => {
                    let disclosure = if service_list.collapsed.contains(&group.name) {
                        "▸ "
                    } else {
                        "▾ "
                    };
                    let group_style = Style::default()
                        .fg(circle_color(group.status))
                        .add_modifier(Modifier::BOLD);
                    ListItem::new(Line::from(vec![
                        Span::styled(disclosure, group_style),
                        Span::styled("● ", group_style),
                        Span::styled(group.name.clone(), group_style),
                        Span::styled(
                            format!(" ({})", group.services.len()),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]))
                }
                ServiceListRow::Service(service) => ListItem::new(service_line(
                    service,
                    chunks[2].width,
                    service_list.inner.selected() == Some(index),
                )),
            })
            .collect()
    };
    frame.render_stateful_widget(
        List::new(items).highlight_style(Style::default().bg(SERVICE_SELECTION_BG)),
        chunks[2],
        &mut service_list.inner,
    );

    frame.render_widget(
        Paragraph::new(Text::from(footer_lines)).wrap(Wrap { trim: true }),
        chunks[3],
    );
}

fn service_line(service: &Service, width: u16, selected: bool) -> Line<'static> {
    let badge = (!service.actions.is_empty()).then(|| {
        let badge_limit = usize::from(width).saturating_sub(10).max(1);
        if selected && badge_limit > 2 {
            let titles = service
                .actions
                .iter()
                .map(ServiceAction::label)
                .collect::<Vec<_>>()
                .join(" · ");
            format!("↗ {}", clip_with_ellipsis(&titles, badge_limit - 2))
        } else if selected || service.actions.len() == 1 {
            "↗".to_owned()
        } else {
            format!("↗ {}", service.actions.len())
        }
    });
    let reserved = badge.as_ref().map_or(0, |badge| badge.chars().count() + 1);
    let left_width = usize::from(width).saturating_sub(reserved);
    let name_width = left_width.saturating_sub(4);
    let name = clip_with_ellipsis(&service.name, name_width);
    let detail_width = left_width.saturating_sub(4 + name.chars().count());
    let detail = if detail_width >= 3 {
        clip_with_ellipsis(&format!("  {}", service.detail), detail_width)
    } else {
        String::new()
    };
    let mut spans = vec![
        Span::raw("  "),
        Span::styled("● ", Style::default().fg(circle_color(service.status))),
        Span::styled(name, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(detail),
    ];
    if let Some(badge) = badge {
        let content_width = Line::from(spans.clone()).width();
        let padding = usize::from(width).saturating_sub(content_width + badge.chars().count());
        spans.push(Span::raw(" ".repeat(padding)));
        if selected && badge.starts_with("↗ ") {
            spans.push(Span::styled("↗", Style::default().fg(Color::Cyan)));
            spans.push(Span::styled(
                badge.trim_start_matches('↗').to_owned(),
                Style::default().fg(Color::DarkGray),
            ));
        } else {
            spans.push(Span::styled(badge, Style::default().fg(Color::Cyan)));
        }
    }
    Line::from(spans)
}

fn clip_with_ellipsis(value: &str, width: usize) -> String {
    let length = value.chars().count();
    if length <= width {
        value.to_owned()
    } else if width == 0 {
        String::new()
    } else if width == 1 {
        "…".to_owned()
    } else {
        let mut clipped = value.chars().take(width - 1).collect::<String>();
        clipped.push('…');
        clipped
    }
}

fn service_metric_lines(model: &DashboardModel, width: u16) -> Vec<Line<'static>> {
    let mut healthy = 0;
    let mut building = 0;
    let mut failed = 0;
    let mut inactive = 0;
    for service in &model.services {
        match service.status {
            CircleStatus::Green => healthy += 1,
            CircleStatus::Orange => building += 1,
            CircleStatus::Red => failed += 1,
            CircleStatus::Grey => inactive += 1,
        }
    }
    let values = [
        ("Services", model.services.len()),
        ("Healthy", healthy),
        ("Building", building),
        ("Failed", failed),
        ("Inactive", inactive),
    ];
    let entries = values.into_iter().map(|(label, value)| {
        vec![
            Span::styled(format!("{label}: "), Style::default().fg(Color::DarkGray)),
            Span::styled(value.to_string(), Style::default().fg(Color::Gray)),
        ]
    });

    let separator = Span::styled(" │ ", Style::default().fg(Color::Rgb(72, 72, 72)));
    let full_width = usize::from(width);
    let status_width = overall_label(model.overall_status()).len();
    let mut line_capacity = full_width.saturating_sub(status_width + 1);
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut line_width = 0;
    for entry in entries {
        let entry_width = Line::from(entry.clone()).width();
        if spans.is_empty() && lines.is_empty() && entry_width > line_capacity {
            lines.push(Line::default());
            line_capacity = full_width;
        }
        let separator_width = if spans.is_empty() {
            0
        } else {
            separator.width()
        };
        if !spans.is_empty() && line_width + separator_width + entry_width > line_capacity {
            lines.push(Line::from(std::mem::take(&mut spans)));
            line_width = 0;
            line_capacity = full_width;
        }
        if !spans.is_empty() {
            spans.push(separator.clone());
            line_width += separator.width();
        }
        spans.extend(entry);
        line_width += entry_width;
    }
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
}

fn shortcut_legend(model: &DashboardModel, width: u16, show_help: bool) -> Vec<Line<'static>> {
    let navigation_enabled = !model.groups.is_empty();
    let entries = if show_help {
        vec![
            ("↑/↓/j/k", "nav", navigation_enabled),
            ("pg↑/pg↓", "page", navigation_enabled),
            ("home/end", "jump", navigation_enabled),
            ("↵/space", "toggle", navigation_enabled),
            ("t", "trigger", navigation_enabled),
            ("e", "enable/disable", navigation_enabled),
            ("l", "logs", navigation_enabled),
            ("a", "actions", navigation_enabled),
            ("u", "up", model.can_start()),
            ("d", "down", model.can_stop()),
            ("r", "refresh", true),
            ("?", "help", true),
            ("q/esc", "close", true),
        ]
    } else {
        vec![
            ("↑/↓", "nav", navigation_enabled),
            ("↵/space", "toggle", navigation_enabled),
            ("t", "trigger", navigation_enabled),
            ("e", "enable/disable", navigation_enabled),
            ("l", "logs", navigation_enabled),
            ("a", "actions", navigation_enabled),
            ("u", "up", model.can_start()),
            ("d", "down", model.can_stop()),
            ("r", "refresh", true),
            ("?", "help", true),
            ("q", "close", true),
        ]
    };
    let separator = Span::styled(" │ ", Style::default().fg(Color::Rgb(72, 72, 72)));
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut line_width = 0;

    for (key, label, enabled) in entries {
        let key_color = if enabled {
            Color::Gray
        } else {
            Color::DarkGray
        };
        let entry = vec![
            Span::styled(key, Style::default().fg(key_color)),
            Span::styled(format!(" {label}"), Style::default().fg(Color::DarkGray)),
        ];
        let entry_width = Line::from(entry.clone()).width();
        let separator_width = if spans.is_empty() {
            0
        } else {
            separator.width()
        };

        if !spans.is_empty() && line_width + separator_width + entry_width > usize::from(width) {
            lines.push(Line::from(std::mem::take(&mut spans)));
            line_width = 0;
        }
        if !spans.is_empty() {
            spans.push(separator.clone());
            line_width += separator.width();
        }
        spans.extend(entry);
        line_width += entry_width;
    }
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
}

fn down_confirmation_footer(width: u16) -> Vec<Line<'static>> {
    let entries = [
        vec![Span::styled(
            "Stop Tilt and clean up its resources?",
            Style::default().fg(Color::Yellow),
        )],
        vec![
            Span::styled("y", Style::default().fg(Color::Gray)),
            Span::styled(" yes", Style::default().fg(Color::DarkGray)),
        ],
        vec![
            Span::styled("n", Style::default().fg(Color::Gray)),
            Span::styled(" no", Style::default().fg(Color::DarkGray)),
        ],
    ];
    let separator = Span::styled(" │ ", Style::default().fg(Color::Rgb(72, 72, 72)));
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut line_width = 0;

    for entry in entries {
        let entry_width = Line::from(entry.clone()).width();
        let separator_width = if spans.is_empty() {
            0
        } else {
            separator.width()
        };
        if !spans.is_empty() && line_width + separator_width + entry_width > usize::from(width) {
            lines.push(Line::from(std::mem::take(&mut spans)));
            line_width = 0;
        }
        if !spans.is_empty() {
            spans.push(separator.clone());
            line_width += separator.width();
        }
        spans.extend(entry);
        line_width += entry_width;
    }
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
}

fn overall_label(status: OverallStatus) -> &'static str {
    match status {
        OverallStatus::Unavailable => "unavailable",
        OverallStatus::Stopped => "stopped",
        OverallStatus::Starting => "starting",
        OverallStatus::Running => "running",
        OverallStatus::Failed => "failed",
    }
}

fn overall_color(status: OverallStatus) -> Color {
    match status {
        OverallStatus::Running => Color::Green,
        OverallStatus::Starting => Color::Rgb(255, 165, 0),
        OverallStatus::Failed => Color::Red,
        OverallStatus::Unavailable | OverallStatus::Stopped => Color::DarkGray,
    }
}

fn circle_color(status: CircleStatus) -> Color {
    match status {
        CircleStatus::Green => Color::Green,
        CircleStatus::Orange => Color::Rgb(255, 165, 0),
        CircleStatus::Red => Color::Red,
        CircleStatus::Grey => Color::DarkGray,
    }
}

fn empty_message(status: OverallStatus) -> &'static str {
    match status {
        OverallStatus::Unavailable => "No Tiltfile found in this workspace.",
        OverallStatus::Stopped => "Tilt is stopped. Press u to start it.",
        OverallStatus::Starting => "Waiting for the Tilt API…",
        OverallStatus::Running => "Tilt is running but has no visible services.",
        OverallStatus::Failed => "Tilt exited. Check the warning and plugin logs.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn group(name: &str, service: &str) -> crate::tilt::ResourceGroup {
        crate::tilt::ResourceGroup {
            name: name.to_owned(),
            status: CircleStatus::Green,
            services: vec![Service {
                name: service.to_owned(),
                status: CircleStatus::Green,
                detail: "healthy".to_owned(),
                disabled: false,
                actions: vec![],
            }],
        }
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn service_list_can_scroll_to_the_last_item_in_a_small_panel() {
        let project = Project {
            root: PathBuf::from("/project"),
            tiltfile: Some(PathBuf::from("/project/Tiltfile")),
        };
        let mut model = DashboardModel::new(project, PathBuf::from("/state"));
        model.overall_status = OverallStatus::Running;
        model.groups = vec![ResourceGroup {
            name: "services".to_owned(),
            status: CircleStatus::Green,
            services: (0..20)
                .map(|index| Service {
                    name: format!("service-{index}"),
                    status: CircleStatus::Green,
                    detail: "healthy".to_owned(),
                    disabled: false,
                    actions: vec![],
                })
                .collect(),
        }];
        let mut list_state = ServiceListState::default();
        list_state.navigate(ServiceNavigation::End, &model.groups);
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();

        terminal
            .draw(|frame| render(frame, &model, false, false, &mut list_state))
            .unwrap();

        let rendered = terminal.backend().buffer().content().to_vec();
        let selected_cell = rendered
            .iter()
            .find(|cell| cell.symbol() == "9")
            .expect("selected service is rendered");
        assert_eq!(selected_cell.bg, SERVICE_SELECTION_BG);
        assert!(!selected_cell.modifier.contains(Modifier::REVERSED));
        let rendered = rendered
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("service-19"));
        assert!(!rendered.contains("service-0 "));
    }

    #[test]
    fn collapsed_group_stays_hidden_after_a_dashboard_refresh() {
        let project = Project {
            root: PathBuf::from("/project"),
            tiltfile: Some(PathBuf::from("/project/Tiltfile")),
        };
        let mut model = DashboardModel::new(project, PathBuf::from("/state"));
        model.overall_status = OverallStatus::Running;
        model.groups = vec![group("apps", "frontend"), group("infra", "postgres")];
        let mut list_state = ServiceListState::default();
        list_state.sync(&model.groups);
        list_state.toggle_selected_group(&model.groups);

        model.groups = vec![group("apps", "frontend"), group("infra", "postgres")];
        list_state.sync(&model.groups);
        let mut terminal = Terminal::new(TestBackend::new(60, 14)).unwrap();
        terminal
            .draw(|frame| render(frame, &model, false, false, &mut list_state))
            .unwrap();
        let rendered = buffer_text(&terminal);

        assert!(rendered.contains("apps"));
        assert!(!rendered.contains("frontend"));
        assert!(rendered.contains("infra"));
        assert!(rendered.contains("postgres"));
    }

    #[test]
    fn services_render_as_a_borderless_colored_hierarchy() {
        let project = Project {
            root: PathBuf::from("/project"),
            tiltfile: Some(PathBuf::from("/project/Tiltfile")),
        };
        let mut model = DashboardModel::new(project, PathBuf::from("/state"));
        model.overall_status = OverallStatus::Running;
        model.groups = vec![group("apps", "frontend")];
        model.services = model.groups[0].services.clone();
        let mut list_state = ServiceListState::default();
        list_state.sync(&model.groups);
        let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();

        terminal
            .draw(|frame| render(frame, &model, false, false, &mut list_state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let group_row = (0..100)
            .map(|x| buffer[(x, 1)].symbol())
            .collect::<String>();
        let service_row = (0..100)
            .map(|x| buffer[(x, 2)].symbol())
            .collect::<String>();

        assert!(group_row.starts_with("▾ ● apps (1)"));
        assert_eq!(buffer[(4, 1)].fg, circle_color(CircleStatus::Green));
        assert!(service_row.starts_with("  ● frontend  healthy"));
    }

    #[test]
    fn group_toggle_keys_handle_the_selected_header_without_arrow_aliases() {
        let groups = vec![group("apps", "frontend")];
        let mut list_state = ServiceListState::default();
        list_state.sync(&groups);

        assert!(list_state.handle_key(KeyCode::Char(' '), &groups));
        assert_eq!(list_state.visible_rows(&groups).len(), 1);
        assert!(list_state.handle_key(KeyCode::Enter, &groups));
        assert_eq!(list_state.visible_rows(&groups).len(), 2);
        assert!(!list_state.handle_key(KeyCode::Left, &groups));
        assert!(!list_state.handle_key(KeyCode::Right, &groups));
        assert!(!list_state.handle_key(KeyCode::Char('u'), &groups));
    }

    #[test]
    fn service_action_keys_target_only_the_selected_service() {
        let groups = vec![group("apps", "frontend")];
        let mut list_state = ServiceListState::default();
        list_state.sync(&groups);

        assert!(
            list_state
                .selected_service_action(KeyCode::Char('t'), &groups)
                .is_none()
        );

        list_state.navigate(ServiceNavigation::Down, &groups);
        let (action, service) = list_state
            .selected_service_action(KeyCode::Char('t'), &groups)
            .unwrap();
        assert_eq!(action, SelectedServiceAction::Trigger);
        assert_eq!(service.name, "frontend");
        assert_eq!(
            list_state
                .selected_service_action(KeyCode::Char('e'), &groups)
                .unwrap()
                .0,
            SelectedServiceAction::ToggleEnabled
        );
        assert_eq!(
            list_state
                .selected_service_action(KeyCode::Char('l'), &groups)
                .unwrap()
                .0,
            SelectedServiceAction::Logs
        );
        assert_eq!(
            list_state
                .selected_service_action(KeyCode::Char('a'), &groups)
                .unwrap()
                .0,
            SelectedServiceAction::Actions
        );
        assert!(
            list_state
                .selected_service_action(KeyCode::Char('x'), &groups)
                .is_none()
        );
    }

    #[test]
    fn service_rows_show_a_right_aligned_action_count() {
        let mut service = group("apps", "frontend").services.remove(0);
        service.actions = vec![
            ServiceAction::Link {
                label: "App".to_owned(),
                url: "https://app.test".to_owned(),
            },
            ServiceAction::Button {
                name: "seed".to_owned(),
                label: "Seed".to_owned(),
                requires_confirmation: false,
                inputs: vec![],
            },
        ];

        let line = service_line(&service, 40, false);
        let rendered = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(rendered.starts_with("  ● frontend  healthy"));
        assert!(rendered.ends_with("↗ 2"));
        assert_eq!(line.width(), 40);
    }

    #[test]
    fn action_indicator_survives_a_long_service_detail() {
        let mut service = group("apps", "frontend").services.remove(0);
        service.detail = "a very long build failure that cannot fit in this pane".to_owned();
        service.actions = vec![ServiceAction::Link {
            label: "App".to_owned(),
            url: "https://app.test".to_owned(),
        }];

        let line = service_line(&service, 24, false);
        let rendered = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(line.width(), 24);
        assert!(rendered.ends_with('↗'));
    }

    #[test]
    fn selected_service_row_previews_action_titles_next_to_the_icon() {
        let mut service = group("apps", "frontend").services.remove(0);
        service.actions = vec![
            ServiceAction::Link {
                label: "Open app".to_owned(),
                url: "https://app.test".to_owned(),
            },
            ServiceAction::Button {
                name: "seed".to_owned(),
                label: "Seed data".to_owned(),
                requires_confirmation: false,
                inputs: vec![],
            },
        ];

        let line = service_line(&service, 60, true);
        let rendered = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(line.width(), 60);
        assert!(rendered.ends_with("↗ Open app · Seed data"));
    }

    #[test]
    fn selected_action_preview_truncates_without_losing_the_icon() {
        let mut service = group("apps", "frontend").services.remove(0);
        service.detail = "healthy but with a long explanation".to_owned();
        service.actions = vec![ServiceAction::Link {
            label: "An extremely long action title".to_owned(),
            url: "https://app.test".to_owned(),
        }];

        let line = service_line(&service, 28, true);
        let rendered = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(line.width(), 28);
        assert!(rendered.contains("↗ "));
        assert!(rendered.ends_with('…'));
    }

    #[test]
    fn action_picker_navigates_selects_and_returns() {
        let actions = vec![
            ServiceAction::Link {
                label: "App".to_owned(),
                url: "https://app.test".to_owned(),
            },
            ServiceAction::Button {
                name: "seed".to_owned(),
                label: "Seed".to_owned(),
                requires_confirmation: false,
                inputs: vec![],
            },
        ];
        let mut picker = ActionPicker::new("frontend".to_owned(), actions.clone());

        assert_eq!(picker.handle_key(KeyCode::Down), ActionPickerEvent::None);
        assert_eq!(
            picker.handle_key(KeyCode::Enter),
            ActionPickerEvent::Activate(actions[1].clone())
        );
        assert_eq!(picker.handle_key(KeyCode::Esc), ActionPickerEvent::Back);
    }

    #[test]
    fn action_picker_replaces_the_dashboard_and_lists_action_kinds() {
        let mut picker = ActionPicker::new(
            "frontend".to_owned(),
            vec![
                ServiceAction::Link {
                    label: "Open app".to_owned(),
                    url: "https://app.test".to_owned(),
                },
                ServiceAction::Button {
                    name: "seed".to_owned(),
                    label: "Seed data".to_owned(),
                    requires_confirmation: true,
                    inputs: vec![],
                },
            ],
        );
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();

        terminal
            .draw(|frame| render_action_picker(frame, &mut picker, false))
            .unwrap();
        let rendered = buffer_text(&terminal);

        assert!(rendered.contains("frontend actions"));
        assert!(rendered.contains("↗ Open app"));
        assert!(rendered.contains("▶ Seed data"));
        assert!(rendered.contains("↵/space open"));
        assert!(rendered.contains("q/esc back"));
        assert!(!rendered.contains("Services:"));
    }

    #[test]
    fn action_confirmation_replaces_picker_footer() {
        let mut picker = ActionPicker::new(
            "frontend".to_owned(),
            vec![ServiceAction::Button {
                name: "seed".to_owned(),
                label: "Seed data".to_owned(),
                requires_confirmation: true,
                inputs: vec![],
            }],
        );
        let mut terminal = Terminal::new(TestBackend::new(60, 8)).unwrap();

        terminal
            .draw(|frame| render_action_picker(frame, &mut picker, true))
            .unwrap();
        let rendered = buffer_text(&terminal);

        assert!(rendered.contains("Run Seed data?"));
        assert!(rendered.contains("y yes"));
        assert!(rendered.contains("n no"));
        assert!(!rendered.contains("q/esc back"));
    }

    #[test]
    fn shortcut_legend_wraps_without_clipping_in_a_narrow_panel() {
        let project = Project {
            root: PathBuf::from("/project"),
            tiltfile: Some(PathBuf::from("/project/Tiltfile")),
        };
        let mut model = DashboardModel::new(project, PathBuf::from("/state"));
        model.overall_status = OverallStatus::Running;
        model.groups = vec![group("apps", "frontend")];
        let mut list_state = ServiceListState::default();
        list_state.sync(&model.groups);
        let mut terminal = Terminal::new(TestBackend::new(38, 18)).unwrap();

        terminal
            .draw(|frame| render(frame, &model, false, false, &mut list_state))
            .unwrap();
        let rendered = buffer_text(&terminal);

        for shortcut in [
            "↑/↓ nav",
            "↵/space toggle",
            "t trigger",
            "e enable/disable",
            "l logs",
            "a actions",
            "u up",
            "d down",
            "r refresh",
            "? help",
            "q close",
        ] {
            assert!(rendered.contains(shortcut), "missing shortcut: {shortcut}");
        }
        assert!(!rendered.contains("← collapse"));
        assert!(!rendered.contains("→ expand"));
        assert!(!rendered.contains("pg↑/pg↓ page"));
        assert!(!rendered.contains("home/end jump"));
        assert!(rendered.contains("frontend"));
    }

    #[test]
    fn help_footer_shows_all_keybinds_in_a_narrow_panel() {
        let project = Project {
            root: PathBuf::from("/project"),
            tiltfile: Some(PathBuf::from("/project/Tiltfile")),
        };
        let mut model = DashboardModel::new(project, PathBuf::from("/state"));
        model.overall_status = OverallStatus::Running;
        model.groups = vec![group("apps", "frontend")];
        model.services = model.groups[0].services.clone();
        let mut list_state = ServiceListState::default();
        list_state.sync(&model.groups);
        let mut terminal = Terminal::new(TestBackend::new(38, 22)).unwrap();

        terminal
            .draw(|frame| render(frame, &model, false, true, &mut list_state))
            .unwrap();
        let rendered = buffer_text(&terminal);

        for shortcut in [
            "↑/↓/j/k nav",
            "pg↑/pg↓ page",
            "home/end jump",
            "↵/space toggle",
            "t trigger",
            "e enable/disable",
            "l logs",
            "a actions",
            "u up",
            "d down",
            "r refresh",
            "? help",
            "q/esc close",
        ] {
            assert!(rendered.contains(shortcut), "missing shortcut: {shortcut}");
        }
        assert!(rendered.contains("frontend"));
    }

    #[test]
    fn question_mark_toggles_help_visibility() {
        let mut show_help = false;

        assert!(toggle_help_for_key(KeyCode::Char('?'), &mut show_help));
        assert!(show_help);
        assert!(toggle_help_for_key(KeyCode::Char('?'), &mut show_help));
        assert!(!show_help);
        assert!(!toggle_help_for_key(KeyCode::Char('t'), &mut show_help));
    }

    #[cfg(unix)]
    #[test]
    fn log_view_replaces_the_dashboard_with_pretty_live_output_and_controls() {
        let stream = TiltLogStream::spawn("/usr/bin/true", "api", 10350).unwrap();
        let mut view = LogView {
            service_name: "api".to_owned(),
            buffer: LogBuffer::with_limits(20, 1024),
            stream,
            running: true,
        };
        view.buffer.push("word ".repeat(100));
        view.buffer
            .push(r#"api │ {"level":"warn","message":"slow"}"#);
        view.buffer.push("api │ ERROR request failed");
        view.buffer.push("api │ TAIL");
        let mut terminal = Terminal::new(TestBackend::new(60, 14)).unwrap();

        terminal.draw(|frame| render_logs(frame, &view)).unwrap();
        let rendered = buffer_text(&terminal);
        let has_red_error = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| cell.symbol() == "E" && cell.fg == Color::Red);

        assert!(rendered.contains("api logs"));
        assert!(rendered.contains(r#""level": "warn""#));
        assert!(rendered.contains("f follow"));
        assert!(rendered.contains("w wrap"));
        assert!(rendered.contains("c clear"));
        assert!(rendered.contains("q/esc back"));
        assert!(rendered.contains("TAIL"));
        assert!(has_red_error);
        assert!(!rendered.contains("Services:"));
    }

    #[test]
    fn log_view_keys_control_navigation_display_and_back() {
        let mut buffer = LogBuffer::with_limits(10, 80);
        for line in ["one", "two", "three"] {
            buffer.push(line);
        }

        assert!(!handle_log_key(&mut buffer, KeyCode::Up, 2));
        assert!(!buffer.is_following());
        assert!(!handle_log_key(&mut buffer, KeyCode::Char('f'), 2));
        assert!(buffer.is_following());
        assert!(!handle_log_key(&mut buffer, KeyCode::Char('w'), 2));
        assert!(!buffer.is_wrapping());
        assert!(!handle_log_key(&mut buffer, KeyCode::Char('c'), 2));
        assert!(buffer.is_empty());
        assert!(handle_log_key(&mut buffer, KeyCode::Esc, 2));
        assert!(handle_log_key(&mut buffer, KeyCode::Char('q'), 2));
    }

    #[test]
    fn down_confirmation_replaces_the_shortcut_footer_with_yes_or_no() {
        let project = Project {
            root: PathBuf::from("/project"),
            tiltfile: Some(PathBuf::from("/project/Tiltfile")),
        };
        let mut model = DashboardModel::new(project, PathBuf::from("/state"));
        model.overall_status = OverallStatus::Running;
        model.groups = vec![group("apps", "frontend")];
        model.services = model.groups[0].services.clone();
        let mut list_state = ServiceListState::default();
        list_state.sync(&model.groups);
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();

        terminal
            .draw(|frame| render(frame, &model, true, false, &mut list_state))
            .unwrap();
        let rendered = buffer_text(&terminal);

        assert!(rendered.contains("Stop Tilt and clean up its resources?"));
        assert!(rendered.contains("y yes"));
        assert!(rendered.contains("n no"));
        assert!(!rendered.contains("q close"));
        assert!(!rendered.contains("Press d again"));
    }

    #[test]
    fn down_confirmation_requires_an_explicit_yes_or_no() {
        assert_eq!(
            down_confirmation_decision(KeyCode::Char('y')),
            Some(DownConfirmationDecision::Confirm)
        );
        assert_eq!(
            down_confirmation_decision(KeyCode::Char('n')),
            Some(DownConfirmationDecision::Cancel)
        );
        assert_eq!(down_confirmation_decision(KeyCode::Char('d')), None);
        assert_eq!(down_confirmation_decision(KeyCode::Esc), None);
    }

    #[test]
    fn compact_header_shows_status_and_unique_service_totals_without_a_card() {
        let project = Project {
            root: PathBuf::from("/project"),
            tiltfile: Some(PathBuf::from("/project/Tiltfile")),
        };
        let mut model = DashboardModel::new(project, PathBuf::from("/state"));
        model.overall_status = OverallStatus::Running;
        model.services = [
            ("healthy", CircleStatus::Green),
            ("building", CircleStatus::Orange),
            ("failed", CircleStatus::Red),
            ("inactive", CircleStatus::Grey),
        ]
        .into_iter()
        .map(|(name, status)| Service {
            name: name.to_owned(),
            status,
            detail: name.to_owned(),
            disabled: false,
            actions: vec![],
        })
        .collect();
        model.groups = vec![ResourceGroup {
            name: "all".to_owned(),
            status: CircleStatus::Red,
            services: model.services.clone(),
        }];
        let mut list_state = ServiceListState::default();
        list_state.sync(&model.groups);
        let mut terminal = Terminal::new(TestBackend::new(80, 14)).unwrap();

        terminal
            .draw(|frame| render(frame, &model, false, false, &mut list_state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = buffer_text(&terminal);

        let first_row = (0..80).map(|x| buffer[(x, 0)].symbol()).collect::<String>();
        let status = (73..80)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<String>();
        assert_eq!(buffer[(0, 0)].symbol(), "S");
        assert_eq!(status, "running");
        assert!(!rendered.contains("Tilt"));
        assert!(first_row.contains("Services: 4"));
        assert!(first_row.contains("running"));
        assert!(rendered.contains("Services: 4"));
        assert!(rendered.contains("Healthy: 1"));
        assert!(rendered.contains("Building: 1"));
        assert!(rendered.contains("Failed: 1"));
        assert!(rendered.contains("Inactive: 1"));
    }

    #[test]
    fn compact_header_metrics_wrap_without_clipping_in_a_narrow_panel() {
        let project = Project {
            root: PathBuf::from("/project"),
            tiltfile: Some(PathBuf::from("/project/Tiltfile")),
        };
        let mut model = DashboardModel::new(project, PathBuf::from("/state"));
        model.overall_status = OverallStatus::Running;
        model.services = [
            CircleStatus::Green,
            CircleStatus::Orange,
            CircleStatus::Red,
            CircleStatus::Grey,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, status)| Service {
            name: format!("service-{index}"),
            status,
            detail: "status".to_owned(),
            disabled: false,
            actions: vec![],
        })
        .collect();
        model.groups = vec![ResourceGroup {
            name: "all".to_owned(),
            status: CircleStatus::Red,
            services: model.services.clone(),
        }];
        let mut list_state = ServiceListState::default();
        list_state.sync(&model.groups);
        let mut terminal = Terminal::new(TestBackend::new(38, 18)).unwrap();

        terminal
            .draw(|frame| render(frame, &model, false, false, &mut list_state))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let first_row = (0..38).map(|x| buffer[(x, 0)].symbol()).collect::<String>();
        let status = (31..38)
            .map(|x| buffer[(x, 0)].symbol())
            .collect::<String>();
        let rendered = buffer_text(&terminal);

        assert_eq!(buffer[(0, 0)].symbol(), "S");
        assert_eq!(status, "running");
        assert!(first_row.contains("running"));
        assert!(first_row.contains("Services: 4"));
        for metric in [
            "Services: 4",
            "Healthy: 1",
            "Building: 1",
            "Failed: 1",
            "Inactive: 1",
            "running",
        ] {
            assert!(rendered.contains(metric), "missing metric: {metric}");
        }
        assert!(rendered.contains("service-0"));
    }
}
