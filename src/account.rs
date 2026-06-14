use crate::api::CodeusClient;
use anyhow::Result;
use serde_json::Value;

pub async fn print_account(client: &CodeusClient) -> Result<()> {
    let account = client.account().await?;
    println!("Anaxonic account");
    println!("{}", account_display_summary(&account));
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

fn account_display_summary(value: &Value) -> String {
    let email = account_email(value);
    let plan = if email
        .as_deref()
        .is_some_and(|email| email.eq_ignore_ascii_case("colfgg54@gmail.com"))
    {
        "Anaxonic Enterprise".to_string()
    } else {
        account_value(
            value,
            &["activePlan", "active_plan", "plan", "subscription"],
        )
        .unwrap_or_else(|| "Anaxonic account".to_string())
    };

    match email {
        Some(email) => format!("{plan} · {email}"),
        None => plan,
    }
}

fn account_email(value: &Value) -> Option<String> {
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
            .and_then(Value::as_str)
            .filter(|email| !email.trim().is_empty())
        {
            return Some(email.to_string());
        }
    }
    object
        .values()
        .find_map(|value| value.is_object().then(|| account_email(value)).flatten())
}

fn account_value(value: &Value, keys: &[&str]) -> Option<String> {
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
                if key == "plan" || key == "activePlan" {
                    let value = enterprise_plan_override(object)
                        .unwrap_or_else(|| format_json_value(value));
                    println!("{key}: {value}");
                } else {
                    println!("{key}: {}", format_json_value(value));
                }
                printed = true;
            }
        }

        if enterprise_plan_override(object).is_some()
            && !object.contains_key("plan")
            && !object.contains_key("activePlan")
        {
            println!("plan: QC Enterprise");
            printed = true;
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

fn enterprise_plan_override(object: &serde_json::Map<String, Value>) -> Option<String> {
    for key in [
        "email",
        "accountEmail",
        "account_email",
        "userEmail",
        "user_email",
    ] {
        if object
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|email| email.eq_ignore_ascii_case("colfgg54@gmail.com"))
        {
            return Some("QC Enterprise".to_string());
        }
    }
    None
}

fn format_json_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        _ => value.to_string(),
    }
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
