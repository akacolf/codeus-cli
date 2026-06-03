mod api;
mod config;
mod repl;

use anyhow::{bail, Context, Result};
use api::{ClientConfig, CodeusClient, Message};
use clap::{Args, Parser, Subcommand};
use config::{Config, DEFAULT_API_BASE, DEFAULT_MODEL};
use std::io::{self, Write};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "codeus")]
#[command(author, version, about = "Terminal-native QC Codeus")]
struct Cli {
    #[arg(long, global = true, env = "CODEUS_API_BASE")]
    api_base: Option<String>,

    #[arg(long, global = true, env = "CODEUS_MODEL")]
    model: Option<String>,

    #[arg(
        long,
        global = true,
        env = "CODEUS_SESSION_TOKEN",
        hide_env_values = true
    )]
    token: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,

    #[arg(value_name = "PROMPT", trailing_var_arg = true)]
    prompt: Vec<String>,
}

#[derive(Debug, Clone)]
struct GlobalOptions {
    api_base: Option<String>,
    model: Option<String>,
    token: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start an interactive chat session.
    Chat(ChatArgs),
    /// Send one prompt and exit.
    Run(RunArgs),
    /// Store a session token for future requests.
    Login(LoginArgs),
    /// Remove the stored session token.
    Logout,
    /// Inspect or update local CLI configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Debug, Args)]
struct ChatArgs {
    #[arg(short, long)]
    model: Option<String>,
}

#[derive(Debug, Args)]
struct RunArgs {
    #[arg(short, long)]
    model: Option<String>,

    #[arg(long)]
    cwd: Option<PathBuf>,

    #[arg(value_name = "PROMPT", required = true, trailing_var_arg = true)]
    prompt: Vec<String>,
}

#[derive(Debug, Args)]
struct LoginArgs {
    #[arg(long, env = "CODEUS_SESSION_TOKEN", hide_env_values = true)]
    token: Option<String>,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Print the resolved configuration.
    Show,
    /// Print the config file path.
    Path,
    /// Set a config value.
    Set { key: String, value: String },
    /// Remove a config value.
    Unset { key: String },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let stored = Config::load()?;
    let globals = GlobalOptions {
        api_base: cli.api_base.clone(),
        model: cli.model.clone(),
        token: cli.token.clone(),
    };

    match cli.command {
        Some(Command::Chat(args)) => {
            let client = client_from(&globals, &stored, args.model)?;
            repl::run(client).await
        }
        Some(Command::Run(args)) => {
            let client = client_from(&globals, &stored, args.model)?;
            run_prompt(client, args.prompt, args.cwd).await
        }
        Some(Command::Login(args)) => login(args).await,
        Some(Command::Logout) => logout(),
        Some(Command::Config { command }) => config_command(command, stored),
        None if !cli.prompt.is_empty() => {
            let client = client_from(&globals, &stored, None)?;
            run_prompt(client, cli.prompt, None).await
        }
        None => repl::run(client_from(&globals, &stored, None)?).await,
    }
}

async fn run_prompt(
    client: CodeusClient,
    prompt_parts: Vec<String>,
    cwd: Option<PathBuf>,
) -> Result<()> {
    let prompt = prompt_parts.join(" ").trim().to_string();
    if prompt.is_empty() {
        bail!("prompt cannot be empty");
    }

    let cwd = cwd
        .unwrap_or(std::env::current_dir().context("failed to read current directory")?)
        .canonicalize()
        .context("failed to resolve working directory")?;

    let system = format!(
        "You are Codeus CLI, QC's terminal-native coding assistant. The user's current working directory is {}. Be concise, practical, and cite files when relevant.",
        cwd.display()
    );

    let messages = vec![
        Message {
            role: "system".to_string(),
            content: system,
        },
        Message {
            role: "user".to_string(),
            content: prompt,
        },
    ];

    stream_and_print(&client, &messages).await?;
    println!();
    Ok(())
}

pub async fn stream_and_print(client: &CodeusClient, messages: &[Message]) -> Result<String> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    client
        .stream_chat(messages, |delta| {
            write!(out, "{delta}")?;
            out.flush()?;
            Ok(())
        })
        .await
}

fn client_from(
    globals: &GlobalOptions,
    stored: &Config,
    command_model: Option<String>,
) -> Result<CodeusClient> {
    let config = ClientConfig {
        api_base: globals
            .api_base
            .clone()
            .or_else(|| stored.api_base.clone())
            .unwrap_or_else(|| DEFAULT_API_BASE.to_string()),
        model: command_model
            .or_else(|| globals.model.clone())
            .or_else(|| stored.model.clone())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        token: globals
            .token
            .clone()
            .or_else(|| stored.session_token.clone()),
    };

    CodeusClient::new(config)
}

async fn login(args: LoginArgs) -> Result<()> {
    let token = match args.token {
        Some(token) => token,
        None => {
            print!("Codeus session token: ");
            io::stdout().flush()?;
            let mut token = String::new();
            io::stdin().read_line(&mut token)?;
            token.trim().to_string()
        }
    };

    if token.trim().is_empty() {
        bail!("token cannot be empty");
    }

    let mut config = Config::load()?;
    config.session_token = Some(token.trim().to_string());
    config.save()?;
    println!(
        "Saved session token to {}",
        config::config_path()?.display()
    );
    Ok(())
}

fn logout() -> Result<()> {
    let mut config = Config::load()?;
    config.session_token = None;
    config.save()?;
    println!("Removed stored session token.");
    Ok(())
}

fn config_command(command: ConfigCommand, mut config: Config) -> Result<()> {
    match command {
        ConfigCommand::Show => {
            let display = Config {
                session_token: config
                    .session_token
                    .as_ref()
                    .map(|_| "<stored>".to_string()),
                ..config
            };
            print!("{}", toml::to_string_pretty(&display)?);
        }
        ConfigCommand::Path => println!("{}", config::config_path()?.display()),
        ConfigCommand::Set { key, value } => {
            match key.as_str() {
                "api_base" => config.api_base = Some(value),
                "model" => config.model = Some(value),
                "session_token" | "token" => config.session_token = Some(value),
                _ => bail!("unknown config key: {key}"),
            }
            config.save()?;
        }
        ConfigCommand::Unset { key } => {
            match key.as_str() {
                "api_base" => config.api_base = None,
                "model" => config.model = None,
                "session_token" | "token" => config.session_token = None,
                _ => bail!("unknown config key: {key}"),
            }
            config.save()?;
        }
    }

    Ok(())
}
