mod account;
mod api;
mod auth;
mod config;
mod context;
mod harness;
mod input;
mod onboarding;
mod repl;
mod telemetry;
mod terminal;
mod thread_store;
mod tool_protocol;
mod tools;
mod update;

use anyhow::{bail, Context, Result};
use api::{ClientConfig, CodeusClient, Message};
use clap::{Args, Parser, Subcommand, ValueEnum};
use config::{Config, PermissionMode, SandboxPolicy, DEFAULT_API_BASE, DEFAULT_MODEL};
use serde::Serialize;
use serde_json::json;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Parser)]
#[command(name = "codeus")]
#[command(author, version, about = "Terminal-native QC Codeus")]
struct Cli {
    #[arg(long, global = true, env = "CODEUS_API_BASE")]
    api_base: Option<String>,

    #[arg(long, global = true, env = "CODEUS_MODEL")]
    model: Option<String>,

    #[arg(long, global = true, env = "CODEUS_OUTPUT_FORMAT", value_enum, default_value_t = OutputFormat::Text)]
    output_format: OutputFormat,

    #[arg(long, global = true, env = "CODEUS_PERMISSION_MODE", value_enum)]
    permission_mode: Option<PermissionMode>,

    #[arg(long, global = true, env = "CODEUS_SANDBOX_POLICY", value_enum)]
    sandbox_policy: Option<SandboxPolicy>,

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
    permission_mode: Option<PermissionMode>,
    sandbox_policy: Option<SandboxPolicy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy)]
enum AuthMode {
    Passive,
    Required,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start an interactive chat session.
    Chat(ChatArgs),
    /// Send one prompt and exit.
    Run(RunArgs),
    /// Connect Codeus CLI to a QC account.
    Login(LoginArgs),
    /// Remove the stored session token.
    Logout,
    /// Show the signed-in QC account and usage information.
    Account,
    /// Show remaining usage for the signed-in QC account.
    Usage,
    /// Run local diagnostics for auth, proxy, config, and workspace health.
    Doctor,
    /// Print resolved CLI status.
    Status,
    /// Print latest local thread state.
    State,
    /// Check for and install the latest Codeus CLI release.
    Update,
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
    /// Store a token directly. Intended for development and support.
    #[arg(long, env = "CODEUS_SESSION_TOKEN", hide_env_values = true)]
    token: Option<String>,

    /// Print the verification URL instead of opening a browser.
    #[arg(long)]
    no_browser: bool,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Print the resolved configuration.
    Show,
    /// Print environment overrides used by Codeus CLI.
    Env,
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
    if let Some(permission_mode) = cli.permission_mode {
        std::env::set_var("CODEUS_PERMISSION_MODE", permission_mode.to_string());
    }
    if let Some(sandbox_policy) = cli.sandbox_policy {
        std::env::set_var("CODEUS_SANDBOX_POLICY", sandbox_policy.to_string());
    }
    let stored = Config::load()?;
    let globals = GlobalOptions {
        api_base: cli.api_base.clone(),
        model: cli.model.clone(),
        token: cli.token.clone(),
        permission_mode: cli.permission_mode,
        sandbox_policy: cli.sandbox_policy,
    };

    match cli.command {
        Some(Command::Chat(args)) => {
            let stored = maybe_onboard(&globals, stored, args.model.clone()).await?;
            update::maybe_print_update_notice(&stored).await;
            let client = client_from(&globals, &stored, args.model, AuthMode::Passive).await?;
            repl::run(client).await
        }
        Some(Command::Run(args)) => {
            let client = client_from(&globals, &stored, args.model, AuthMode::Required).await?;
            run_prompt(client, args.prompt, args.cwd, cli.output_format).await
        }
        Some(Command::Login(args)) => login(args).await,
        Some(Command::Logout) => logout(),
        Some(Command::Account) => {
            let client = client_from(&globals, &stored, None, AuthMode::Required).await?;
            account_command(&client, cli.output_format).await
        }
        Some(Command::Usage) => {
            let client = client_from(&globals, &stored, None, AuthMode::Required).await?;
            usage_command(&client, cli.output_format).await
        }
        Some(Command::Doctor) => doctor_command(&globals, &stored, cli.output_format).await,
        Some(Command::Status) => status_command(&globals, &stored, cli.output_format),
        Some(Command::State) => state_command(cli.output_format),
        Some(Command::Update) => update::run_update().await,
        Some(Command::Config { command }) => config_command(command, stored, cli.output_format),
        None if !cli.prompt.is_empty() => {
            let client = client_from(&globals, &stored, None, AuthMode::Required).await?;
            run_prompt(client, cli.prompt, None, cli.output_format).await
        }
        None => {
            let stored = maybe_onboard(&globals, stored, None).await?;
            update::maybe_print_update_notice(&stored).await;
            repl::run(client_from(&globals, &stored, None, AuthMode::Passive).await?).await
        }
    }
}

async fn run_prompt(
    client: CodeusClient,
    prompt_parts: Vec<String>,
    cwd: Option<PathBuf>,
    output_format: OutputFormat,
) -> Result<()> {
    let prompt = prompt_parts.join(" ").trim().to_string();
    if prompt.is_empty() {
        bail!("prompt cannot be empty");
    }

    let cwd = cwd
        .unwrap_or(std::env::current_dir().context("failed to read current directory")?)
        .canonicalize()
        .context("failed to resolve working directory")?;

    let context = context::build_context(&cwd, None)?;

    let messages = vec![context.system, Message::new("user", prompt)];

    match output_format {
        OutputFormat::Text => {
            stream_and_print(&client, &messages).await?;
            println!();
        }
        OutputFormat::Json => {
            let content = client.stream_chat(&messages, |_| Ok(())).await?;
            print_json(&json!({
                "content": content,
                "model": client.config().model,
                "api_base": client.config().api_base,
                "cwd": cwd,
                "context": {
                    "agent_doc_count": context.stats.agent_doc_count,
                    "agent_doc_bytes": context.stats.agent_doc_bytes,
                }
            }))?;
        }
    }
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

async fn client_from(
    globals: &GlobalOptions,
    stored: &Config,
    command_model: Option<String>,
    auth_mode: AuthMode,
) -> Result<CodeusClient> {
    let mut stored = stored.clone();
    let token = if let Some(token) = globals
        .token
        .clone()
        .filter(|token| !token.trim().is_empty())
    {
        Some(token)
    } else if let Some(token) = stored
        .session_token
        .clone()
        .filter(|token| !token.trim().is_empty())
        .filter(|_| !auth::token_is_expired(&stored))
    {
        Some(token)
    } else if matches!(auth_mode, AuthMode::Required) {
        if let Some(refresh_token) = stored
            .refresh_token
            .clone()
            .filter(|token| !token.trim().is_empty())
        {
            Some(
                auth::refresh_session(
                    globals.api_base.clone(),
                    globals.model.clone(),
                    &mut stored,
                    &refresh_token,
                )
                .await
                .context("could not refresh your Codeus session")?,
            )
        } else {
            None
        }
    } else {
        None
    };

    if matches!(auth_mode, AuthMode::Required) && token.is_none() {
        bail!(
            "No Codeus account is connected. Run `codeus` to open the terminal interface, then use `/login`, or run `codeus login --token ...` for development."
        );
    }

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
        token,
    };

    CodeusClient::new(config)
}

async fn maybe_onboard(
    globals: &GlobalOptions,
    stored: Config,
    command_model: Option<String>,
) -> Result<Config> {
    if !onboarding::should_run(&stored) || globals.token.is_some() {
        return Ok(stored);
    }

    onboarding::run(
        globals.api_base.clone(),
        command_model.or_else(|| globals.model.clone()),
        stored,
    )
    .await
}

async fn login(args: LoginArgs) -> Result<()> {
    if let Some(token) = args.token {
        return auth::login_with_token(token).await;
    }

    let globals = GlobalOptions {
        api_base: None,
        model: None,
        token: None,
        permission_mode: None,
        sandbox_policy: None,
    };
    let stored = Config::load()?;
    auth::login_with_browser(globals.api_base, globals.model, &stored, args.no_browser)
        .await
        .context("browser login could not start. Verify CODEUS_API_BASE points at https://api.quantumclub.sh/v1 or another host exposing the Codeus device-link API")?;
    println!(
        "Connected Codeus CLI to your QC account at {}",
        config::config_path()?.display()
    );
    Ok(())
}

fn logout() -> Result<()> {
    let mut config = Config::load()?;
    config.session_token = None;
    config.refresh_token = None;
    config.token_expires_at = None;
    config.save()?;
    println!("Removed stored session token.");
    Ok(())
}

async fn account_command(client: &CodeusClient, output_format: OutputFormat) -> Result<()> {
    match output_format {
        OutputFormat::Text => account::print_account(client).await,
        OutputFormat::Json => {
            let account = client.account().await?;
            let usage = client.usage().await.ok();
            print_json(&json!({
                "account": account,
                "usage": usage,
            }))
        }
    }
}

async fn usage_command(client: &CodeusClient, output_format: OutputFormat) -> Result<()> {
    match output_format {
        OutputFormat::Text => account::print_usage(client).await,
        OutputFormat::Json => {
            let usage = client.usage().await?;
            print_json(&usage)
        }
    }
}

async fn doctor_command(
    globals: &GlobalOptions,
    stored: &Config,
    output_format: OutputFormat,
) -> Result<()> {
    let report = build_doctor_report(globals, stored).await;
    match output_format {
        OutputFormat::Text => print_doctor_text(&report),
        OutputFormat::Json => print_json(&report)?,
    }

    if report.ok {
        Ok(())
    } else {
        bail!("Codeus doctor found failing checks")
    }
}

fn status_command(
    globals: &GlobalOptions,
    stored: &Config,
    output_format: OutputFormat,
) -> Result<()> {
    let report = build_status_report(globals, stored)?;
    match output_format {
        OutputFormat::Text => print_status_text(&report),
        OutputFormat::Json => print_json(&report)?,
    }
    Ok(())
}

fn state_command(output_format: OutputFormat) -> Result<()> {
    let report = build_state_report()?;
    match output_format {
        OutputFormat::Text => print_state_text(&report),
        OutputFormat::Json => print_json(&report)?,
    }
    Ok(())
}

fn config_command(
    command: ConfigCommand,
    mut config: Config,
    output_format: OutputFormat,
) -> Result<()> {
    match command {
        ConfigCommand::Show => {
            let display = Config {
                session_token: config
                    .session_token
                    .as_ref()
                    .map(|_| "<stored>".to_string()),
                refresh_token: config
                    .refresh_token
                    .as_ref()
                    .map(|_| "<stored>".to_string()),
                ..config
            };
            match output_format {
                OutputFormat::Text => print!("{}", toml::to_string_pretty(&display)?),
                OutputFormat::Json => print_json(&display)?,
            }
        }
        ConfigCommand::Env => {
            let env = env_report();
            match output_format {
                OutputFormat::Text => print_env_text(&env),
                OutputFormat::Json => print_json(&env)?,
            }
        }
        ConfigCommand::Path => match output_format {
            OutputFormat::Text => println!("{}", config::config_path()?.display()),
            OutputFormat::Json => print_json(&json!({ "path": config::config_path()? }))?,
        },
        ConfigCommand::Set { key, value } => {
            match key.as_str() {
                "api_base" => config.api_base = Some(value),
                "model" => config.model = Some(value),
                "theme" => config.theme = Some(value),
                "session_token" | "token" => config.session_token = Some(value),
                "refresh_token" => config.refresh_token = Some(value),
                "font" => config.font = Some(value),
                "permission_mode" => {
                    config.permission_mode = Some(PermissionMode::parse(&value)?);
                }
                "sandbox_policy" => {
                    config.sandbox_policy = Some(SandboxPolicy::parse(&value)?);
                }
                "token_expires_at" => {
                    config.token_expires_at = Some(
                        value
                            .parse()
                            .context("token_expires_at must be a Unix timestamp")?,
                    )
                }
                "update_check_disabled" => {
                    config.update_check_disabled = Some(parse_bool(&value)?);
                }
                "last_update_check_at" => {
                    config.last_update_check_at = Some(
                        value
                            .parse()
                            .context("last_update_check_at must be a Unix timestamp")?,
                    )
                }
                _ => bail!("unknown config key: {key}"),
            }
            config.save()?;
            print_config_mutation(output_format, "set", &key)?;
        }
        ConfigCommand::Unset { key } => {
            match key.as_str() {
                "api_base" => config.api_base = None,
                "model" => config.model = None,
                "theme" => config.theme = None,
                "session_token" | "token" => config.session_token = None,
                "refresh_token" => config.refresh_token = None,
                "font" => config.font = None,
                "permission_mode" => config.permission_mode = None,
                "sandbox_policy" => config.sandbox_policy = None,
                "token_expires_at" => config.token_expires_at = None,
                "update_check_disabled" => config.update_check_disabled = None,
                "last_update_check_at" => config.last_update_check_at = None,
                _ => bail!("unknown config key: {key}"),
            }
            config.save()?;
            print_config_mutation(output_format, "unset", &key)?;
        }
    }

    Ok(())
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => bail!("expected true or false"),
    }
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_config_mutation(output_format: OutputFormat, action: &str, key: &str) -> Result<()> {
    match output_format {
        OutputFormat::Text => Ok(()),
        OutputFormat::Json => print_json(&json!({
            "ok": true,
            "action": action,
            "key": key,
        })),
    }
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    ok: bool,
    checked_at: u64,
    checks: Vec<DoctorCheck>,
}

#[derive(Debug, Serialize)]
struct DoctorCheck {
    name: String,
    status: CheckStatus,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckStatus {
    Ok,
    Warn,
    Fail,
    Skip,
}

#[derive(Debug, Serialize)]
struct StatusReport {
    binary: &'static str,
    version: &'static str,
    api_base: String,
    model: String,
    auth: AuthReport,
    permission_mode: String,
    sandbox_policy: String,
    config_path: PathBuf,
    cwd: PathBuf,
    context: ContextReport,
    threads: ThreadsReport,
    trace: TraceReport,
    approved_command_prefixes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct AuthReport {
    signed_in: bool,
    token_source: &'static str,
    token_expired: bool,
    refresh_token_present: bool,
}

#[derive(Debug, Serialize)]
struct ContextReport {
    agent_doc_count: usize,
    agent_doc_bytes: usize,
}

#[derive(Debug, Serialize)]
struct ThreadsReport {
    count: usize,
    latest_id: Option<String>,
    latest_path: Option<PathBuf>,
    directory: PathBuf,
}

#[derive(Debug, Serialize)]
struct TraceReport {
    enabled: bool,
    path: PathBuf,
}

#[derive(Debug, Serialize)]
struct StateReport {
    threads_directory: PathBuf,
    latest_thread: Option<ThreadStateReport>,
}

#[derive(Debug, Serialize)]
struct ThreadStateReport {
    id: String,
    path: PathBuf,
    cwd: PathBuf,
    updated_at: u64,
    size: u64,
    message_count: usize,
    compacted: bool,
}

#[derive(Debug, Serialize)]
struct EnvVarReport {
    name: &'static str,
    set: bool,
    value: Option<String>,
    redacted: bool,
}

async fn build_doctor_report(globals: &GlobalOptions, stored: &Config) -> DoctorReport {
    let mut checks = Vec::new();

    push_check(
        &mut checks,
        "config.load",
        CheckStatus::Ok,
        "configuration parsed successfully",
    );

    let config_path = match config::config_path() {
        Ok(path) => {
            let detail = if path.exists() {
                format!("using {}", path.display())
            } else {
                format!("{} does not exist yet", path.display())
            };
            push_check(&mut checks, "config.path", CheckStatus::Ok, detail);
            Some(path)
        }
        Err(error) => {
            push_check(
                &mut checks,
                "config.path",
                CheckStatus::Fail,
                error.to_string(),
            );
            None
        }
    };

    if let Some(path) = &config_path {
        check_config_permissions(&mut checks, path, stored.session_token.is_some());
    }

    let api_base = resolve_api_base(globals, stored);
    let api_status = if api_base.starts_with("http://") || api_base.starts_with("https://") {
        CheckStatus::Ok
    } else {
        CheckStatus::Fail
    };
    push_check(
        &mut checks,
        "api_base",
        api_status,
        format!("resolved to {api_base}"),
    );

    let model = resolve_model(globals, stored);
    push_check(
        &mut checks,
        "model",
        if model.trim().is_empty() {
            CheckStatus::Fail
        } else {
            CheckStatus::Ok
        },
        format!("resolved to {model}"),
    );

    let auth = build_auth_report(globals, stored);
    let auth_status = if auth.signed_in && auth.token_expired {
        CheckStatus::Warn
    } else if auth.signed_in {
        CheckStatus::Ok
    } else {
        CheckStatus::Warn
    };
    push_check(
        &mut checks,
        "auth",
        auth_status,
        if auth.signed_in {
            format!(
                "session token present from {}; expired={}",
                auth.token_source, auth.token_expired
            )
        } else {
            "no session token; run `codeus login` before chat or run".to_string()
        },
    );

    match std::env::current_dir()
        .context("failed to read current directory")
        .and_then(|path| {
            path.canonicalize()
                .context("failed to resolve current directory")
        }) {
        Ok(cwd) => push_check(
            &mut checks,
            "workspace.cwd",
            CheckStatus::Ok,
            cwd.display().to_string(),
        ),
        Err(error) => push_check(
            &mut checks,
            "workspace.cwd",
            CheckStatus::Fail,
            error.to_string(),
        ),
    }

    match thread_store::list_threads() {
        Ok(threads) => push_check(
            &mut checks,
            "threads",
            CheckStatus::Ok,
            format!("{} saved thread(s)", threads.len()),
        ),
        Err(error) => push_check(&mut checks, "threads", CheckStatus::Fail, error.to_string()),
    }

    let permission_mode = resolve_permission_mode(globals, stored);
    push_check(
        &mut checks,
        "permissions",
        CheckStatus::Ok,
        format!("permission mode is {permission_mode}"),
    );

    let sandbox_policy = resolve_sandbox_policy(globals, stored);
    push_check(
        &mut checks,
        "sandbox",
        CheckStatus::Warn,
        format!("sandbox policy is {sandbox_policy}; native OS sandboxing is not implemented yet"),
    );

    let prefix_warnings = command_prefix_warnings(stored);
    if prefix_warnings.is_empty() {
        push_check(
            &mut checks,
            "approved_command_prefixes",
            CheckStatus::Ok,
            tools::format_allowed_prefixes(stored),
        );
    } else {
        push_check(
            &mut checks,
            "approved_command_prefixes",
            CheckStatus::Fail,
            prefix_warnings.join("; "),
        );
    }

    match telemetry::trace_path() {
        Ok(path) => push_check(
            &mut checks,
            "telemetry",
            CheckStatus::Ok,
            format!(
                "trace {} at {}",
                if telemetry::enabled() {
                    "enabled"
                } else {
                    "disabled"
                },
                path.display()
            ),
        ),
        Err(error) => push_check(
            &mut checks,
            "telemetry",
            CheckStatus::Fail,
            error.to_string(),
        ),
    }

    if auth.signed_in {
        match CodeusClient::new(ClientConfig {
            api_base,
            model,
            token: resolve_token(globals, stored),
        }) {
            Ok(client) => match client.account().await {
                Ok(_) => push_check(
                    &mut checks,
                    "proxy.account",
                    CheckStatus::Ok,
                    "authenticated account endpoint responded",
                ),
                Err(error) => push_check(
                    &mut checks,
                    "proxy.account",
                    CheckStatus::Fail,
                    error.to_string(),
                ),
            },
            Err(error) => push_check(
                &mut checks,
                "proxy.client",
                CheckStatus::Fail,
                error.to_string(),
            ),
        }
    } else {
        push_check(
            &mut checks,
            "proxy.account",
            CheckStatus::Skip,
            "no session token available",
        );
    }

    let ok = checks.iter().all(|check| check.status != CheckStatus::Fail);
    DoctorReport {
        ok,
        checked_at: now_secs(),
        checks,
    }
}

fn build_status_report(globals: &GlobalOptions, stored: &Config) -> Result<StatusReport> {
    let cwd = std::env::current_dir()
        .context("failed to read current directory")?
        .canonicalize()
        .context("failed to resolve current directory")?;
    let context = context::build_context(&cwd, None)?;
    let threads = thread_store::list_threads()?;
    let latest = threads.first();
    Ok(StatusReport {
        binary: "codeus",
        version: env!("CARGO_PKG_VERSION"),
        api_base: resolve_api_base(globals, stored),
        model: resolve_model(globals, stored),
        auth: build_auth_report(globals, stored),
        permission_mode: resolve_permission_mode(globals, stored).to_string(),
        sandbox_policy: resolve_sandbox_policy(globals, stored).to_string(),
        config_path: config::config_path()?,
        cwd,
        context: ContextReport {
            agent_doc_count: context.stats.agent_doc_count,
            agent_doc_bytes: context.stats.agent_doc_bytes,
        },
        threads: ThreadsReport {
            count: threads.len(),
            latest_id: latest.map(|thread| thread.id.clone()),
            latest_path: latest.map(|thread| thread.path.clone()),
            directory: threads_dir()?,
        },
        trace: TraceReport {
            enabled: telemetry::enabled(),
            path: telemetry::trace_path()?,
        },
        approved_command_prefixes: tools::allowed_prefix_labels(stored),
    })
}

fn build_state_report() -> Result<StateReport> {
    let threads = thread_store::list_threads()?;
    let latest_thread = if let Some(info) = threads.first() {
        let (_, stored) = thread_store::ThreadStore::open(&info.id)?;
        Some(ThreadStateReport {
            id: info.id.clone(),
            path: info.path.clone(),
            cwd: stored.cwd,
            updated_at: info.updated_at,
            size: info.size,
            message_count: stored.transcript.len(),
            compacted: stored.summary.is_some(),
        })
    } else {
        None
    };

    Ok(StateReport {
        threads_directory: threads_dir()?,
        latest_thread,
    })
}

fn print_doctor_text(report: &DoctorReport) {
    println!("Codeus doctor");
    for check in &report.checks {
        let label = match check.status {
            CheckStatus::Ok => "ok",
            CheckStatus::Warn => "warn",
            CheckStatus::Fail => "fail",
            CheckStatus::Skip => "skip",
        };
        println!("  {label:<4} {:<28} {}", check.name, check.detail);
    }
    println!();
    println!("result: {}", if report.ok { "ok" } else { "failed" });
}

fn print_status_text(report: &StatusReport) {
    println!("Codeus status");
    println!("  binary: {} {}", report.binary, report.version);
    println!("  model: {}", report.model);
    println!("  api: {}", report.api_base);
    println!(
        "  auth: {} ({})",
        if report.auth.signed_in {
            "signed in"
        } else {
            "not signed in"
        },
        report.auth.token_source
    );
    println!("  permission mode: {}", report.permission_mode);
    println!("  sandbox policy: {}", report.sandbox_policy);
    println!("  cwd: {}", report.cwd.display());
    println!("  config: {}", report.config_path.display());
    println!("  threads: {}", report.threads.count);
    if let Some(id) = &report.threads.latest_id {
        println!("  latest thread: {id}");
    }
    println!(
        "  AGENTS.md files: {} ({} bytes)",
        report.context.agent_doc_count, report.context.agent_doc_bytes
    );
    println!(
        "  trace: {} ({})",
        if report.trace.enabled {
            "enabled"
        } else {
            "disabled"
        },
        report.trace.path.display()
    );
    println!(
        "  approved prefixes: {}",
        report.approved_command_prefixes.join(", ")
    );
}

fn print_state_text(report: &StateReport) {
    println!("Codeus state");
    println!(
        "  threads directory: {}",
        report.threads_directory.display()
    );
    if let Some(thread) = &report.latest_thread {
        println!("  latest thread: {}", thread.id);
        println!("  path: {}", thread.path.display());
        println!("  cwd: {}", thread.cwd.display());
        println!("  messages: {}", thread.message_count);
        println!("  compacted: {}", thread.compacted);
    } else {
        println!("  latest thread: none");
    }
}

fn env_report() -> Vec<EnvVarReport> {
    [
        ("CODEUS_API_BASE", false),
        ("CODEUS_MODEL", false),
        ("CODEUS_SESSION_TOKEN", true),
        ("CODEUS_PERMISSION_MODE", false),
        ("CODEUS_SANDBOX_POLICY", false),
        ("CODEUS_OUTPUT_FORMAT", false),
        ("CODEUS_NO_UPDATE_CHECK", false),
        ("CODEUS_UPDATE_ENDPOINT", false),
        ("CODEUS_TRACE", false),
    ]
    .into_iter()
    .map(|(name, redacted)| {
        let value = std::env::var(name).ok();
        EnvVarReport {
            name,
            set: value.is_some(),
            value: value.map(|value| if redacted { "<set>".to_string() } else { value }),
            redacted,
        }
    })
    .collect()
}

fn print_env_text(vars: &[EnvVarReport]) {
    for var in vars {
        println!("{}={}", var.name, var.value.as_deref().unwrap_or("<unset>"));
    }
}

fn push_check(
    checks: &mut Vec<DoctorCheck>,
    name: impl Into<String>,
    status: CheckStatus,
    detail: impl Into<String>,
) {
    checks.push(DoctorCheck {
        name: name.into(),
        status,
        detail: detail.into(),
    });
}

fn check_config_permissions(
    checks: &mut Vec<DoctorCheck>,
    path: &std::path::Path,
    has_token: bool,
) {
    if !path.exists() {
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match fs::metadata(path) {
            Ok(metadata) => {
                let mode = metadata.permissions().mode() & 0o777;
                if has_token && mode & 0o077 != 0 {
                    push_check(
                        checks,
                        "config.permissions",
                        CheckStatus::Warn,
                        format!(
                            "{} has mode {mode:o}; expected 600 with a stored token",
                            path.display()
                        ),
                    );
                } else {
                    push_check(
                        checks,
                        "config.permissions",
                        CheckStatus::Ok,
                        format!("{} mode {mode:o}", path.display()),
                    );
                }
            }
            Err(error) => push_check(
                checks,
                "config.permissions",
                CheckStatus::Warn,
                error.to_string(),
            ),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = has_token;
        push_check(
            checks,
            "config.permissions",
            CheckStatus::Skip,
            "file mode checks are not available on this platform",
        );
    }
}

fn resolve_api_base(globals: &GlobalOptions, stored: &Config) -> String {
    globals
        .api_base
        .clone()
        .or_else(|| stored.api_base.clone())
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string())
}

fn resolve_model(globals: &GlobalOptions, stored: &Config) -> String {
    globals
        .model
        .clone()
        .or_else(|| stored.model.clone())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

fn resolve_token(globals: &GlobalOptions, stored: &Config) -> Option<String> {
    globals
        .token
        .clone()
        .filter(|token| !token.trim().is_empty())
        .or_else(|| {
            stored
                .session_token
                .clone()
                .filter(|token| !token.trim().is_empty())
        })
}

fn resolve_permission_mode(globals: &GlobalOptions, stored: &Config) -> PermissionMode {
    globals
        .permission_mode
        .or_else(|| {
            std::env::var("CODEUS_PERMISSION_MODE")
                .ok()
                .and_then(|value| PermissionMode::parse(&value).ok())
        })
        .or(stored.permission_mode)
        .unwrap_or_default()
}

fn resolve_sandbox_policy(globals: &GlobalOptions, stored: &Config) -> SandboxPolicy {
    globals
        .sandbox_policy
        .or_else(|| {
            std::env::var("CODEUS_SANDBOX_POLICY")
                .ok()
                .and_then(|value| SandboxPolicy::parse(&value).ok())
        })
        .or(stored.sandbox_policy)
        .unwrap_or_default()
}

fn build_auth_report(globals: &GlobalOptions, stored: &Config) -> AuthReport {
    let token_source = if globals
        .token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty())
    {
        "env_or_flag"
    } else if stored
        .session_token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty())
    {
        "config"
    } else {
        "none"
    };
    AuthReport {
        signed_in: token_source != "none",
        token_source,
        token_expired: auth::token_is_expired(stored),
        refresh_token_present: stored
            .refresh_token
            .as_deref()
            .is_some_and(|token| !token.trim().is_empty()),
    }
}

fn command_prefix_warnings(config: &Config) -> Vec<String> {
    config
        .approved_command_prefixes
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .enumerate()
        .filter_map(|(index, prefix)| {
            if prefix.is_empty() {
                Some(format!("approved_command_prefixes[{index}] is empty"))
            } else if prefix.iter().any(|part| part.trim().is_empty()) {
                Some(format!(
                    "approved_command_prefixes[{index}] contains an empty argument"
                ))
            } else {
                None
            }
        })
        .collect()
}

fn threads_dir() -> Result<PathBuf> {
    let path = config::config_path()?;
    let parent = path.parent().context("config path has no parent")?;
    Ok(parent.join("threads"))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
