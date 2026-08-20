use std::collections::VecDeque;
use std::ffi::OsStr;
use std::io::{self, BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result};

pub const DEFAULT_MAX_LOG_LINES: usize = 2_000;
pub const DEFAULT_MAX_LOG_LINE_BYTES: usize = 8 * 1024;
const LOG_CHANNEL_CAPACITY: usize = 128;
const LOG_POLL_BATCH_SIZE: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogNavigation {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Left,
    Right,
}

#[derive(Debug)]
pub struct LogBuffer {
    lines: VecDeque<String>,
    max_lines: usize,
    max_line_bytes: usize,
    dropped_lines: u64,
    truncated_lines: u64,
    offset_from_bottom: usize,
    following: bool,
    wrapping: bool,
    horizontal_offset: u16,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::with_limits(DEFAULT_MAX_LOG_LINES, DEFAULT_MAX_LOG_LINE_BYTES)
    }
}

impl LogBuffer {
    pub fn with_limits(max_lines: usize, max_line_bytes: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(max_lines),
            max_lines,
            max_line_bytes,
            dropped_lines: 0,
            truncated_lines: 0,
            offset_from_bottom: 0,
            following: true,
            wrapping: true,
            horizontal_offset: 0,
        }
    }

    pub fn push(&mut self, line: impl AsRef<str>) {
        self.push_received(line.as_ref(), false);
    }

    fn push_received(&mut self, line: &str, already_truncated: bool) {
        if self.max_lines == 0 {
            self.dropped_lines = self.dropped_lines.saturating_add(1);
            return;
        }
        let mut line = strip_terminal_sequences(line.trim_end_matches(['\r', '\n']));
        if already_truncated {
            self.truncated_lines = self.truncated_lines.saturating_add(1);
        }
        if line.len() > self.max_line_bytes {
            let mut boundary = self.max_line_bytes;
            while !line.is_char_boundary(boundary) {
                boundary -= 1;
            }
            line.truncate(boundary);
            self.truncated_lines = self.truncated_lines.saturating_add(1);
        }
        if self.lines.len() == self.max_lines {
            self.lines.pop_front();
            self.dropped_lines = self.dropped_lines.saturating_add(1);
        }
        self.lines.push_back(line);
        if !self.following {
            self.offset_from_bottom = self
                .offset_from_bottom
                .saturating_add(1)
                .min(self.lines.len());
        }
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }

    pub fn visible_lines(&self, height: usize) -> Vec<&str> {
        let end = self.lines.len().saturating_sub(self.offset_from_bottom);
        let start = end.saturating_sub(height);
        self.lines.range(start..end).map(String::as_str).collect()
    }

    pub fn navigate(&mut self, navigation: LogNavigation, page_size: usize) {
        let vertical = matches!(
            navigation,
            LogNavigation::Up
                | LogNavigation::Down
                | LogNavigation::PageUp
                | LogNavigation::PageDown
                | LogNavigation::Home
                | LogNavigation::End
        );
        match navigation {
            LogNavigation::Up => {
                self.offset_from_bottom = self.offset_from_bottom.saturating_add(1);
            }
            LogNavigation::Down => {
                self.offset_from_bottom = self.offset_from_bottom.saturating_sub(1);
            }
            LogNavigation::PageUp => {
                self.offset_from_bottom = self.offset_from_bottom.saturating_add(page_size);
            }
            LogNavigation::PageDown => {
                self.offset_from_bottom = self.offset_from_bottom.saturating_sub(page_size);
            }
            LogNavigation::Home => {
                self.offset_from_bottom = self
                    .lines
                    .len()
                    .saturating_sub(page_size.max(1).min(self.lines.len()));
            }
            LogNavigation::End => self.offset_from_bottom = 0,
            LogNavigation::Left if !self.wrapping => {
                self.horizontal_offset = self.horizontal_offset.saturating_sub(1);
            }
            LogNavigation::Right if !self.wrapping => {
                self.horizontal_offset = self.horizontal_offset.saturating_add(1);
            }
            LogNavigation::Left | LogNavigation::Right => {}
        }
        if vertical {
            let max_offset = self
                .lines
                .len()
                .saturating_sub(page_size.max(1).min(self.lines.len()));
            self.offset_from_bottom = self.offset_from_bottom.min(max_offset);
            self.following = self.offset_from_bottom == 0;
        }
    }

    pub fn is_following(&self) -> bool {
        self.following
    }

    pub fn toggle_follow(&mut self) {
        self.following = !self.following;
        if self.following {
            self.offset_from_bottom = 0;
        }
    }

    pub fn toggle_wrap(&mut self) {
        self.wrapping = !self.wrapping;
        if self.wrapping {
            self.horizontal_offset = 0;
        }
    }

    pub fn is_wrapping(&self) -> bool {
        self.wrapping
    }

    pub fn horizontal_offset(&self) -> u16 {
        self.horizontal_offset
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.dropped_lines = 0;
        self.truncated_lines = 0;
        self.offset_from_bottom = 0;
        self.following = true;
        self.horizontal_offset = 0;
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn dropped_lines(&self) -> u64 {
        self.dropped_lines
    }

    pub fn truncated_lines(&self) -> u64 {
        self.truncated_lines
    }
}

fn strip_terminal_sequences(input: &str) -> String {
    #[derive(Clone, Copy)]
    enum State {
        Text,
        Escape,
        ControlSequence,
        OperatingSystemCommand,
        OperatingSystemEscape,
    }

    let mut output = String::with_capacity(input.len());
    let mut state = State::Text;
    for character in input.chars() {
        state = match state {
            State::Text if character == '\u{1b}' => State::Escape,
            State::Text => {
                output.push(character);
                State::Text
            }
            State::Escape if character == '[' => State::ControlSequence,
            State::Escape if character == ']' => State::OperatingSystemCommand,
            State::Escape => State::Text,
            State::ControlSequence if ('@'..='~').contains(&character) => State::Text,
            State::ControlSequence => State::ControlSequence,
            State::OperatingSystemCommand if character == '\u{7}' => State::Text,
            State::OperatingSystemCommand if character == '\u{1b}' => State::OperatingSystemEscape,
            State::OperatingSystemCommand => State::OperatingSystemCommand,
            State::OperatingSystemEscape if character == '\\' => State::Text,
            State::OperatingSystemEscape => State::OperatingSystemCommand,
        };
    }
    output
}

enum LogMessage {
    Line {
        text: String,
        truncated: bool,
        stderr: bool,
    },
    ReaderClosed,
}

pub struct TiltLogStream {
    child: Option<Child>,
    receiver: Option<Receiver<LogMessage>>,
    readers: Vec<JoinHandle<()>>,
    open_readers: usize,
    last_error: Option<String>,
}

impl TiltLogStream {
    pub fn spawn(tilt: impl AsRef<OsStr>, service_name: &str, port: u16) -> Result<Self> {
        let mut child = Command::new(tilt)
            .args([
                "logs",
                service_name,
                "--follow",
                "--port",
                &port.to_string(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("stream logs for Tilt service {service_name}"))?;
        let stdout = child.stdout.take().context("capture Tilt log output")?;
        let stderr = child.stderr.take().context("capture Tilt log errors")?;
        let (sender, receiver) = mpsc::sync_channel(LOG_CHANNEL_CAPACITY);
        let readers = vec![
            spawn_reader(stdout, sender.clone(), false),
            spawn_reader(stderr, sender, true),
        ];
        Ok(Self {
            child: Some(child),
            receiver: Some(receiver),
            readers,
            open_readers: 2,
            last_error: None,
        })
    }

    pub fn poll_into(&mut self, logs: &mut LogBuffer, limit: usize) -> usize {
        let Some(receiver) = self.receiver.as_ref() else {
            return 0;
        };
        let mut received = 0;
        for _ in 0..limit.min(LOG_POLL_BATCH_SIZE) {
            match receiver.try_recv() {
                Ok(LogMessage::Line {
                    text,
                    truncated,
                    stderr,
                }) => {
                    if stderr {
                        self.last_error = Some(text.clone());
                    }
                    logs.push_received(&text, truncated);
                    received += 1;
                }
                Ok(LogMessage::ReaderClosed) => {
                    self.open_readers = self.open_readers.saturating_sub(1);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.open_readers = 0;
                    break;
                }
            }
        }
        received
    }

    pub fn is_running(&mut self) -> bool {
        if self.open_readers == 0 {
            return false;
        }
        self.child
            .as_mut()
            .is_some_and(|child| child.try_wait().ok().flatten().is_none())
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
}

impl Drop for TiltLogStream {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        drop(self.receiver.take());
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

fn spawn_reader(
    pipe: impl io::Read + Send + 'static,
    sender: SyncSender<LogMessage>,
    stderr: bool,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        loop {
            match read_bounded_line(&mut reader, DEFAULT_MAX_LOG_LINE_BYTES) {
                Ok(Some((text, truncated))) => {
                    if sender
                        .send(LogMessage::Line {
                            text,
                            truncated,
                            stderr,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = sender.send(LogMessage::Line {
                        text: format!("log stream read error: {error}"),
                        truncated: false,
                        stderr: true,
                    });
                    break;
                }
            }
        }
        let _ = sender.send(LogMessage::ReaderClosed);
    })
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    max_bytes: usize,
) -> io::Result<Option<(String, bool)>> {
    let mut bytes = Vec::with_capacity(max_bytes.min(1024));
    let mut saw_bytes = false;
    let mut truncated = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if !saw_bytes {
                return Ok(None);
            }
            break;
        }
        saw_bytes = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let content_len = newline.unwrap_or(available.len());
        let remaining = max_bytes.saturating_sub(bytes.len());
        let retained = content_len.min(remaining);
        bytes.extend_from_slice(&available[..retained]);
        truncated |= retained < content_len;
        let consumed = newline.map_or(available.len(), |index| index + 1);
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    Ok(Some((
        String::from_utf8_lossy(&bytes).into_owned(),
        truncated,
    )))
}
