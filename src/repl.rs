use crate::api::CodeusClient;
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
use std::io::{self, IsTerminal};
use std::time::Instant;

const MAX_MODEL_TOOL_STEPS: usize = 4;
const AVAILABLE_MODELS: &[&str] = &["sonnet-4.6"];

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
        detail: "Show available models",
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
    ("/model", "Show available models"),
    ("/clear, /reset", "Clear context and start fresh"),
    ("/exit, /quit", "Leave chat"),
];

pub async fn run(client: CodeusClient) -> Result<()> {
    let stored = Config::load()?;
    let mut terminal = Terminal::detect_with_theme(stored.theme.as_deref());
    let mut session = Session::new()?;
    let mut account = account_summary(&client).await;
    let mut input_history = input::InputHistory::default();

    print_welcome(&terminal, &client, &session, &account).await;

    loop {
        let (prompt, prompt_width, chrome) = prompt(&terminal, &account, &session);
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
                &client,
                &mut session,
                &mut account,
            )
            .await?
            {
                CommandResult::Continue => continue,
                CommandResult::Exit => break,
            }
        }

        if client.config().token.is_none() {
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

        run_agent_turn(&client, &terminal, &mut session, &input).await?;
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
                    let observation = tool_protocol::execute_tool_call(
                        session.cwd(),
                        &config,
                        &call,
                        &mut approval,
                    )
                    .await?;
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
                    tool_protocol::execute_tool_call(session.cwd(), &config, &call, &mut approval)
                        .await?;
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
            renderer.finish();
            let text = if text.is_empty() {
                completion.content.clone()
            } else {
                text
            };
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
    client: &CodeusClient,
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
            println!("{}", terminal.accent("Available models"));
            for model in AVAILABLE_MODELS {
                let marker = if *model == client.config().model {
                    "*"
                } else {
                    " "
                };
                println!("  {marker} {model}");
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
    if client.config().token.is_none() {
        print_signed_out_intro(terminal);
        return;
    }
    print_intro(
        terminal,
        &client.config().model,
        &account.display_value(),
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

fn show_status(_client: &CodeusClient, session: &Session, account: &AccountSummary) -> Result<()> {
    input::show_status_view(&status_rows(account, session))
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
    println!(
        "  • Send Mira a concise intent: `Quote, explain, and risk-check this Omniston v1beta8 route: <input asset/amount/source chain> to <output asset/destination chain>. Do not execute without explicit wallet confirmation.`"
    );
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
}

async fn account_summary(client: &CodeusClient) -> AccountSummary {
    if client.config().token.is_none() {
        return AccountSummary::signed_out();
    }

    let Ok(account) = client.account().await else {
        return AccountSummary {
            plan: "QC account".to_string(),
            email: None,
        };
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
    account: &AccountSummary,
    session: &Session,
) -> (String, usize, input::ComposerChrome) {
    let chrome = input::ComposerChrome {
        active_commands: active_commands(),
        help_rows: command_help_rows(),
        status_rows: status_rows(account, session),
        prompt_suggestions: prompt_suggestions(),
    };
    (terminal.composer_prompt(), 2, chrome)
}

fn status_rows(account: &AccountSummary, session: &Session) -> Vec<input::StatusRow> {
    let turns = session.transcript_len();
    vec![
        input::StatusRow {
            label: "QC account".to_string(),
            value: account.display_value(),
        },
        input::StatusRow {
            label: "Model".to_string(),
            value: AVAILABLE_MODELS.join(", "),
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

fn prompt_suggestions() -> Vec<String> {
    vec![
        "Build an Omniston v1beta8 quote flow for STON.fi and Mira".to_string(),
        "Review this repo for hackathon readiness".to_string(),
        "Design a safe cross-chain swap confirmation flow".to_string(),
    ]
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
