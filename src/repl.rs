use crate::api::{ClientConfig, CodeusClient};
use crate::auth;
use crate::config::Config;
use crate::harness::Session;
use crate::harness_report;
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
use std::process::Command;
use std::time::Instant;

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
        value: "/new",
        detail: "Start a fresh thread",
    },
    Suggestion {
        value: "/compact",
        detail: "Compact older context",
    },
    Suggestion {
        value: "/files",
        detail: "List workspace files",
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
        value: "/permissions",
        detail: "Show approved command prefixes",
    },
    Suggestion {
        value: "/harness",
        detail: "Show harness readiness",
    },
    Suggestion {
        value: "/review",
        detail: "Review current changes",
    },
    Suggestion {
        value: "/skills",
        detail: "Show skill support",
    },
    Suggestion {
        value: "/feedback",
        detail: "Show feedback channel",
    },
    Suggestion {
        value: "/swap",
        detail: "Omniston v1beta8 swap helper",
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
        detail: "Print QC Codeus logo",
    },
    Suggestion {
        value: "/exit",
        detail: "Leave chat",
    },
];

const COMMAND_HELP_ROWS: &[(&str, &str)] = &[
    ("/help, /commands", "Show this command list"),
    ("/status", "Show model, API base, cwd, and thread size"),
    ("/threads", "List saved local threads"),
    ("/resume [id|latest]", "Resume a saved thread"),
    ("/new", "Start a fresh thread"),
    ("/compact", "Compact older messages into summary context"),
    ("/files [path]", "List workspace files"),
    ("/read <path>", "Read a workspace file into the thread"),
    ("/search <pattern> [path]", "Search workspace text"),
    ("/git status|diff", "Run safe git inspection commands"),
    ("/run <command>", "Run an approved command prefix"),
    ("/approvals, /permissions", "Show approved command prefixes"),
    ("/harness", "Show local agent-harness readiness"),
    ("/review", "Review current working tree changes"),
    ("/skills", "Show local skill support"),
    ("/feedback", "Show the feedback channel"),
    ("/swap", "Show Omniston v1beta8 swap helper"),
    ("/trace", "Show local telemetry status"),
    ("/theme", "Change the terminal theme and font"),
    ("/font", "Change the coding font preference"),
    ("/login [--no-browser]", "Connect a QC account"),
    ("/logout", "Remove the stored session"),
    (
        "/cwd [path], /cd [path]",
        "Show or change the session working directory",
    ),
    ("/model", "Show the active model"),
    ("/api", "Show the active API base"),
    ("/clear, /reset", "Clear this thread"),
    ("/logo", "Print the QC Codeus logo"),
    ("/exit, /quit", "Leave chat"),
];

pub async fn run(mut client: CodeusClient) -> Result<()> {
    let stored = Config::load()?;
    let mut terminal = Terminal::detect_with_theme(stored.theme.as_deref());
    let mut session = Session::new()?;

    print_welcome(&terminal, &client, &session).await;

    loop {
        let (prompt, prompt_width, chrome) = prompt(&terminal, &session);
        let input = match input::read_line_with_chrome(
            &prompt,
            prompt_width,
            COMMAND_SUGGESTIONS,
            &chrome,
        )? {
            PromptOutcome::Line(input) => input,
            PromptOutcome::Eof | PromptOutcome::Interrupted => break,
        };
        if input.is_empty() {
            continue;
        }

        let command_input = input.trim();
        if command_input.starts_with('/') {
            match handle_command(command_input, &mut terminal, &mut client, &mut session).await? {
                CommandResult::Continue => continue,
                CommandResult::Exit => break,
            }
        }

        if client.config().token.is_none() {
            println!(
                "{} Sign in with /login before sending chat messages.",
                terminal.warn("•")
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
            "codeus.turn.started",
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
            "codeus.turn.finished",
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
                        terminal.muted(&format!("QC Codeus requested tool `{}`", call.name))
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
                    terminal.muted(&format!("QC Codeus requested tool `{}`", call.name))
                );
                let config = Config::load()?;
                let observation =
                    tool_protocol::execute_tool_call(session.cwd(), &config, &call).await?;
                print_observation(terminal, &observation);
                session.record_tool(&observation)?;
            }
            Ok(None) => {
                if !response.printed && !response.text.trim().is_empty() {
                    println!("{} {}", terminal.accent("QC Codeus"), response.text);
                }
                session.record_assistant(response.text)?;
                return Ok(());
            }
            Err(error) => {
                if !response.printed && !response.text.trim().is_empty() {
                    println!("{} {}", terminal.accent("QC Codeus"), response.text);
                }
                println!(
                    "{} Could not parse model tool call: {error}",
                    terminal.warn("•")
                );
                session.record_assistant(response.text)?;
                return Ok(());
            }
        }
    }

    println!(
        "{} Stopped after the model requested too many tools in one turn.",
        terminal.warn("•")
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

            print!("{}", terminal.accent("QC Codeus"));
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
            show_status(client, session)?;
            Ok(CommandResult::Continue)
        }
        "/threads" => {
            print_threads(terminal)?;
            Ok(CommandResult::Continue)
        }
        "/new" => {
            *session = Session::new()?;
            println!("{}", terminal.success("Started a fresh thread."));
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
                println!("{} Usage: /read <path>", terminal.warn("•"));
            } else {
                let observation = tools::read_file(session.cwd(), path)?;
                print_observation(terminal, &observation);
                session.record_tool(&observation)?;
            }
            Ok(CommandResult::Continue)
        }
        "/files" => {
            let path = input[command.len()..].trim();
            let observation = tools::list_files(session.cwd(), (!path.is_empty()).then_some(path))?;
            print_observation(terminal, &observation);
            session.record_tool(&observation)?;
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
                println!("{} Usage: /search <pattern> [path]", terminal.warn("•"));
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
                println!("{} Usage: /run <approved command>", terminal.warn("•"));
            } else {
                let config = Config::load()?;
                let observation = tools::run_command(session.cwd(), &config, command_line).await?;
                print_observation(terminal, &observation);
                session.record_tool(&observation)?;
            }
            Ok(CommandResult::Continue)
        }
        "/approvals" | "/permissions" => {
            let config = Config::load()?;
            println!("{}", terminal.accent("Approved command prefixes"));
            println!("  {}", tools::format_allowed_prefixes(&config));
            println!(
                "  permission mode: {}",
                crate::config::effective_permission_mode(&config)
            );
            println!(
                "  sandbox policy: {}",
                crate::config::effective_sandbox_policy(&config)
            );
            Ok(CommandResult::Continue)
        }
        "/harness" => {
            let config = Config::load()?;
            let report = harness_report::build_report(&config)?;
            harness_report::print_text(&report);
            Ok(CommandResult::Continue)
        }
        "/review" => {
            if client.config().token.is_none() {
                println!(
                    "{} Sign in with /login before asking QC Codeus to review changes.",
                    terminal.warn("•")
                );
                return Ok(CommandResult::Continue);
            }

            session.record_user(
                "Review the current working tree. Focus on bugs, regressions, missing tests, and risky behavior. Start with findings ordered by severity and cite files.".to_string(),
            )?;
            run_agent_turn(client, terminal, session).await?;
            Ok(CommandResult::Continue)
        }
        "/skills" => {
            println!("{}", terminal.accent("Skills"));
            println!(
                "{} This CLI has no local skill runtime yet. Use AGENTS.md for project instructions.",
                terminal.muted("•")
            );
            Ok(CommandResult::Continue)
        }
        "/feedback" => {
            println!("{}", terminal.accent("Feedback"));
            println!("  • GitHub: https://github.com/atomicfield/codeus-cli/issues");
            println!(
                "{} Include `codeus /status` output when reporting terminal UI issues.",
                terminal.muted("•")
            );
            Ok(CommandResult::Continue)
        }
        "/swap" => {
            print_swap_helper(terminal, input[command.len()..].trim());
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
            login_from_repl(client, terminal, session, no_browser).await?;
            Ok(CommandResult::Continue)
        }
        "/logout" => {
            let mut config = Config::load()?;
            config.session_token = None;
            config.refresh_token = None;
            config.token_expires_at = None;
            config.onboarded = Some(false);
            config.save()?;
            let current = client.config().clone();
            let config = onboarding::run(
                Some(current.api_base.clone()),
                Some(current.model.clone()),
                config,
            )
            .await?;
            *terminal = Terminal::detect_with_theme(config.theme.as_deref());
            *client = CodeusClient::new(ClientConfig {
                api_base: current.api_base,
                model: current.model,
                token: config.session_token,
            })?;
            if client.config().token.is_some() {
                clear_screen();
                print_welcome(terminal, client, session).await;
            }
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
                terminal.warn("•"),
                command
            );
            Ok(CommandResult::Continue)
        }
    }
}

async fn print_welcome(terminal: &Terminal, client: &CodeusClient, session: &Session) {
    if client.config().token.is_none() {
        print_signed_out_intro(terminal);
        return;
    }
    let subscription = subscription_label(client).await;
    print_intro(
        terminal,
        &client.config().model,
        &subscription,
        &session.cwd().display().to_string(),
    );
}

fn print_signed_out_intro(terminal: &Terminal) {
    println!();
    println!();
    for line in terminal.welcome_art_lines() {
        println!("{line}");
    }
    println!();
    println!("{}", terminal.title("Let's get you started."));
    println!(
        "{}",
        terminal.muted("Choose a theme with /theme, or use /login when you are ready to connect.")
    );
    println!();
}

fn print_help(terminal: &Terminal) {
    println!();
    for row in command_help_rows() {
        println!("  {:<28} {}", row.command, terminal.muted(&row.detail));
    }
    println!();
}

fn print_observation(terminal: &Terminal, observation: &ToolObservation) {
    let status = match observation.status {
        ToolStatus::Success => "ok",
        ToolStatus::Failed => "failed",
        ToolStatus::Denied => "denied",
    };
    let title = format!(
        "tool {status}: {} ({}ms)",
        observation.name, observation.elapsed_ms
    );
    let colored_title = match observation.status {
        ToolStatus::Success => terminal.success(&title),
        ToolStatus::Failed | ToolStatus::Denied => terminal.warn(&title),
    };
    print_output_panel(terminal, &title, &colored_title, observation.output.trim());
    let _ = telemetry::record(
        "codeus.tool.observed",
        serde_json::json!({
            "name": observation.name,
            "status": observation.status,
            "elapsed_ms": observation.elapsed_ms,
            "output_bytes": observation.output.len(),
        }),
    );
}

fn show_status(client: &CodeusClient, session: &Session) -> Result<()> {
    let stored = Config::load().unwrap_or_default();
    let theme = stored
        .theme
        .as_deref()
        .and_then(theme_label)
        .unwrap_or("Auto");
    let turns = session.transcript_len();
    let rows = vec![
        input::StatusRow {
            label: "Account".to_string(),
            value: auth_label(client).to_string(),
        },
        input::StatusRow {
            label: "Model".to_string(),
            value: display_model(&client.config().model),
        },
        input::StatusRow {
            label: "Project".to_string(),
            value: session.cwd().display().to_string(),
        },
        input::StatusRow {
            label: "Thread".to_string(),
            value: format!("{} messages", turns),
        },
        input::StatusRow {
            label: "Memory".to_string(),
            value: if session.summary().is_some() {
                "older context compacted".to_string()
            } else {
                "fresh context".to_string()
            },
        },
        input::StatusRow {
            label: "Theme".to_string(),
            value: theme.to_string(),
        },
        input::StatusRow {
            label: "Font".to_string(),
            value: font_label(stored.font.as_deref()),
        },
    ];
    input::show_status_view(&rows)
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

fn print_swap_helper(terminal: &Terminal, args: &str) {
    if args != "acknowledge" {
        println!("{}", terminal.accent("Omniston v1beta8 swap helper"));
        println!(
            "{} Financial risk: swap software can lose funds through bugs, bad inputs, failed transactions, or misuse.",
            terminal.warn("•")
        );
        println!(
            "{} Type `/swap acknowledge` to confirm you understand this risk and view v1beta8 integration guidance.",
            terminal.muted("•")
        );
        return;
    }

    println!("{}", terminal.accent("Omniston v1beta8 integration"));
    println!("  • Use package namespace: stonfi.omni.v1beta8");
    println!("  • Flow: RFQ -> Quote -> swap or order settlement -> tracking");
    println!(
        "  • Build quote requests with inputAsset, outputAsset, amount, and settlementParams."
    );
    println!("  • Branch on quote.settlementData before building transactions.");
    println!("  • Swap settlement: build TON messages with tonBuildSwap, then sign through the user's wallet.");
    println!(
        "  • Track sent swaps with swapTrack using quoteId, traderAddress, and outgoingTxQuery."
    );
    println!("  • Fees use integrator_address and integrator_fee_pips; one pip is 0.0001%.");
    println!("  • Chains: ton=50, base=3, bnb=4, ethereum=5, polygon=6.");
    println!();
    println!("{}", terminal.accent("Mira handoff"));
    println!(
        "  • Send @mira a concise intent: `Quote and explain this Omniston v1beta8 swap: <input asset/amount/source chain> to <output asset/destination chain>. Do not execute without wallet confirmation.`"
    );
}

async fn login_from_repl(
    client: &mut CodeusClient,
    terminal: &mut Terminal,
    session: &Session,
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
            *terminal = Terminal::detect_with_theme(config.theme.as_deref());
            *client = CodeusClient::new(ClientConfig {
                api_base: current.api_base,
                model: current.model,
                token: config.session_token,
            })?;
            clear_screen();
            print_welcome(terminal, client, session).await;
        }
        Err(error) => {
            println!(
                "{} Browser login could not start: {error}",
                terminal.warn("•")
            );
        }
    }

    Ok(())
}

fn clear_screen() {
    print!("\x1b[2J\x1b[H");
}

fn auth_label(client: &CodeusClient) -> &'static str {
    if client.config().token.is_some() {
        "connected"
    } else {
        "not connected"
    }
}

async fn subscription_label(client: &CodeusClient) -> String {
    if client.config().token.is_none() {
        return "not connected".to_string();
    }

    match client.account().await {
        Ok(account) => account_value(
            &account,
            &["activePlan", "active_plan", "plan", "subscription"],
        )
        .unwrap_or_else(|| "QC account".to_string()),
        Err(_) => "QC account".to_string(),
    }
}

fn account_value(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    for key in keys {
        let Some(value) = object.get(*key) else {
            continue;
        };
        if let Some(text) = value.as_str().filter(|text| !text.trim().is_empty()) {
            return Some(format_subscription(text));
        }
        if value.is_object() {
            if let Some(name) = account_value(value, &["name", "label", "plan", "tier"]) {
                return Some(name);
            }
        }
    }
    None
}

fn format_subscription(value: &str) -> String {
    value
        .split(['-', '_', ' '])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut text = first.to_uppercase().collect::<String>();
                    text.push_str(&chars.as_str().to_ascii_lowercase());
                    text
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn display_model(model: &str) -> String {
    model
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            if part.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
                part.to_string()
            } else {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => {
                        let mut text = first.to_uppercase().collect::<String>();
                        text.push_str(chars.as_str());
                        text
                    }
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn prompt(terminal: &Terminal, session: &Session) -> (String, usize, input::ComposerChrome) {
    let project = project_name(session.cwd());
    let branch = git_branch(session.cwd());
    let left_hint = match branch {
        Some(branch) => format!("{} ({})", terminal.muted(&project), terminal.warn(&branch)),
        None => terminal.muted(&project),
    };
    let chrome = input::ComposerChrome {
        left_hint,
        right_hint: "• /help for commands".to_string(),
        picker_hint: String::new(),
        active_commands: active_commands(),
        help_rows: command_help_rows(),
    };
    (terminal.composer_prompt(), 2, chrome)
}

fn command_help_rows() -> Vec<input::HelpRow> {
    COMMAND_HELP_ROWS
        .iter()
        .map(|(command, detail)| input::HelpRow {
            command: (*command).to_string(),
            detail: (*detail).to_string(),
        })
        .collect()
}

fn active_commands() -> Vec<String> {
    let mut commands = COMMAND_HELP_ROWS
        .iter()
        .flat_map(|(commands, _)| commands.split(','))
        .filter_map(|command| command.split_whitespace().next())
        .map(str::trim)
        .filter(|command| command.starts_with('/'))
        .map(str::to_string)
        .collect::<Vec<_>>();
    commands.push("/?".to_string());
    commands
}

fn project_name(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("/")
        .to_string()
}

fn git_branch(cwd: &std::path::Path) -> Option<String> {
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!branch.is_empty()).then_some(branch)
}

fn print_intro(terminal: &Terminal, model: &str, subscription: &str, cwd: &str) {
    println!();
    let logo = terminal.logo_lines();
    let text = [
        format!(
            "{} {}",
            terminal.title("QC Codeus"),
            terminal.muted(&format!("v{}", env!("CARGO_PKG_VERSION")))
        ),
        terminal.muted(&format!("{} · {}", display_model(model), subscription)),
        terminal.muted(cwd),
    ];
    let logo_width = logo
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);

    for index in 0..logo.len().max(text.len()) {
        let logo_line = logo.get(index).map(String::as_str).unwrap_or("");
        let text_line = text.get(index).map(String::as_str).unwrap_or("");
        println!(
            "  {}  {}",
            terminal.warn(&format!("{logo_line:<logo_width$}")),
            text_line
        );
    }
    println!();
}

fn print_output_panel(terminal: &Terminal, plain_title: &str, colored_title: &str, output: &str) {
    const WIDTH: usize = 76;
    let title_width = plain_title.chars().count();
    let remaining = WIDTH.saturating_sub(title_width + 2);
    println!();
    println!(
        "{} {} {}",
        terminal.accent("┌"),
        colored_title,
        terminal.accent(&format!("{}┐", "─".repeat(remaining)))
    );
    print_panel_line(terminal, WIDTH, "");

    if output.is_empty() {
        print_panel_line(terminal, WIDTH, "");
    } else {
        for line in output.lines().take(18) {
            print_panel_line(terminal, WIDTH, line);
        }
        if output.lines().count() > 18 {
            print_panel_line(
                terminal,
                WIDTH,
                "[truncated in terminal view; full output is saved in thread context]",
            );
        }
    }

    print_panel_line(terminal, WIDTH, "");
    println!("{}", terminal.accent(&format!("└{}┘", "─".repeat(WIDTH))));
    println!();
}

fn print_panel_line(terminal: &Terminal, width: usize, value: &str) {
    let visible: String = value.chars().take(width.saturating_sub(2)).collect();
    let padding = width.saturating_sub(visible.chars().count() + 2);
    println!(
        "{} {}{} {}",
        terminal.accent("│"),
        visible,
        " ".repeat(padding),
        terminal.accent("│")
    );
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
