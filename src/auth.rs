use crate::api::{ClientConfig, CodeusClient, DeviceAuthorization, DeviceTokenStatus, TokenSet};
use crate::config::{self, Config};
use crate::terminal::Terminal;
use anyhow::{bail, Context, Result};
use std::io::{self, IsTerminal, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub async fn login_with_token(token: String) -> Result<()> {
    if token.trim().is_empty() {
        bail!("token cannot be empty");
    }

    let mut config = Config::load()?;
    config.session_token = Some(token.trim().to_string());
    config.save()?;
    println!(
        "Saved session token to {}",
        crate::config::config_path()?.display()
    );
    Ok(())
}

pub async fn login_with_browser(
    api_base: Option<String>,
    model: Option<String>,
    stored: &Config,
    no_browser: bool,
) -> Result<Config> {
    let auth_client = CodeusClient::new(ClientConfig {
        api_base: config::effective_api_base(api_base, stored),
        model: config::effective_model(model, stored),
        token: None,
    })?;

    let auth = auth_client.begin_device_login().await?;
    print_device_instructions(&auth, no_browser, stored.theme.as_deref());
    let token = wait_for_device_token(&auth_client, &auth).await?;

    let mut config = Config::load()?;
    save_token_set(&mut config, token)?;
    config.save()?;
    Ok(config)
}

pub async fn refresh_session(
    api_base: Option<String>,
    model: Option<String>,
    config: &mut Config,
    refresh_token: &str,
) -> Result<String> {
    let client = CodeusClient::new(ClientConfig {
        api_base: config::effective_api_base(api_base, config),
        model: config::effective_model(model, config),
        token: None,
    })?;

    let token = client.refresh_session(refresh_token).await?;
    let session_token = token.session_token.clone();
    save_token_set(config, token)?;
    config.save()?;
    Ok(session_token)
}

pub fn token_is_expired(config: &Config) -> bool {
    let Some(expires_at) = config.token_expires_at else {
        return false;
    };

    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return false;
    };

    now.as_secs().saturating_add(60) >= expires_at
}

fn print_device_instructions(auth: &DeviceAuthorization, no_browser: bool, theme: Option<&str>) {
    let terminal = Terminal::detect_with_theme(theme);
    let url = auth
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&auth.verification_uri);

    if io::stdout().is_terminal() {
        print!("\x1b[2J\x1b[H");
    } else {
        println!();
    }
    print_auth_art(&terminal);
    if let Some(message) = &auth.message {
        println!("{message}");
        println!();
    }
    println!(
        "{}",
        terminal.muted("Browser did not open? Use the URL below to sign in. Press c to copy.")
    );
    println!();
    println!("{url}");
    if let Some(user_code) = &auth.user_code {
        println!();
        println!("Use code: {}", terminal.selection(user_code));
    }

    if !no_browser {
        match open_browser(url) {
            Ok(()) => println!(
                "{}",
                terminal.muted("Opened your browser. Waiting for approval...")
            ),
            Err(error) => println!(
                "{} Could not open a browser automatically: {error}",
                terminal.warn("•")
            ),
        }
    } else {
        println!("{}", terminal.muted("Waiting for browser approval..."));
    }
}

fn print_auth_art(terminal: &Terminal) {
    for line in terminal.welcome_art_lines() {
        println!("{line}");
    }
    println!();
    println!(
        "{}",
        terminal.title("Sign in with your Quantum Club account")
    );
    println!("{}", terminal.muted("Complete sign in in your browser."));
    println!();
}

async fn wait_for_device_token(
    client: &CodeusClient,
    auth: &DeviceAuthorization,
) -> Result<TokenSet> {
    let mut interval = Duration::from_secs(auth.interval);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(auth.expires_in);
    let mut next_poll = tokio::time::Instant::now() + interval;
    let url = auth
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&auth.verification_uri);
    let _raw = RawMode::enable_if_interactive();

    loop {
        if tokio::time::Instant::now() >= deadline {
            bail!("browser login expired; run `codeus` to try again");
        }

        handle_wait_key(url)?;

        if tokio::time::Instant::now() >= next_poll {
            match client.poll_device_authorization(auth).await? {
                DeviceTokenStatus::Authorized(token) => return Ok(token),
                DeviceTokenStatus::Pending => {}
                DeviceTokenStatus::SlowDown => interval += Duration::from_secs(5),
                DeviceTokenStatus::Denied(message) => bail!("browser login was denied: {message}"),
                DeviceTokenStatus::Expired => {
                    bail!("browser login expired; run `codeus` again")
                }
            }
            next_poll = tokio::time::Instant::now() + interval;
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn handle_wait_key(url: &str) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Ok(());
    }
    if !crossterm::event::poll(Duration::from_millis(0))? {
        return Ok(());
    }
    let crossterm::event::Event::Key(key) = crossterm::event::read()? else {
        return Ok(());
    };
    if matches!(key.code, crossterm::event::KeyCode::Char('c' | 'C')) {
        if copy_to_clipboard(url).is_ok() {
            println!(
                "\r{}",
                Terminal::detect_with_theme(None).success("Copied sign-in link.")
            );
        } else {
            println!(
                "\r{}",
                Terminal::detect_with_theme(None)
                    .muted("Could not copy automatically; select the URL above.")
            );
        }
    }
    Ok(())
}

fn copy_to_clipboard(value: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut child = Command::new("pbcopy").stdin(Stdio::piped()).spawn()?;

    #[cfg(target_os = "windows")]
    let mut child = Command::new("clip").stdin(Stdio::piped()).spawn()?;

    #[cfg(all(unix, not(target_os = "macos")))]
    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .spawn()
        .or_else(|_| {
            Command::new("xclip")
                .args(["-selection", "clipboard"])
                .stdin(Stdio::piped())
                .spawn()
        })
        .or_else(|_| {
            Command::new("xsel")
                .args(["--clipboard", "--input"])
                .stdin(Stdio::piped())
                .spawn()
        })?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(value.as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        bail!("clipboard command failed")
    }
}

struct RawMode {
    enabled: bool,
}

impl RawMode {
    fn enable_if_interactive() -> Self {
        let enabled = io::stdin().is_terminal()
            && io::stdout().is_terminal()
            && crossterm::terminal::enable_raw_mode().is_ok();
        Self { enabled }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if self.enabled {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

fn save_token_set(config: &mut Config, token: TokenSet) -> Result<()> {
    config.session_token = Some(token.session_token);
    config.refresh_token = token.refresh_token;
    config.token_expires_at = match token.expires_in {
        Some(expires_in) => Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock is before the Unix epoch")?
                .as_secs()
                + expires_in,
        ),
        None => None,
    };
    Ok(())
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let status = Command::new("open").arg(url).status();

    #[cfg(target_os = "linux")]
    let status = Command::new("xdg-open").arg(url).status();

    #[cfg(target_os = "windows")]
    let status = Command::new("cmd").args(["/C", "start", "", url]).status();

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let status = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "automatic browser opening is not supported on this platform",
    ));

    let status = status.context("failed to launch browser command")?;
    if !status.success() {
        bail!("browser command exited with {status}");
    }

    Ok(())
}
