use std::env;
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::project::Project;
use crate::project::{InvocationContext, resolve_project};
use crate::session::{SessionPhase, load_session};
use crate::tilt::{CircleStatus, Service, parse_ui_resources};
use anyhow::{Context, Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

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
        let Some(session) = load_session(&self.project, &self.state_dir) else {
            if self.project.tiltfile.is_some() {
                self.overall_status = OverallStatus::Stopped;
                self.warning = None;
                self.services.clear();
            }
            return Ok(());
        };
        if session.phase == SessionPhase::Exited {
            self.services.clear();
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

        let output = Command::new(tilt)
            .args([
                "get",
                "uiresources",
                "-o",
                "json",
                "--port",
                &session.port.to_string(),
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

    loop {
        if last_refresh.elapsed() >= Duration::from_secs(1) {
            if let Err(error) = model.refresh_with_tilt(&tilt)
                && model.overall_status() != OverallStatus::Starting
            {
                model.set_warning(error.to_string());
            }
            last_refresh = Instant::now();
        }
        terminal
            .terminal
            .draw(|frame| render(frame, &model, confirm_down))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
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

fn render(frame: &mut ratatui::Frame<'_>, model: &DashboardModel, confirm_down: bool) {
    let has_banner = model.warning().is_some() || confirm_down;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if has_banner {
            vec![
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(2),
                Constraint::Length(2),
            ]
        } else {
            vec![
                Constraint::Length(3),
                Constraint::Length(0),
                Constraint::Min(2),
                Constraint::Length(2),
            ]
        })
        .split(frame.area());

    let title = Line::from(vec![
        Span::styled("Tilt ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(
            overall_label(model.overall_status()),
            Style::default().fg(overall_color(model.overall_status())),
        ),
        Span::raw(format!("  {}", model.project.root.display())),
    ]);
    frame.render_widget(
        Paragraph::new(title).block(Block::default().borders(Borders::ALL)),
        chunks[0],
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

    let items = if model.services.is_empty() {
        vec![ListItem::new(empty_message(model.overall_status()))]
    } else {
        model
            .services
            .iter()
            .map(|service| {
                ListItem::new(Line::from(vec![
                    Span::styled("● ", Style::default().fg(circle_color(service.status))),
                    Span::styled(
                        service.name.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("  {}", service.detail)),
                ]))
            })
            .collect()
    };
    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title("Services")),
        chunks[2],
    );

    let up = if model.can_start() {
        "u Up"
    } else {
        "u Up (disabled)"
    };
    let down = if model.can_stop() {
        "d Down"
    } else {
        "d Down (disabled)"
    };
    frame.render_widget(
        Paragraph::new(format!(" {up}   {down}   r Refresh   q Close"))
            .style(Style::default().fg(Color::DarkGray)),
        chunks[3],
    );
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
