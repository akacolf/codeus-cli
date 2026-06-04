use crate::api::{ClientConfig, CodeusClient, Message};
use crate::auth;
use crate::config::Config;
use crate::stream_and_print;
use crate::terminal::Terminal;
use anyhow::{Context, Result};
use std::io::{self, Write};
use std::path::PathBuf;

pub async fn run(mut client: CodeusClient) -> Result<()> {
    let terminal = Terminal::detect();
    let mut session = Session::new()?;

    print_welcome(&terminal, &client, &session);

    loop {
        print!("{}", prompt(&terminal, &session));
        io::stdout().flush()?;

        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 {
            println!();
            break;
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        if input.starts_with('/') {
            match handle_command(input, &terminal, &mut client, &mut session).await? {
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

        session.messages.push(Message {
            role: "user".to_string(),
            content: input.to_string(),
        });

        print!("{}", terminal.accent("codeus"));
        print!(" ");
        io::stdout().flush()?;
        let assistant = stream_and_print(&client, &session.messages).await?;
        println!();
        session.messages.push(Message {
            role: "assistant".to_string(),
            content: assistant,
        });
    }

    Ok(())
}

struct Session {
    messages: Vec<Message>,
    cwd: PathBuf,
}

impl Session {
    fn new() -> Result<Self> {
        let cwd = std::env::current_dir().context("failed to read current directory")?;
        Ok(Self {
            messages: vec![system_message(&cwd)],
            cwd,
        })
    }

    fn reset(&mut self) {
        self.messages.clear();
        self.messages.push(system_message(&self.cwd));
    }

    fn set_cwd(&mut self, cwd: PathBuf) {
        self.cwd = cwd;
        self.reset();
    }
}

enum CommandResult {
    Continue,
    Exit,
}

async fn handle_command(
    input: &str,
    terminal: &Terminal,
    client: &mut CodeusClient,
    session: &mut Session,
) -> Result<CommandResult> {
    let mut parts = input.split_whitespace();
    let command = parts.next().unwrap_or_default();

    match command {
        "/exit" | "/quit" => Ok(CommandResult::Exit),
        "/clear" | "/reset" => {
            session.reset();
            println!("{}", terminal.success("Thread cleared."));
            Ok(CommandResult::Continue)
        }
        "/commands" | "/help" | "/?" => {
            print_help(terminal);
            Ok(CommandResult::Continue)
        }
        "/status" => {
            print_status(terminal, client, session);
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
                println!("{}", session.cwd.display());
            } else {
                let next = expand_cwd(path).context("failed to resolve directory")?;
                std::env::set_current_dir(&next)
                    .with_context(|| format!("failed to enter {}", next.display()))?;
                session.set_cwd(next);
                println!("cwd: {}", session.cwd.display());
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
    println!("{}", terminal.logo());
    println!(
        "{}",
        terminal
            .muted("QC terminal agent. Type /help for commands, /login to connect, /exit to quit.")
    );
    println!(
        "{} {}  {} {}  {} {}",
        terminal.muted("model"),
        client.config().model,
        terminal.muted("auth"),
        auth_label(client),
        terminal.muted("cwd"),
        session.cwd.display()
    );
    println!();
}

fn print_help(terminal: &Terminal) {
    println!("{}", terminal.accent("Codeus commands"));
    println!("  /help, /commands       Show this command list");
    println!("  /status                Show model, API base, cwd, and thread size");
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

fn print_status(terminal: &Terminal, client: &CodeusClient, session: &Session) {
    let turns = session.messages.len().saturating_sub(1);
    println!("{}", terminal.accent("Session"));
    println!("  model: {}", client.config().model);
    println!("  auth: {}", auth_label(client));
    println!("  api: {}", client.config().api_base);
    println!("  cwd: {}", session.cwd.display());
    println!("  messages: {turns}");
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
                    "If the hosted proxy does not expose device login yet, use `codeus login --token ...` or set CODEUS_SESSION_TOKEN for development."
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

fn prompt(terminal: &Terminal, session: &Session) -> String {
    let name = session
        .cwd
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("workspace");
    format!("{}:{}> ", terminal.accent("codeus"), name)
}

fn system_message(cwd: &PathBuf) -> Message {
    Message {
        role: "system".to_string(),
        content: format!(
            "You are Codeus CLI, QC's terminal-native coding assistant. The user's current working directory is {}. Keep answers concise and practical. Cite files when relevant. Do not claim to run shell commands or edit files unless a tool call actually did it.",
            cwd.display()
        ),
    }
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
