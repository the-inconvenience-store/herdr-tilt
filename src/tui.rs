use std::collections::BTreeSet;
use std::env;
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::project::Project;
use crate::project::{InvocationContext, resolve_project};
use crate::session::{SessionPhase, load_session};
use crate::tilt::{
    CircleStatus, ResourceGroup, Service, parse_session_identity, parse_ui_resources,
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
        self.project.tiltfile.is_some() && self.overall_status != OverallStatus::Running
    }

    pub fn can_stop(&self) -> bool {
        self.project.tiltfile.is_some()
            && matches!(
                self.overall_status,
                OverallStatus::Starting | OverallStatus::Running | OverallStatus::Failed
            )
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

        let output = Command::new(tilt)
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
        let snapshot = parse_ui_resources(&String::from_utf8_lossy(&output.stdout))?;
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

    fn set_warning(&mut self, warning: impl Into<String>) {
        self.warning = Some(warning.into());
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
    let mut service_list = ServiceListState::default();

    loop {
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
            .draw(|frame| render(frame, &model, confirm_down, &mut service_list))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if service_list.handle_key(key.code, &model.groups) {
            confirm_down = false;
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
            KeyCode::Char('d') if model.can_stop() && confirm_down => {
                confirm_down = false;
                if let Err(error) = model.stop_with_binary(&binary) {
                    model.set_warning(error.to_string());
                }
                last_refresh = Instant::now() - Duration::from_secs(2);
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

fn render(
    frame: &mut ratatui::Frame<'_>,
    model: &DashboardModel,
    confirm_down: bool,
    service_list: &mut ServiceListState,
) {
    let has_banner = model.warning().is_some() || confirm_down;
    let metric_lines = service_metric_lines(model, frame.area().width);
    let metrics_height = u16::try_from(metric_lines.len()).unwrap_or(u16::MAX).max(1);
    let header_height = metrics_height;
    let metrics = Paragraph::new(Text::from(metric_lines));
    let shortcut_lines = shortcut_legend(model, frame.area().width);
    let shortcut_height = u16::try_from(shortcut_lines.len())
        .unwrap_or(u16::MAX)
        .max(1);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if has_banner {
            vec![
                Constraint::Length(header_height),
                Constraint::Length(3),
                Constraint::Min(2),
                Constraint::Length(shortcut_height),
            ]
        } else {
            vec![
                Constraint::Length(header_height),
                Constraint::Length(0),
                Constraint::Min(2),
                Constraint::Length(shortcut_height),
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
        let message = if confirm_down {
            "Press d again to stop Tilt and clean up its resources"
        } else {
            model.warning().unwrap_or_default()
        };
        frame.render_widget(
            Paragraph::new(message)
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
            .map(|row| match row {
                ServiceListRow::Group(group) => {
                    let disclosure = if service_list.collapsed.contains(&group.name) {
                        "▸ "
                    } else {
                        "▾ "
                    };
                    ListItem::new(Line::from(vec![
                        Span::raw(disclosure),
                        Span::styled("● ", Style::default().fg(circle_color(group.status))),
                        Span::styled(
                            group.name.clone(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("  {}", group.services.len()),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]))
                }
                ServiceListRow::Service(service) => ListItem::new(Line::from(vec![
                    Span::styled("   ● ", Style::default().fg(circle_color(service.status))),
                    Span::styled(
                        service.name.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("  {}", service.detail)),
                ])),
            })
            .collect()
    };
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Services"))
            .highlight_symbol("› ")
            .highlight_style(Style::default().bg(SERVICE_SELECTION_BG)),
        chunks[2],
        &mut service_list.inner,
    );

    frame.render_widget(
        Paragraph::new(Text::from(shortcut_lines)).wrap(Wrap { trim: true }),
        chunks[3],
    );
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

fn shortcut_legend(model: &DashboardModel, width: u16) -> Vec<Line<'static>> {
    let navigation_enabled = !model.groups.is_empty();
    let entries = [
        ("↑/↓", "nav", navigation_enabled),
        ("pg↑/pg↓", "page", navigation_enabled),
        ("home/end", "jump", navigation_enabled),
        ("↵/space", "toggle", navigation_enabled),
        ("u", "up", model.can_start()),
        ("d", "down", model.can_stop()),
        ("r", "refresh", true),
        ("q", "close", true),
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
                })
                .collect(),
        }];
        let mut list_state = ServiceListState::default();
        list_state.navigate(ServiceNavigation::End, &model.groups);
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();

        terminal
            .draw(|frame| render(frame, &model, false, &mut list_state))
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
            .draw(|frame| render(frame, &model, false, &mut list_state))
            .unwrap();
        let rendered = buffer_text(&terminal);

        assert!(rendered.contains("apps"));
        assert!(!rendered.contains("frontend"));
        assert!(rendered.contains("infra"));
        assert!(rendered.contains("postgres"));
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
            .draw(|frame| render(frame, &model, false, &mut list_state))
            .unwrap();
        let rendered = buffer_text(&terminal);

        for shortcut in [
            "↑/↓ nav",
            "↵/space toggle",
            "u up",
            "d down",
            "r refresh",
            "q close",
        ] {
            assert!(rendered.contains(shortcut), "missing shortcut: {shortcut}");
        }
        assert!(!rendered.contains("← collapse"));
        assert!(!rendered.contains("→ expand"));
        assert!(rendered.contains("frontend"));
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
            .draw(|frame| render(frame, &model, false, &mut list_state))
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
            .draw(|frame| render(frame, &model, false, &mut list_state))
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
