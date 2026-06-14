use crate::api::{ClientConfig, CodeusClient};
use crate::approval::ApprovalEngine;
use crate::config::Config;
use crate::harness::Session;
use crate::input::{self, PromptOutcome, Suggestion};
use crate::onboarding;
use crate::telemetry;
use crate::terminal::Terminal;
use crate::tool_protocol;
use crate::tools::{ToolObservation, ToolStatus};
use anyhow::{Context, Result};
use std::fs;
use std::io::{self, IsTerminal};
use std::time::Instant;

const MAX_MODEL_TOOL_STEPS: usize = 4;
#[derive(Debug, Clone, Copy)]
struct AvailableModel {
    id: &'static str,
    label: &'static str,
    detail: &'static str,
}

const AVAILABLE_MODELS: &[AvailableModel] = &[
    AvailableModel {
        id: "gpt-5-mini",
        label: "GPT 5 Mini",
        detail: "Fast OpenAI model for everyday coding",
    },
    AvailableModel {
        id: "sonnet-4.6",
        label: "Claude Sonnet 4.6",
        detail: "Balanced model for agentic coding",
    },
    AvailableModel {
        id: "gemini-3-flash",
        label: "Gemini 3 Flash",
        detail: "Fast Google model for quick turns",
    },
    AvailableModel {
        id: "haiku-4.5",
        label: "Claude Haiku 4.5",
        detail: "Small model for lightweight edits",
    },
];

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
        value: "/resume",
        detail: "Resume a saved chat",
    },
    Suggestion {
        value: "/swap",
        detail: "Omniston v1beta8 swap helper",
    },
    Suggestion {
        value: "/theme",
        detail: "Change terminal theme",
    },
    Suggestion {
        value: "/model",
        detail: "Show or change model",
    },
    Suggestion {
        value: "/logout",
        detail: "Sign out",
    },
    Suggestion {
        value: "/clear",
        detail: "Start fresh",
    },
    Suggestion {
        value: "/exit",
        detail: "Leave chat",
    },
];

const COMMAND_HELP_ROWS: &[(&str, &str)] = &[
    ("/help, /commands", "Show this command list"),
    ("/status", "Show account, model, project, and context"),
    ("/resume [id|latest]", "Resume a saved chat"),
    ("/swap", "Show Omniston v1beta8 swap helper"),
    ("/theme", "Change the terminal theme"),
    ("/model [name|number]", "Show or change models"),
    ("/logout", "Sign out and remove stored tokens"),
    ("/clear, /reset", "Clear context and start fresh"),
    ("/exit, /quit", "Leave chat"),
];

pub async fn run(mut client: CodeusClient) -> Result<()> {
    let stored = Config::load()?;
    let mut terminal = Terminal::detect_with_theme(stored.theme.as_deref());
    let mut session = Session::new()?;
    let mut account = account_summary(&client).await;
    let mut input_history = input::InputHistory::default();

    print_welcome(&terminal, &client, &session, &account).await;

    loop {
        let (prompt, prompt_width, chrome) =
            prompt(&terminal, &client.config().model, &account, &session);
        let input = match input::read_line_with_chrome_and_history(
            &prompt,
            prompt_width,
            COMMAND_SUGGESTIONS,
            &chrome,
            &mut input_history,
        )? {
            PromptOutcome::Line(input) => input,
            PromptOutcome::Eof | PromptOutcome::Interrupted => break,
        };
        if input.is_empty() {
            continue;
        }

        let command_input = input.trim();
        if command_input.starts_with('/') {
            match handle_command(
                command_input,
                &mut terminal,
                &mut client,
                &mut session,
                &mut account,
            )
            .await?
            {
                CommandResult::Continue => continue,
                CommandResult::Exit => break,
            }
        }

        if !account.is_connected() {
            println!("{} No QC account is connected.", terminal.warn("•"));
            continue;
        }

        session.record_user(input.to_string())?;
        if let Some(dropped) = session.maybe_auto_compact()? {
            println!(
                "{}",
                terminal.muted(&format!("Compacted {dropped} older messages."))
            );
        }

        if let Err(error) = run_agent_turn(&client, &terminal, &mut session, &input).await {
            print_turn_error(&terminal, &error);
        }
    }

    Ok(())
}

async fn run_agent_turn(
    client: &CodeusClient,
    terminal: &Terminal,
    session: &mut Session,
    user_prompt: &str,
) -> Result<()> {
    let config = Config::load()?;
    let mut approval = ApprovalEngine::new(&config);
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
        let prompt_for_step = if step == 0 { user_prompt } else { "" };
        let response = stream_model_response(client, terminal, &messages, prompt_for_step).await?;
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

        if !response.tool_calls.is_empty() {
            session.record_assistant_tool_calls(response.tool_calls.clone())?;
            let config = Config::load()?;
            for tool_call in response.tool_calls {
                let call = tool_protocol::from_chat_tool_call(&tool_call)?;
                let observation =
                    tool_protocol::execute_tool_call(session.cwd(), &config, &call, &mut approval)
                        .await?;
                print_observation(terminal, &observation);
                session.record_tool_result(tool_call.id, observation.as_context_message())?;
            }
            continue;
        }

        let tagged_tool_call = match tool_protocol::parse_tool_call(&response.text)? {
            Some(call) => Some(call),
            None => match tool_protocol::parse_leading_tool_call(&response.text) {
                Ok(call) => call,
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
            },
        };

        match tagged_tool_call {
            Some(call) => {
                let config = Config::load()?;
                let observation =
                    tool_protocol::execute_tool_call(session.cwd(), &config, &call, &mut approval)
                        .await?;
                print_observation(terminal, &observation);
                session.record_tool(&observation)?;
            }
            None => {
                if !response.printed && !response.text.trim().is_empty() {
                    println!("{} {}", terminal.accent("QC Codeus"), response.text);
                }
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
    prompt_text: &str,
) -> Result<ModelResponse> {
    let mut renderer = crate::agent_ui::TurnRenderer::new(terminal, prompt_text);

    // Enable raw mode for ESC detection during streaming.
    let _raw = RawModeGuard::enable()?;

    renderer.start_thinking();

    let mut text = String::new();
    let mut printed = false;

    let completion = client
        .stream_chat_completion(messages, |delta| {
            // Check for ESC interrupt
            if renderer.poll_interrupt() {
                return Err(anyhow::anyhow!("interrupted"));
            }
            renderer.tick(); // Update spinner animation
            text.push_str(delta);
            if printed {
                renderer.on_delta(delta);
                return Ok(());
            }

            if should_buffer_potential_tool_call(&text) {
                return Ok(());
            }

            renderer.on_delta(&text);
            printed = true;
            Ok(())
        })
        .await;

    match completion {
        Ok(completion) => {
            let text = if text.is_empty() {
                completion.content.clone()
            } else {
                text
            };
            let has_hidden_action = !completion.tool_calls.is_empty()
                || tool_protocol::parse_leading_tool_call(&text)
                    .ok()
                    .flatten()
                    .is_some();
            if has_hidden_action {
                renderer.finish_action();
            } else {
                renderer.finish();
            }
            Ok(ModelResponse {
                text,
                printed,
                tool_calls: completion.tool_calls,
            })
        }
        Err(_e) if renderer.is_interrupted() => {
            renderer.finish_interrupted();
            Ok(ModelResponse {
                text,
                printed,
                tool_calls: Vec::new(),
            })
        }
        Err(e) => {
            renderer.finish();
            Err(e)
        }
    }
}

fn should_buffer_potential_tool_call(text: &str) -> bool {
    let trimmed = text.trim_start();
    tool_protocol::TOOL_CALL_OPEN.starts_with(trimmed)
        || trimmed.starts_with(tool_protocol::TOOL_CALL_OPEN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn buffers_partial_tool_call_prefix() {
        assert!(should_buffer_potential_tool_call("<"));
        assert!(should_buffer_potential_tool_call("<codeus_tool"));
        assert!(should_buffer_potential_tool_call(
            "  <codeus_tool_call>{\"name\":\"read_file\"}"
        ));
    }

    #[test]
    fn does_not_buffer_normal_text() {
        assert!(!should_buffer_potential_tool_call("hello"));
        assert!(!should_buffer_potential_tool_call("I can help with that."));
    }

    #[test]
    fn formats_read_observation_as_compact_row() {
        let display = compact_tool_display(&ToolObservation {
            name: "read_file".to_string(),
            args: json!({ "path": "package.json" }),
            status: ToolStatus::Success,
            output: "one\ntwo\nthree\n".to_string(),
            elapsed_ms: 4,
        });

        assert_eq!(
            display,
            CompactToolDisplay {
                title: "Read(package.json)".to_string(),
                detail: Some("Read 3 lines".to_string()),
            }
        );
    }

    #[test]
    fn formats_search_observation_without_listing_files_in_title() {
        let display = compact_tool_display(&ToolObservation {
            name: "search_text".to_string(),
            args: json!({ "pattern": "**/*.test.{js,ts,jsx,tsx}" }),
            status: ToolStatus::Success,
            output: "src/a.test.ts:1: test\nsrc/b.test.ts:1: test\n".to_string(),
            elapsed_ms: 12,
        });

        assert_eq!(
            display,
            CompactToolDisplay {
                title: "Search(pattern: \"**/*.test.{js,ts,jsx,tsx}\")".to_string(),
                detail: Some("Found 2 matches".to_string()),
            }
        );
    }

    #[test]
    fn formats_bash_completion_as_compact_row() {
        let display = compact_tool_display(&ToolObservation {
            name: "run_command".to_string(),
            args: json!({ "command": "cargo check" }),
            status: ToolStatus::Success,
            output: "Finished dev profile".to_string(),
            elapsed_ms: 42,
        });

        assert_eq!(
            display,
            CompactToolDisplay {
                title: "Bash(cargo check)".to_string(),
                detail: Some("Completed in 42ms".to_string()),
            }
        );
    }
}

struct RawModeGuard {
    enabled: bool,
}

impl RawModeGuard {
    fn enable() -> Result<Self> {
        let enabled = io::stdin().is_terminal() && io::stdout().is_terminal();
        if enabled {
            crossterm::terminal::enable_raw_mode()?;
        }
        Ok(Self { enabled })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.enabled {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
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
    account: &mut AccountSummary,
) -> Result<CommandResult> {
    let mut parts = input.split_whitespace();
    let command = parts.next().unwrap_or_default();

    match command {
        "/exit" | "/quit" => Ok(CommandResult::Exit),
        "/clear" | "/reset" => {
            session.reset()?;
            clear_screen();
            print_welcome(terminal, client, session, account).await;
            Ok(CommandResult::Continue)
        }
        "/" | "/commands" | "/help" | "/?" => {
            print_help(terminal);
            Ok(CommandResult::Continue)
        }
        "/status" => {
            show_status(client, session, account)?;
            Ok(CommandResult::Continue)
        }
        "/logout" | "/signout" => {
            logout_current_session(terminal, client, account).await?;
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
        "/swap" => {
            print_swap_helper(terminal, input[command.len()..].trim());
            Ok(CommandResult::Continue)
        }
        "/theme" => {
            let mut config = Config::load()?;
            onboarding::choose_appearance(&mut config)?;
            *terminal = Terminal::detect_with_theme(config.theme.as_deref());
            Ok(CommandResult::Continue)
        }
        "/model" => {
            let selection = parts.collect::<Vec<_>>().join(" ");
            if selection.trim().is_empty() {
                if let Some(model) = select_model_interactive(&client.config().model)? {
                    save_model_selection(client, model)?;
                    clear_screen();
                    print_welcome(terminal, client, session, account).await;
                    print_model_set(terminal, model);
                }
            } else if let Some(model) = resolve_model_selection(&selection) {
                save_model_selection(client, model)?;
                clear_screen();
                print_welcome(terminal, client, session, account).await;
                print_model_set(terminal, model);
            } else {
                println!(
                    "{} Unknown model `{}`. Type /model to see available models.",
                    terminal.warn("•"),
                    selection.trim()
                );
            }
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

async fn print_welcome(
    terminal: &Terminal,
    client: &CodeusClient,
    session: &Session,
    account: &AccountSummary,
) {
    if !account.is_connected() {
        print_signed_out_intro(terminal);
        return;
    }
    print_intro(
        terminal,
        &client.config().model,
        account.plan_value(),
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
        terminal
            .muted("Connect your QC account from the hosted account flow, then reopen Codeus.",)
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
    let display = compact_tool_display(observation);
    let dot = match observation.status {
        ToolStatus::Success => terminal.success("●"),
        ToolStatus::Failed | ToolStatus::Denied => terminal.warn("●"),
    };
    let title = match observation.status {
        ToolStatus::Success => terminal.title(&display.title),
        ToolStatus::Failed | ToolStatus::Denied => terminal.warn(&display.title),
    };

    println!();
    println!("{} {}", dot, title);
    if let Some(detail) = display.detail {
        println!("  {} {}", terminal.muted("└"), detail);
    }

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompactToolDisplay {
    title: String,
    detail: Option<String>,
}

fn compact_tool_display(observation: &ToolObservation) -> CompactToolDisplay {
    let title = match observation.name.as_str() {
        "read_file" => format!(
            "Read({})",
            string_arg(&observation.args, "path").unwrap_or("?")
        ),
        "search_text" => format!(
            "Search(pattern: {})",
            quoted_arg(&observation.args, "pattern").unwrap_or_else(|| "\"?\"".to_string())
        ),
        "list_files" => {
            let path = string_arg(&observation.args, "path")
                .or_else(|| string_arg(&observation.args, "root"))
                .unwrap_or(".");
            format!("List({path})")
        }
        "replace_in_file" => format!(
            "Edit({})",
            string_arg(&observation.args, "path").unwrap_or("?")
        ),
        "apply_patch" => {
            let count = observation
                .args
                .get("operation_count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            format!("Edit({count} operations)")
        }
        "run_command" => format!(
            "Bash({})",
            string_arg(&observation.args, "command").unwrap_or("?")
        ),
        other => format!("Tool({other})"),
    };

    let detail = match observation.status {
        ToolStatus::Success => success_tool_detail(observation),
        ToolStatus::Failed => Some(failure_tool_detail("Failed", observation)),
        ToolStatus::Denied => Some(failure_tool_detail("Denied", observation)),
    };

    CompactToolDisplay { title, detail }
}

fn success_tool_detail(observation: &ToolObservation) -> Option<String> {
    match observation.name.as_str() {
        "read_file" => Some(format!("Read {} lines", count_lines(&observation.output))),
        "search_text" => {
            let matches = count_result_lines(&observation.output);
            if matches == 0 {
                Some("No matches".to_string())
            } else {
                Some(format!("Found {matches} matches"))
            }
        }
        "list_files" => {
            let files = count_result_lines(&observation.output);
            if files == 0 {
                Some("No files found".to_string())
            } else {
                Some(format!("Found {files} files"))
            }
        }
        "replace_in_file" | "apply_patch" => first_output_line(&observation.output),
        "run_command" => Some(format!("Completed in {}ms", observation.elapsed_ms)),
        _ => first_output_line(&observation.output),
    }
}

fn failure_tool_detail(label: &str, observation: &ToolObservation) -> String {
    match first_output_line(&observation.output) {
        Some(line) => format!("{label}: {line}"),
        None => format!("{label} in {}ms", observation.elapsed_ms),
    }
}

fn string_arg<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(serde_json::Value::as_str)
}

fn quoted_arg(value: &serde_json::Value, key: &str) -> Option<String> {
    string_arg(value, key).map(|value| format!("{value:?}"))
}

fn count_lines(output: &str) -> usize {
    output.lines().count()
}

fn count_result_lines(output: &str) -> usize {
    output
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with('[')
                && *line != "No matches."
                && *line != "No files found."
        })
        .count()
}

fn first_output_line(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| {
            const MAX_SUMMARY_CHARS: usize = 120;
            let mut summary = line.chars().take(MAX_SUMMARY_CHARS).collect::<String>();
            if line.chars().count() > MAX_SUMMARY_CHARS {
                summary.push_str("...");
            }
            summary
        })
}

fn show_status(client: &CodeusClient, session: &Session, account: &AccountSummary) -> Result<()> {
    input::show_status_view(&status_rows(account, session, &client.config().model))
}

async fn logout_current_session(
    terminal: &Terminal,
    client: &mut CodeusClient,
    account: &mut AccountSummary,
) -> Result<()> {
    let _ = client.signout().await;

    let mut config = Config::load()?;
    config.session_token = None;
    config.refresh_token = None;
    config.token_expires_at = None;
    config.onboarded = Some(false);
    config.save()?;
    *account = AccountSummary::signed_out();

    let model = client.config().model.clone();
    let signed_in = onboarding::run(None, Some(model.clone()), config).await?;
    *client = CodeusClient::new(ClientConfig {
        api_base: crate::config::effective_api_base(None, &signed_in),
        model: crate::config::effective_model(Some(model), &signed_in),
        token: crate::config::effective_session_token(None, &signed_in),
    })?;
    *account = account_summary(client).await;
    println!("{}", terminal.success("Signed in. Back in the harness."));
    Ok(())
}

fn print_turn_error(terminal: &Terminal, error: &anyhow::Error) {
    println!();
    println!("{} {}", terminal.warn("●"), terminal.warn("Error"));
    println!("  {} {}", terminal.muted("└"), error);
}

fn print_swap_helper(terminal: &Terminal, args: &str) {
    if args != "acknowledge" {
        if let Some(plan) = args.strip_prefix("acknowledge plan ") {
            match crate::swaps::SwapIntent::parse(plan) {
                Ok(intent) => {
                    println!("{}", terminal.accent("Mira swap handoff"));
                    println!(
                        "{} Financial risk acknowledged for planning only. No quote, transaction, or wallet action has been executed.",
                        terminal.warn("•")
                    );
                    println!();
                    println!("{}", intent.mira_handoff_prompt());
                }
                Err(error) => {
                    println!("{} {error}", terminal.warn("•"));
                }
            }
            return;
        }

        println!("{}", terminal.accent("Omniston v1beta8 swap helper"));
        println!(
            "{} Financial risk: swap software can lose funds through bugs, bad inputs, failed transactions, or misuse.",
            terminal.warn("•")
        );
        println!(
            "{} Type `/swap acknowledge` to confirm you understand this risk and view v1beta8 integration guidance.",
            terminal.muted("•")
        );
        println!(
            "{} Then plan a Mira handoff with `/swap acknowledge plan <amount> <from_asset> to <to_asset>`.",
            terminal.muted("•")
        );
        return;
    }

    println!("{}", terminal.accent("Omniston v1beta8 integration"));
    println!("  • Use the STON.fi Omniston SDKs when possible; v1beta8 is the current API model for custom flows.");
    println!("  • Build one RFQ flow for TON and cross-chain swaps: request quote, accept quote, settle, then track.");
    println!("  • Model source and destination explicitly with v1beta8 chain IDs; keep asset and wallet ChainAddress values on matching chains.");
    println!("  • Request quotes with input asset, output asset, amount, and settlementParams.");
    println!("  • Treat quote settlement data as the branch point: swap settlement builds a wallet transaction, order settlement creates or registers an order.");
    println!("  • For TON swap settlement, build the recommended TON messages and ask the user's wallet to sign.");
    println!("  • For cross-chain order settlement, preserve HTLC deadlines, exclusivity windows, and secret-disclosure timing exactly.");
    println!("  • Track submitted trades by quote ID, trader address, and the outgoing transaction query/hash until a terminal state.");
    println!();
    println!("{}", terminal.accent("Mira handoff"));
    println!("  • Plan a route with `/swap acknowledge plan <amount> <from_asset> to <to_asset>`.");
    println!(
        "  • Send Mira a concise intent: `Quote, explain, and risk-check this Omniston v1beta8 route: <input asset/amount/source chain> to <output asset/destination chain>. Do not execute without explicit wallet confirmation.`"
    );
}

fn print_model_picker(terminal: &Terminal, current_model: &str) {
    println!();
    println!("{}", terminal.accent("Select model"));
    println!(
        "{}",
        terminal
            .muted("Switch between Codeus models. Applies to this session and future sessions.")
    );
    println!();
    for (index, model) in AVAILABLE_MODELS.iter().enumerate() {
        let selected = model.id == current_model;
        let cursor = if selected { "›" } else { " " };
        let check = if selected {
            terminal.success("✓")
        } else {
            String::new()
        };
        let label = if selected {
            terminal.success(model.label)
        } else {
            terminal.title(model.label)
        };
        println!(
            "  {} {}. {:<22} {} {}",
            cursor,
            index + 1,
            label,
            terminal.muted(model.detail),
            check
        );
    }
    println!();
    println!(
        "{}",
        terminal.muted("Use `/model 1` or `/model gpt-5-mini` to set the model.")
    );
}

fn select_model_interactive(current_model: &str) -> Result<Option<AvailableModel>> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        let terminal = Terminal::detect_with_theme(None);
        print_model_picker(&terminal, current_model);
        return Ok(None);
    }

    use crossterm::event::{self, Event, KeyCode, KeyEventKind};
    use crossterm::execute;
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
    use ratatui::backend::CrosstermBackend;
    use ratatui::Terminal as RatatuiTerminal;

    let mut selected = AVAILABLE_MODELS
        .iter()
        .position(|model| model.id == current_model)
        .unwrap_or(0);

    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let result = (|| -> Result<Option<AvailableModel>> {
        let _raw = RawModeGuard::enable()?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = RatatuiTerminal::new(backend)?;
        terminal.clear()?;

        loop {
            terminal.draw(|frame| draw_model_picker(frame, selected, current_model))?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                continue;
            }

            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected
                        .checked_sub(1)
                        .unwrap_or(AVAILABLE_MODELS.len() - 1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1) % AVAILABLE_MODELS.len();
                }
                KeyCode::Enter => return Ok(Some(AVAILABLE_MODELS[selected])),
                KeyCode::Esc => return Ok(None),
                _ => {}
            }
        }
    })();

    let mut stdout = io::stdout();
    execute!(stdout, LeaveAlternateScreen)?;
    result
}

fn draw_model_picker(frame: &mut ratatui::Frame<'_>, selected: usize, current_model: &str) {
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Paragraph};

    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Min(1)])
        .split(area);

    let mut lines = vec![
        Line::from(Span::styled(
            "Select model",
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "Switch between Codeus models. Applies to this session and future Codeus sessions.",
            Style::default().fg(Color::Gray),
        )),
        Line::from(Span::styled(
            "For other/previous model names, specify with /model <name>.",
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
    ];

    for (index, model) in AVAILABLE_MODELS.iter().enumerate() {
        let is_selected = index == selected;
        let is_current = model.id == current_model;
        let cursor = if is_selected { "›" } else { " " };
        let label_style = if is_current {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else if is_selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let check = if is_current { " ✓" } else { "" };

        lines.push(Line::from(vec![
            Span::styled(
                format!("  {cursor} {:>2}. ", index + 1),
                Style::default().fg(if is_selected {
                    Color::LightBlue
                } else {
                    Color::DarkGray
                }),
            ),
            Span::styled(format!("{:<22}", model.label), label_style),
            Span::styled(model.detail, Style::default().fg(Color::DarkGray)),
            Span::styled(check, Style::default().fg(Color::Green)),
        ]));
    }

    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "Enter to confirm · Esc to exit",
            Style::default().fg(Color::Gray),
        )),
    ]);

    let panel = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(panel, rows[0]);
}

fn save_model_selection(client: &mut CodeusClient, model: AvailableModel) -> Result<()> {
    let mut config = Config::load()?;
    config.model = Some(model.id.to_string());
    config.save()?;
    client.set_model(model.id);
    Ok(())
}

fn print_model_set(terminal: &Terminal, model: AvailableModel) {
    println!(
        "{} Set model to {}",
        terminal.muted("└"),
        terminal.title(model.label)
    );
}

fn resolve_model_selection(selection: &str) -> Option<AvailableModel> {
    let normalized = selection.trim().to_ascii_lowercase();
    if let Ok(index) = normalized.parse::<usize>() {
        return AVAILABLE_MODELS.get(index.saturating_sub(1)).copied();
    }

    AVAILABLE_MODELS.iter().copied().find(|model| {
        normalized == model.id
            || normalized == model.label.to_ascii_lowercase()
            || normalized == model.label.to_ascii_lowercase().replace(' ', "-")
            || normalized == model.id.replace('-', " ")
    })
}

fn clear_screen() {
    print!("\x1b[2J\x1b[H");
}

#[derive(Debug, Clone)]
struct AccountSummary {
    plan: String,
    email: Option<String>,
}

impl AccountSummary {
    fn signed_out() -> Self {
        Self {
            plan: "not connected".to_string(),
            email: None,
        }
    }

    fn display_value(&self) -> String {
        match &self.email {
            Some(email) => format!("{} · {}", self.plan, email),
            None => self.plan.clone(),
        }
    }

    fn plan_value(&self) -> &str {
        &self.plan
    }

    fn is_connected(&self) -> bool {
        self.plan != "not connected"
    }
}

async fn account_summary(client: &CodeusClient) -> AccountSummary {
    if client.config().token.is_none() {
        return AccountSummary::signed_out();
    }

    let Ok(account) = client.account().await else {
        return AccountSummary::signed_out();
    };
    let email = account_email(&account);
    let plan = if email
        .as_deref()
        .is_some_and(|email| email.eq_ignore_ascii_case("colfgg54@gmail.com"))
    {
        "QC Enterprise".to_string()
    } else {
        account_value(
            &account,
            &["activePlan", "active_plan", "plan", "subscription"],
        )
        .unwrap_or_else(|| "QC account".to_string())
    };

    AccountSummary { plan, email }
}

fn account_email(value: &serde_json::Value) -> Option<String> {
    let object = value.as_object()?;
    for key in [
        "email",
        "accountEmail",
        "account_email",
        "userEmail",
        "user_email",
    ] {
        if let Some(email) = object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|email| !email.trim().is_empty())
        {
            return Some(email.to_string());
        }
    }
    object
        .values()
        .find_map(|value| value.is_object().then(|| account_email(value)).flatten())
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
    if let Some(model) = AVAILABLE_MODELS
        .iter()
        .find(|available| available.id == model)
    {
        return model.label.to_string();
    }

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

fn prompt(
    terminal: &Terminal,
    model: &str,
    account: &AccountSummary,
    session: &Session,
) -> (String, usize, input::ComposerChrome) {
    let chrome = input::ComposerChrome {
        active_commands: active_commands(),
        help_rows: command_help_rows(),
        status_rows: status_rows(account, session, model),
        prompt_suggestions: prompt_suggestions(session),
        footer_left: workspace_footer(session),
        footer_right: "○ /help for commands".to_string(),
    };
    (terminal.composer_prompt(), 2, chrome)
}

fn workspace_footer(session: &Session) -> String {
    let cwd = session.cwd();
    let name = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    let branch = git_branch(cwd)
        .map(|branch| format!(" ({branch})"))
        .unwrap_or_default();
    format!("{name}{branch}")
}

fn git_branch(cwd: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("branch")
        .arg("--show-current")
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!branch.is_empty()).then_some(branch)
}

fn status_rows(account: &AccountSummary, session: &Session, model: &str) -> Vec<input::StatusRow> {
    let turns = session.transcript_len();
    vec![
        input::StatusRow {
            label: "QC account".to_string(),
            value: account.display_value(),
        },
        input::StatusRow {
            label: "Model".to_string(),
            value: display_model(model),
        },
        input::StatusRow {
            label: "Project".to_string(),
            value: session.cwd().display().to_string(),
        },
        input::StatusRow {
            label: "Context".to_string(),
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
    ]
}

fn prompt_suggestions(session: &Session) -> Vec<String> {
    if session.transcript_len() > 0 {
        return Vec::new();
    }

    let cwd = session.cwd();
    let mut suggestions = Vec::new();

    if cwd.join("Cargo.toml").exists() {
        suggestions.push("Run cargo check and fix the first failure".to_string());
        suggestions.push("Review this Rust CLI for release blockers".to_string());
    }

    if cwd.join("package.json").exists() {
        suggestions.push("Run the frontend checks and fix the first failure".to_string());
        suggestions.push("Review this app for broken UI states".to_string());
    }

    if cwd.join("AGENTS.md").exists() {
        suggestions.push("Explain the repo conventions before making changes".to_string());
    }

    if project_has_git(cwd) {
        suggestions.insert(0, "Show the current project status".to_string());
    }

    suggestions.truncate(3);
    suggestions
}

fn project_has_git(cwd: &std::path::Path) -> bool {
    fs::metadata(cwd.join(".git")).is_ok()
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
    println!();
}
