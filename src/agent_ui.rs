use crate::terminal::Terminal;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const FLAVOR_MESSAGES: &[&str] = &[
    "Grooving...",
    "Thinking...",
    "Processing...",
    "Analyzing...",
];
/// How many spinner frames before rotating to the next flavor message.
const FRAMES_PER_FLAVOR: usize = 20;

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
            println!("{}", self.terminal.prompt_highlight(&self.prompt_text));
            println!();
        }

        // Show initial thinking spinner
        self.render_spinner_line();
    }

    /// Called for each delta received from the streaming response.
    pub fn on_delta(&mut self, delta: &str) {
        if self.state == TurnState::Thinking {
            self.transition_to_streaming();
        }

        // Write the delta content directly
        let stdout = io::stdout();
        let mut out = stdout.lock();
        let _ = write!(out, "{delta}");
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
        self.clear_spinner_line();
        self.render_spinner_line();
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
            }
        }
        false
    }

    /// Call when the model response is complete.
    pub fn finish(&mut self) {
        match self.state {
            TurnState::Thinking => {
                self.clear_spinner_line();
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

    /// Call when the model response was interrupted.
    pub fn finish_interrupted(&mut self) {
        match self.state {
            TurnState::Thinking => {
                self.clear_spinner_line();
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
        self.clear_spinner_line();

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

        // Print agent bullet prefix before streaming content
        print!("{} ", self.terminal.agent_bullet());
        let _ = io::stdout().flush();

        self.state = TurnState::Streaming;
    }

    fn render_spinner_line(&self) {
        let frame = SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()];
        let flavor = FLAVOR_MESSAGES[self.flavor_index % FLAVOR_MESSAGES.len()];
        let elapsed = self.started.elapsed();
        let secs = elapsed.as_secs();

        let status = if secs >= 2 {
            format!("esc to interrupt · {secs}s")
        } else {
            "esc to interrupt".to_string()
        };

        let line = format!(
            "{} {} ({})",
            self.terminal.spinner_text(frame),
            self.terminal.spinner_text(flavor),
            self.terminal.token_stats(&status)
        );
        print!("\r{line}\x1b[K");
        let _ = io::stdout().flush();
    }

    fn clear_spinner_line(&self) {
        print!("\r\x1b[K");
        let _ = io::stdout().flush();
    }
}
