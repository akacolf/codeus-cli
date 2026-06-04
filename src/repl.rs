use crate::api::{ClientConfig, CodeusClient};
use crate::auth;
use crate::config::Config;
use crate::harness::Session;
use crate::input::{self, PromptOutcome, Suggestion};
use crate::onboarding;
use crate::telemetry;
use crate::terminal::{font_label, theme_label, Terminal};
use crate::thread_store;
use crate::tool_protocol;
use crate::tools::{self, ToolObservation, ToolStatus};
use anyhow::{Context, Result};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const MAX_MODEL_TOOL_STEPS: usize = 4;

const COMMAND_SUGGESTIONS: &[Suggestion<'static>] = &[
    Suggestion {
        value: "/help",
        detail: "Show command list",
    },
    Suggestion {
        value: "/status",
        detail: "Show session details",
    },
    Suggestion {
        value: "/threads",
        detail: "List saved threads",
    },
    Suggestion {
        value: "/resume",
        detail: "Resume latest or named thread",
    },
    Suggestion {
        value: "/compact",
        detail: "Compact older context",
    },
    Suggestion {
        value: "/read",
        detail: "Read a workspace file",
    },
    Suggestion {
        value: "/search",
        detail: "Search text in workspace",
    },
    Suggestion {
        value: "/git",
        detail: "Run git status/diff",
    },
    Suggestion {
        value: "/run",
        detail: "Run approved command",
    },
    Suggestion {
        value: "/approvals",
        detail: "Show approved command prefixes",
    },
    Suggestion {
        value: "/trace",
        detail: "Show telemetry status",
    },
    Suggestion {
        value: "/theme",
        detail: "Change theme and font",
    },
    Suggestion {
        value: "/font",
        detail: "Change coding font",
    },
    Suggestion {
        value: "/login",
        detail: "Connect a QC account",
    },
    Suggestion {
        value: "/logout",
        detail: "Remove stored session",
    },
    Suggestion {
        value: "/cwd",
        detail: "Show or change directory",
    },
    Suggestion {
        value: "/model",
        detail: "Show active model",
    },
    Suggestion {
        value: "/api",
        detail: "Show API base",
    },
    Suggestion {
        value: "/clear",
        detail: "Clear this thread",
    },
    Suggestion {
        value: "/logo",
        detail: "Print Codeus logo",
    },
    Suggestion {
        value: "/exit",
        detail: "Leave chat",
    },
];

pub async fn run(mut client: CodeusClient) -> Result<()> {
    let stored = Config::load()?;
    let mut terminal = Terminal::detect_with_theme(stored.theme.as_deref());
    let mut session = Session::new()?;

    print_welcome(&terminal, &client, &session);

    loop {
        let (prompt, prompt_width) = prompt(&terminal, &session);
        let input =
            match input::read_line_with_suggestions(&prompt, prompt_width, COMMAND_SUGGESTIONS)? {
                PromptOutcome::Line(input) => input,
                PromptOutcome::Eof | PromptOutcome::Interrupted => break,
            };
        if input.is_empty() {
            continue;
        }

        if input.starts_with('/') {
            match handle_command(&input, &mut terminal, &mut client, &mut session).await? {
                CommandResult::Continue => continue,
                CommandResult::Exit => break,
            }
        }

        if client.config().token.is_none() {
            println!(
                "{} Sign in with /login before sending chat messages. The terminal is ready; auth only starts when you ask for it.",
                terminal.warn("!")
            );
            continue;
        }

        session.record_user(input.to_string())?;
        if let Some(dropped) = session.maybe_auto_compact()? {
            println!(
                "{}",
                terminal.muted(&format!("Compacted {dropped} older messages."))
            );
        }

        run_agent_turn(&client, &terminal, &mut session).await?;
    }

    Ok(())
}

async fn run_agent_turn(
    client: &CodeusClient,
    terminal: &Terminal,
    session: &mut Session,
) -> Result<()> {
    for step in 0..MAX_MODEL_TOOL_STEPS {
        let messages = session.messages_for_turn()?;
        let _ = telemetry::record(
            "turn_start",
            serde_json::json!({
                "thread_id": session.thread_id(),
                "model": client.config().model,
                "message_count": messages.len(),
                "step": step,
            }),
        );

        let started = Instant::now();
        let response = stream_model_response(client, terminal, &messages).await?;
        let elapsed_ms = started.elapsed().as_millis();
        let _ = telemetry::record(
            "turn_finish",
            serde_json::json!({
                "thread_id": session.thread_id(),
                "model": client.config().model,
                "elapsed_ms": elapsed_ms,
                "assistant_bytes": response.text.len(),
                "step": step,
            }),
        );

        match tool_protocol::parse_tool_call(&response.text) {
            _ if !response.tool_calls.is_empty() => {
                session.record_assistant_tool_calls(response.tool_calls.clone())?;
                let config = Config::load()?;
                for tool_call in response.tool_calls {
                    let call = tool_protocol::from_chat_tool_call(&tool_call)?;
                    println!(
                        "{}",
                        terminal.muted(&format!("codeus requested tool `{}`", call.name))
                    );
                    let observation =
                        tool_protocol::execute_tool_call(session.cwd(), &config, &call).await?;
                    print_observation(terminal, &observation);
                    session.record_tool_result(tool_call.id, observation.as_context_message())?;
                }
            }
            Ok(Some(call)) => {
                println!(
                    "{}",
                    terminal.muted(&format!("codeus requested tool `{}`", call.name))
                );
                let config = Config::load()?;
                let observation =
                    tool_protocol::execute_tool_call(session.cwd(), &config, &call).await?;
                print_observation(terminal, &observation);
                session.record_tool(&observation)?;
            }
            Ok(None) => {
                if !response.printed && !response.text.trim().is_empty() {
                    println!("{} {}", terminal.accent("codeus"), response.text);
                }
                session.record_assistant(response.text)?;
                return Ok(());
            }
            Err(error) => {
                if !response.printed && !response.text.trim().is_empty() {
                    println!("{} {}", terminal.accent("codeus"), response.text);
                }
                println!(
                    "{} Could not parse model tool call: {error}",
                    terminal.warn("!")
                );
                session.record_assistant(response.text)?;
                return Ok(());
            }
        }
    }

    println!(
        "{}",
        terminal.warn("Stopped after the model requested too many tools in one turn.")
    );
    Ok(())
}

struct ModelResponse {
    text: String,
    printed: bool,
    tool_calls: Vec<crate::api::ChatToolCall>,
}

async fn stream_model_response(
    client: &CodeusClient,
    terminal: &Terminal,
    messages: &[crate::api::Message],
) -> Result<ModelResponse> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut text = String::new();
    let mut printed = false;

    let completion = client
        .stream_chat_completion(messages, |delta| {
            text.push_str(delta);
            if printed {
                write!(out, "{delta}")?;
                out.flush()?;
                return Ok(());
            }

            let trimmed = text.trim_start();
            if tool_protocol::TOOL_CALL_OPEN.starts_with(trimmed)
                || trimmed.starts_with(tool_protocol::TOOL_CALL_OPEN)
            {
                return Ok(());
            }

            print!("{}", terminal.accent("codeus"));
            print!(" ");
            write!(out, "{text}")?;
            out.flush()?;
            printed = true;
            Ok(())
        })
        .await?;

    if printed {
        println!();
    }

    if text.is_empty() {
        text = completion.content.clone();
    }

    Ok(ModelResponse {
        text,
        printed,
        tool_calls: completion.tool_calls,
    })
}

enum CommandResult {
    Continue,
    Exit,
}

async fn handle_command(
    input: &str,
    terminal: &mut Terminal,
    client: &mut CodeusClient,
    session: &mut Session,
) -> Result<CommandResult> {
    let mut parts = input.split_whitespace();
    let command = parts.next().unwrap_or_default();

    match command {
        "/exit" | "/quit" => Ok(CommandResult::Exit),
        "/clear" | "/reset" => {
            session.reset()?;
            println!("{}", terminal.success("Thread cleared."));
            Ok(CommandResult::Continue)
        }
        "/" | "/commands" | "/help" | "/?" => {
            print_help(terminal);
            Ok(CommandResult::Continue)
        }
        "/status" => {
            print_status(terminal, client, session);
            Ok(CommandResult::Continue)
        }
        "/threads" => {
            print_threads(terminal)?;
            Ok(CommandResult::Continue)
        }
        "/resume" => {
            let id = parts.next();
            let resumed = match id {
                Some("latest") | None => Session::resume_latest_excluding(session.thread_id())?,
                Some(id) => Some(Session::resume(id)?),
            };

            if let Some(next) = resumed {
                std::env::set_current_dir(next.cwd())
                    .with_context(|| format!("failed to enter {}", next.cwd().display()))?;
                *session = next;
                println!(
                    "{}",
                    terminal.success(&format!("Resumed thread {}.", session.thread_id()))
                );
            } else {
                println!("{}", terminal.muted("No saved threads yet."));
            }
            Ok(CommandResult::Continue)
        }
        "/compact" => {
            let dropped = session.compact()?;
            if dropped == 0 {
                println!("{}", terminal.muted("Nothing to compact yet."));
            } else {
                println!(
                    "{}",
                    terminal.success(&format!("Compacted {dropped} older messages."))
                );
            }
            Ok(CommandResult::Continue)
        }
        "/read" => {
            let path = input[command.len()..].trim();
            if path.is_empty() {
                println!("{}", terminal.warn("Usage: /read <path>"));
            } else {
                let observation = tools::read_file(session.cwd(), path)?;
                print_observation(terminal, &observation);
                session.record_tool(&observation)?;
            }
            Ok(CommandResult::Continue)
        }
        "/search" => {
            let mut args = input[command.len()..].split_whitespace();
            let pattern = args.next();
            let root = args.next();
            if let Some(pattern) = pattern {
                let observation = tools::search_text(session.cwd(), pattern, root)?;
                print_observation(terminal, &observation);
                session.record_tool(&observation)?;
            } else {
                println!("{}", terminal.warn("Usage: /search <pattern> [path]"));
            }
            Ok(CommandResult::Continue)
        }
        "/git" => {
            let subcommand = parts.next().unwrap_or("status");
            let observation = match subcommand {
                "status" => tools::git_status(session.cwd()).await?,
                "diff" => tools::git_diff(session.cwd()).await?,
                _ => ToolObservation {
                    name: "git".to_string(),
                    args: serde_json::json!({ "subcommand": subcommand }),
                    status: ToolStatus::Denied,
                    output: "Usage: /git status | /git diff".to_string(),
                    elapsed_ms: 0,
                },
            };
            print_observation(terminal, &observation);
            session.record_tool(&observation)?;
            Ok(CommandResult::Continue)
        }
        "/run" => {
            let command_line = input[command.len()..].trim();
            if command_line.is_empty() {
                println!("{}", terminal.warn("Usage: /run <approved command>"));
            } else {
                let config = Config::load()?;
                let observation = tools::run_command(session.cwd(), &config, command_line).await?;
                print_observation(terminal, &observation);
                session.record_tool(&observation)?;
            }
            Ok(CommandResult::Continue)
        }
        "/approvals" => {
            let config = Config::load()?;
            println!("{}", terminal.accent("Approved command prefixes"));
            println!("  {}", tools::format_allowed_prefixes(&config));
            Ok(CommandResult::Continue)
        }
        "/trace" => {
            println!(
                "trace: {}",
                if telemetry::enabled() {
                    "enabled"
                } else {
                    "disabled"
                }
            );
            println!("trace file: {}", telemetry::trace_path()?.display());
            println!(
                "{}",
                terminal.muted("Set CODEUS_TRACE=1 to write local JSONL telemetry events.")
            );
            Ok(CommandResult::Continue)
        }
        "/theme" => {
            let mut config = Config::load()?;
            onboarding::choose_appearance(&mut config)?;
            *terminal = Terminal::detect_with_theme(config.theme.as_deref());
            Ok(CommandResult::Continue)
        }
        "/font" => {
            let mut config = Config::load()?;
            onboarding::choose_font(&mut config)?;
            Ok(CommandResult::Continue)
        }
        "/login" => {
            let no_browser = parts.any(|part| part == "--no-browser");
            login_from_repl(client, terminal, no_browser).await?;
            Ok(CommandResult::Continue)
        }
        "/logout" => {
            let mut config = Config::load()?;
            config.session_token = None;
            config.refresh_token = None;
            config.token_expires_at = None;
            config.save()?;
            let current = client.config().clone();
            *client = CodeusClient::new(ClientConfig {
                api_base: current.api_base,
                model: current.model,
                token: None,
            })?;
            println!("{}", terminal.success("Signed out."));
            Ok(CommandResult::Continue)
        }
        "/model" => {
            println!("model: {}", client.config().model);
            Ok(CommandResult::Continue)
        }
        "/api" => {
            println!("api: {}", client.config().api_base);
            Ok(CommandResult::Continue)
        }
        "/cwd" | "/cd" => {
            let path = input[command.len()..].trim();
            if path.is_empty() {
                println!("{}", session.cwd().display());
            } else {
                let next = expand_cwd(path).context("failed to resolve directory")?;
                std::env::set_current_dir(&next)
                    .with_context(|| format!("failed to enter {}", next.display()))?;
                session.set_cwd(next)?;
                println!("cwd: {}", session.cwd().display());
            }
            Ok(CommandResult::Continue)
        }
        "/logo" => {
            println!("{}", terminal.logo());
            Ok(CommandResult::Continue)
        }
        _ => {
            println!(
                "{} Unknown command `{}`. Type /help for commands.",
                terminal.warn("!"),
                command
            );
            Ok(CommandResult::Continue)
        }
    }
}

fn print_welcome(terminal: &Terminal, client: &CodeusClient, session: &Session) {
    let stored = Config::load().unwrap_or_default();
    let theme = stored
        .theme
        .as_deref()
        .and_then(theme_label)
        .unwrap_or("Auto");
    let font = font_label(stored.font.as_deref());
    let cwd = compact_path(session.cwd());

    let mut left = Vec::from([
        String::new(),
        "Welcome back to Codeus.".to_string(),
        String::new(),
    ]);
    left.extend([
        "   ____          __                ".to_string(),
        "  / ___|___   __| | ___ _   _ ___ ".to_string(),
        " | |   / _ \\ / _` |/ _ \\ | | / __|".to_string(),
        " | |__| (_) | (_| |  __/ |_| \\__ \\".to_string(),
        "  \\____\\___/ \\__,_|\\___|\\__,_|___/".to_string(),
    ]);
    left.extend([
        String::new(),
        format!("{} - {}", client.config().model, auth_label(client)),
        cwd,
        format!("{theme} - {font}"),
    ]);

    let mut right = Vec::new();
    right.push("Recent activity".to_string());
    right.extend(recent_activity_lines(session.thread_id()));
    right.push(String::new());
    right.push("What's new".to_string());
    right.push("/login connects a QC account".to_string());
    right.push("Native tools can read, search, and inspect git".to_string());
    right.push("/resume continues local saved threads".to_string());

    println!(
        "{}",
        terminal.accent(&format!(
            "Codeus CLI v{} {}",
            env!("CARGO_PKG_VERSION"),
            "-".repeat(58)
        ))
    );
    print_split_panel(terminal, &left, &right);
    println!();
    println!(
        "{}",
        terminal.muted("> Try \"explain this repo\" or type /help")
    );
    println!();
}

fn print_help(terminal: &Terminal) {
    println!("{}", terminal.accent("Codeus commands"));
    println!("  /help, /commands       Show this command list");
    println!("  /status                Show model, API base, cwd, and thread size");
    println!("  /threads               List saved local threads");
    println!("  /resume [id|latest]    Resume a saved thread");
    println!("  /compact               Compact older messages into summary context");
    println!("  /read <path>           Read a workspace file into the thread");
    println!("  /search <pattern> [path] Search workspace text");
    println!("  /git status|diff       Run safe git inspection commands");
    println!("  /run <command>         Run an approved command prefix");
    println!("  /approvals             Show approved command prefixes");
    println!("  /trace                 Show local telemetry status");
    println!("  /theme                 Change the terminal theme and font");
    println!("  /font                  Change the coding font preference");
    println!("  /login [--no-browser]  Connect a QC account");
    println!("  /logout                Remove the stored session");
    println!("  /cwd [path], /cd [path] Show or change the session working directory");
    println!("  /model                 Show the active model");
    println!("  /api                   Show the active API base");
    println!("  /clear, /reset         Clear this thread");
    println!("  /logo                  Print the Codeus logo");
    println!("  /exit, /quit           Leave chat");
    println!();
    println!(
        "{}",
        terminal.muted("Anything else is sent as a chat message.")
    );
}

fn print_observation(terminal: &Terminal, observation: &ToolObservation) {
    let label = match observation.status {
        ToolStatus::Success => terminal.success("tool ok"),
        ToolStatus::Failed => terminal.warn("tool failed"),
        ToolStatus::Denied => terminal.warn("tool denied"),
    };
    println!(
        "{label} {} ({}ms)",
        observation.name, observation.elapsed_ms
    );
    if !observation.output.trim().is_empty() {
        println!("{}", observation.output);
    }
    let _ = telemetry::record(
        "tool_observation",
        serde_json::json!({
            "name": observation.name,
            "status": observation.status,
            "elapsed_ms": observation.elapsed_ms,
            "output_bytes": observation.output.len(),
        }),
    );
}

fn print_status(terminal: &Terminal, client: &CodeusClient, session: &Session) {
    let stored = Config::load().unwrap_or_default();
    let theme = stored
        .theme
        .as_deref()
        .and_then(theme_label)
        .unwrap_or("Auto");
    let turns = session.transcript_len();
    let context_stats = session.context_stats();
    println!("{}", terminal.accent("Session"));
    println!("  thread: {}", session.thread_id());
    println!("  thread file: {}", session.thread_path().display());
    println!("  model: {}", client.config().model);
    println!("  auth: {}", auth_label(client));
    println!("  api: {}", client.config().api_base);
    println!("  cwd: {}", session.cwd().display());
    println!("  theme: {theme}");
    println!("  font: {}", font_label(stored.font.as_deref()));
    println!("  messages: {turns}");
    println!(
        "  AGENTS.md files: {} ({} bytes)",
        context_stats.agent_doc_count, context_stats.agent_doc_bytes
    );
    println!(
        "  compacted summary: {}",
        if session.summary().is_some() {
            "yes"
        } else {
            "no"
        }
    );
}

fn print_threads(terminal: &Terminal) -> Result<()> {
    let threads = thread_store::list_threads()?;
    if threads.is_empty() {
        println!("{}", terminal.muted("No saved threads yet."));
        return Ok(());
    }

    println!("{}", terminal.accent("Saved threads"));
    for thread in threads.into_iter().take(10) {
        println!(
            "  {:<24} updated={} size={} path={}",
            thread.id,
            thread.updated_at,
            thread.size,
            thread.path.display()
        );
    }
    Ok(())
}

async fn login_from_repl(
    client: &mut CodeusClient,
    terminal: &Terminal,
    no_browser: bool,
) -> Result<()> {
    let stored = Config::load()?;
    let current = client.config().clone();

    match auth::login_with_browser(
        Some(current.api_base.clone()),
        Some(current.model.clone()),
        &stored,
        no_browser,
    )
    .await
    {
        Ok(config) => {
            *client = CodeusClient::new(ClientConfig {
                api_base: current.api_base,
                model: current.model,
                token: config.session_token,
            })?;
            println!("{}", terminal.success("Connected."));
        }
        Err(error) => {
            println!(
                "{} Browser login could not start: {error}",
                terminal.warn("!")
            );
            println!(
                "{}",
                terminal.muted(
                    "Verify CODEUS_API_BASE points at https://api.quantumclub.sh/v1, or use `codeus login --token ...` for development/support."
                )
            );
        }
    }

    Ok(())
}

fn auth_label(client: &CodeusClient) -> &'static str {
    if client.config().token.is_some() {
        "connected"
    } else {
        "not connected"
    }
}

fn prompt(terminal: &Terminal, _session: &Session) -> (String, usize) {
    (format!("{} ", terminal.accent(">")), 2)
}

fn print_split_panel(terminal: &Terminal, left: &[String], right: &[String]) {
    const LEFT_WIDTH: usize = 39;
    const RIGHT_WIDTH: usize = 49;
    let border = format!(
        "+{}+{}+",
        "-".repeat(LEFT_WIDTH + 2),
        "-".repeat(RIGHT_WIDTH + 2)
    );
    println!("{}", terminal.accent(&border));

    let rows = left.len().max(right.len());
    for index in 0..rows {
        let left_value = left.get(index).map(String::as_str).unwrap_or_default();
        let right_value = right.get(index).map(String::as_str).unwrap_or_default();
        let left_cell = pad_cell(left_value, LEFT_WIDTH);
        let right_cell = pad_cell(right_value, RIGHT_WIDTH);

        let left_cell = if left_value.contains("___") || left_value.contains("| |") {
            terminal.accent(&left_cell)
        } else {
            left_cell
        };
        let right_cell = match right_value {
            "Recent activity" | "What's new" => terminal.accent(&right_cell),
            _ => right_cell,
        };

        println!(
            "{} {} {} {} {}",
            terminal.accent("|"),
            left_cell,
            terminal.accent("|"),
            right_cell,
            terminal.accent("|")
        );
    }

    println!("{}", terminal.accent(&border));
}

fn pad_cell(value: &str, width: usize) -> String {
    let mut visible: String = value.chars().take(width).collect();
    let padding = width.saturating_sub(visible.chars().count());
    visible.push_str(&" ".repeat(padding));
    visible
}

fn recent_activity_lines(current_thread_id: &str) -> Vec<String> {
    let Ok(threads) = thread_store::list_threads() else {
        return vec!["No saved threads yet".to_string()];
    };
    let mut lines = threads
        .into_iter()
        .filter(|thread| thread.id != current_thread_id)
        .take(4)
        .map(|thread| {
            format!(
                "{:<8} Resume {}",
                format_age(thread.updated_at),
                short_thread_id(&thread.id)
            )
        })
        .collect::<Vec<_>>();

    if lines.is_empty() {
        lines.push("No saved threads yet".to_string());
    } else {
        lines.push("... /resume for more".to_string());
    }

    lines
}

fn short_thread_id(id: &str) -> String {
    id.chars().take(12).collect()
}

fn format_age(updated_at: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(updated_at);
    let seconds = now.saturating_sub(updated_at);
    if seconds < 60 {
        "now".to_string()
    } else if seconds < 60 * 60 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 60 * 60 * 24 {
        format!("{}h ago", seconds / 60 / 60)
    } else if seconds < 60 * 60 * 24 * 7 {
        format!("{}d ago", seconds / 60 / 60 / 24)
    } else {
        format!("{}w ago", seconds / 60 / 60 / 24 / 7)
    }
}

fn compact_path(path: &PathBuf) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(stripped) = path.strip_prefix(&home) {
            let stripped = stripped.display().to_string();
            return if stripped.is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", stripped)
            };
        }
    }

    path.display().to_string()
}

fn expand_cwd(path: &str) -> Result<PathBuf> {
    let path = if let Some(rest) = path.strip_prefix("~/") {
        let home = dirs::home_dir().context("could not locate home directory")?;
        home.join(rest)
    } else if path == "~" {
        dirs::home_dir().context("could not locate home directory")?
    } else {
        PathBuf::from(path)
    };

    path.canonicalize()
        .with_context(|| format!("failed to resolve {}", path.display()))
}
