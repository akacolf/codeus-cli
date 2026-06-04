use crate::api::{ClientConfig, CodeusClient, DeviceAuthorization, DeviceTokenStatus, TokenSet};
use crate::config::{Config, DEFAULT_API_BASE, DEFAULT_MODEL};
use crate::terminal::Terminal;
use anyhow::{bail, Context, Result};
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
        api_base: api_base
            .or_else(|| stored.api_base.clone())
            .unwrap_or_else(|| DEFAULT_API_BASE.to_string()),
        model: model
            .or_else(|| stored.model.clone())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
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
        api_base: api_base
            .or_else(|| config.api_base.clone())
            .unwrap_or_else(|| DEFAULT_API_BASE.to_string()),
        model: model
            .or_else(|| config.model.clone())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
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

    println!();
    println!(
        "{}",
        terminal.title("Sign in to your QC account to connect Codeus CLI")
    );
    if let Some(message) = &auth.message {
        println!("{message}");
    } else {
        println!(
            "{}",
            terminal.muted("Browser did not open? Use the URL below to sign in.")
        );
        println!();
        println!("{url}");
        if let Some(user_code) = &auth.user_code {
            println!();
            println!("Use code: {}", terminal.selection(user_code));
        }
    }

    if !no_browser {
        match open_browser(url) {
            Ok(()) => println!(
                "{}",
                terminal.muted("Opened your browser. Waiting for approval...")
            ),
            Err(error) => println!(
                "{} Could not open a browser automatically: {error}",
                terminal.warn("!")
            ),
        }
    } else {
        println!("{}", terminal.muted("Waiting for browser approval..."));
    }
}

async fn wait_for_device_token(
    client: &CodeusClient,
    auth: &DeviceAuthorization,
) -> Result<TokenSet> {
    let mut interval = Duration::from_secs(auth.interval);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(auth.expires_in);

    loop {
        if tokio::time::Instant::now() >= deadline {
            bail!("browser login expired; run `codeus login` to try again");
        }

        tokio::time::sleep(interval).await;
        match client.poll_device_token(&auth.device_code).await? {
            DeviceTokenStatus::Authorized(token) => return Ok(token),
            DeviceTokenStatus::Pending => {}
            DeviceTokenStatus::SlowDown => interval += Duration::from_secs(5),
            DeviceTokenStatus::Denied(message) => bail!("browser login was denied: {message}"),
            DeviceTokenStatus::Expired => bail!("browser login expired; run `codeus login` again"),
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
    let status = std::process::Command::new("open").arg(url).status();

    #[cfg(target_os = "linux")]
    let status = std::process::Command::new("xdg-open").arg(url).status();

    #[cfg(target_os = "windows")]
    let status = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .status();

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
