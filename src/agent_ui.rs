use crate::terminal::Terminal;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SPINNER_FRAMES: &[&str] = &["✶", "✷", "✸", "✹", "✺", "✹", "✸", "✷"];
const FLAVOR_MESSAGES: &[&str] = &[
    "Reading context...",
    "Thinking...",
    "Making a plan...",
    "Checking the workspace...",
    "Preparing tools...",
    "Shaping the answer...",
];
/// How many spinner frames before rotating to the next flavor message.
const FRAMES_PER_FLAVOR: usize = 20;
const DEFAULT_STREAM_WIDTH: usize = 100;

pub struct TurnRenderer<'a> {
    terminal: &'a Terminal,
    started: Instant,
    first_content_at: Option<Instant>,
    state: TurnState,
    interrupted: Arc<AtomicBool>,
    spinner_frame: usize,
    last_spinner_render: Option<Instant>,
    flavor_index: usize,
    prompt_text: String,
    thinking_visible: bool,
    status_block_rendered: bool,
    output_column: usize,
    stream_started: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnState {
    Thinking,
    Streaming,
    Done,
}

impl<'a> TurnRenderer<'a> {
    pub fn new(terminal: &'a Terminal, prompt: &str) -> Self {
        Self {
            terminal,
            started: Instant::now(),
            first_content_at: None,
            state: TurnState::Thinking,
            interrupted: Arc::new(AtomicBool::new(false)),
            spinner_frame: 0,
            last_spinner_render: None,
            flavor_index: 0,
            prompt_text: prompt.to_string(),
            thinking_visible: true,
            status_block_rendered: false,
            output_column: 0,
            stream_started: false,
        }
    }

    pub fn is_interrupted(&self) -> bool {
        self.interrupted.load(Ordering::Relaxed)
    }

    /// Call this once before streaming starts to render the submitted prompt
    /// and show the initial spinner.
    pub fn start_thinking(&mut self) {
        if !self.prompt_text.is_empty() {
            println!();
            println!(
                "{} {}",
                self.terminal.muted(">"),
                self.terminal.prompt_highlight(&self.prompt_text)
            );
            println!();
        }

        self.render_status_block();
    }

    /// Called for each delta received from the streaming response.
    pub fn on_delta(&mut self, delta: &str) {
        if self.state == TurnState::Thinking {
            self.transition_to_streaming();
        }

        let delta = self.display_delta(delta);
        if delta.is_empty() {
            return;
        }

        let width = stream_width();
        let wrapped = wrap_stream_delta(delta, &mut self.output_column, width);
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let _ = write!(out, "{wrapped}");
        let _ = out.flush();
    }

    /// Render spinner with current timing info. Call this periodically
    /// (e.g., every 100ms) during the thinking phase.
    pub fn tick(&mut self) {
        if self.state != TurnState::Thinking {
            return;
        }

        let now = Instant::now();
        if let Some(last) = self.last_spinner_render {
            if now.duration_since(last) < Duration::from_millis(100) {
                return;
            }
        }

        self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
        // Rotate the flavor message every N spinner frames
        if self.spinner_frame.is_multiple_of(FRAMES_PER_FLAVOR) {
            self.flavor_index = (self.flavor_index + 1) % FLAVOR_MESSAGES.len();
        }
        self.clear_status_block();
        self.render_status_block();
        self.last_spinner_render = Some(now);
    }

    /// Check for ESC key press without blocking.
    pub fn poll_interrupt(&mut self) -> bool {
        if self.interrupted.load(Ordering::Relaxed) {
            return true;
        }

        if event::poll(Duration::from_millis(0)).unwrap_or(false) {
            if let Ok(Event::Key(key)) = event::read() {
                if matches!(key.kind, KeyEventKind::Press) && key.code == KeyCode::Esc {
                    self.interrupted.store(true, Ordering::Relaxed);
                    return true;
                }
                if matches!(key.kind, KeyEventKind::Press) && key.code == KeyCode::Tab {
                    self.thinking_visible = !self.thinking_visible;
                    if self.state == TurnState::Thinking {
                        self.clear_status_block();
                        self.render_status_block();
                    }
                }
            }
        }
        false
    }

    /// Call when the model response is complete.
    pub fn finish(&mut self) {
        match self.state {
            TurnState::Thinking => {
                self.clear_status_block();
                let elapsed = self.started.elapsed();
                println!(
                    "{}",
                    self.terminal.thinking_indicator(&format!(
                        "∴ Waited {:.1}s with no response",
                        elapsed.as_secs_f64()
                    ))
                );
            }
            TurnState::Streaming => {
                println!(); // Newline after streamed content
            }
            TurnState::Done => {}
        }
        self.state = TurnState::Done;
    }

    /// Call when the model produced a hidden action such as a tool call.
    pub fn finish_action(&mut self) {
        match self.state {
            TurnState::Thinking => {
                self.clear_status_block();
                let thinking_secs = self.started.elapsed().as_secs_f64();
                println!(
                    "{}",
                    self.terminal
                        .thinking_indicator(&format!("∴ Thought for {:.0}s", thinking_secs))
                );
            }
            TurnState::Streaming => {
                println!();
            }
            TurnState::Done => {}
        }
        self.state = TurnState::Done;
    }

    /// Call when the model response was interrupted.
    pub fn finish_interrupted(&mut self) {
        match self.state {
            TurnState::Thinking => {
                self.clear_status_block();
            }
            TurnState::Streaming => {
                println!();
            }
            _ => {}
        }
        println!(
            "{}",
            self.terminal.thinking_indicator("⏎ Interrupted by user")
        );
        self.state = TurnState::Done;
    }

    fn transition_to_streaming(&mut self) {
        self.first_content_at = Some(Instant::now());
        self.clear_status_block();
        self.output_column = 0;
        self.stream_started = false;

        // Show thinking time summary
        let thinking_secs = self.started.elapsed().as_secs_f64();
        if thinking_secs >= 0.5 {
            println!(
                "{}",
                self.terminal
                    .thinking_indicator(&format!("∴ Thought for {:.0}s", thinking_secs))
            );
            println!();
        }

        self.state = TurnState::Streaming;
    }

    fn render_status_block(&mut self) {
        let frame = SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()];
        let flavor = FLAVOR_MESSAGES[self.flavor_index % FLAVOR_MESSAGES.len()];
        let elapsed = self.started.elapsed();
        let secs = elapsed.as_secs();

        let lead = if self.thinking_visible {
            if secs >= 2 {
                "∴ Thought"
            } else {
                "∴ Thinking"
            }
        } else {
            "∴ Thinking hidden"
        };
        let interrupt_hint = if secs >= 2 {
            format!("esc to interrupt · {secs}s")
        } else {
            "esc to interrupt".to_string()
        };

        let width = stream_width();
        let status = format!(
            "{}  {} {} ({})",
            self.terminal.thinking_indicator(lead),
            self.terminal.spinner_text(&format!("{frame}")),
            self.terminal.spinner_text(flavor),
            self.terminal.token_stats(&interrupt_hint)
        );
        print!("\r{}\x1b[K", truncate_plain_status(&status, width));
        let _ = io::stdout().flush();
        self.status_block_rendered = true;
    }

    fn clear_status_block(&mut self) {
        if self.status_block_rendered {
            print!("\r\x1b[K");
            self.status_block_rendered = false;
        } else {
            print!("\r\x1b[K");
        }
        let _ = io::stdout().flush();
    }

    fn display_delta<'d>(&mut self, delta: &'d str) -> &'d str {
        if self.stream_started {
            return delta;
        }

        let trimmed = delta.trim_start_matches([' ', '\t', '\r', '\n']);
        if !trimmed.is_empty() {
            self.stream_started = true;
        }
        trimmed
    }
}

fn stream_width() -> usize {
    terminal::size()
        .ok()
        .map(|(width, _)| usize::from(width).saturating_sub(1).max(1))
        .unwrap_or(DEFAULT_STREAM_WIDTH)
}

fn wrap_stream_delta(delta: &str, column: &mut usize, width: usize) -> String {
    let width = width.max(1);
    let mut output = String::with_capacity(delta.len());

    for character in delta.chars() {
        match character {
            '\r' => {
                output.push(character);
                *column = 0;
            }
            '\n' => {
                output.push(character);
                *column = 0;
            }
            _ => {
                if *column >= width {
                    output.push('\n');
                    *column = 0;
                    if character == ' ' {
                        continue;
                    }
                }
                output.push(character);
                *column += 1;
            }
        }
    }

    output
}

fn truncate_plain_status(value: &str, width: usize) -> String {
    if visible_len(value) <= width {
        return value.to_string();
    }

    let mut output = String::new();
    let mut visible = 0usize;
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\x1b' {
            output.push(character);
            for next in chars.by_ref() {
                output.push(next);
                if next == 'm' {
                    break;
                }
            }
            continue;
        }
        if visible >= width.saturating_sub(1) {
            break;
        }
        output.push(character);
        visible += 1;
    }
    output.push('…');
    output.push_str("\x1b[0m");
    output
}

fn visible_len(value: &str) -> usize {
    let mut len = 0usize;
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\x1b' {
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
            }
        } else {
            len += 1;
        }
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_stream_delta_at_terminal_width() {
        let mut column = 0;
        let wrapped = wrap_stream_delta("hello world", &mut column, 6);

        assert_eq!(wrapped, "hello \nworld");
        assert_eq!(column, 5);
    }

    #[test]
    fn wraps_stream_delta_across_multiple_chunks() {
        let mut column = 4;
        let wrapped = wrap_stream_delta(" world", &mut column, 6);

        assert_eq!(wrapped, " w\norld");
        assert_eq!(column, 4);
    }

    #[test]
    fn truncates_status_without_counting_ansi_codes() {
        let status = "\x1b[2mThinking\x1b[0m with a long tail";
        let truncated = truncate_plain_status(status, 12);

        assert!(truncated.contains("\x1b[2m"));
        assert!(truncated.ends_with("\x1b[0m"));
        assert_eq!(visible_len(&truncated), 12);
    }
}
