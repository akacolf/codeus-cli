use crate::api::CodeusClient;
use anyhow::Result;
use serde_json::Value;

pub async fn print_account(client: &CodeusClient) -> Result<()> {
    let account = client.account().await?;
    println!("Account");
    print_summary_or_json(&account);

    match client.usage().await {
        Ok(usage) => {
            println!();
            println!("Usage");
            print_summary_or_json(&usage);
        }
        Err(error) => {
            println!();
            println!("Usage unavailable: {error}");
        }
    }

    Ok(())
}

pub async fn print_usage(client: &CodeusClient) -> Result<()> {
    let usage = client.usage().await?;
    print_summary_or_json(&usage);
    Ok(())
}

fn print_summary_or_json(value: &Value) {
    if let Some(object) = value.as_object() {
        let mut printed = false;
        for key in [
            "name",
            "email",
            "id",
            "plan",
            "usage_remaining",
            "remaining",
            "credits_remaining",
            "creditsRemaining",
            "balanceMicros",
            "freeCreditMicros",
            "walletAddress",
            "activePlan",
            "usage",
            "limit",
            "used",
            "reset_at",
            "resetAt",
        ] {
            if let Some(value) = object.get(key) {
                println!("{key}: {}", format_json_value(value));
                printed = true;
            }
        }

        if printed {
            return;
        }
    }

    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
}

fn format_json_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        _ => value.to_string(),
    }
}
