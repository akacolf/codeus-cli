use crate::api::{CodeusClient, Message};
use crate::stream_and_print;
use anyhow::Result;
use std::io::{self, Write};

pub async fn run(client: CodeusClient) -> Result<()> {
    let mut messages = vec![Message {
        role: "system".to_string(),
        content: "You are Codeus CLI, QC's terminal-native coding assistant. Keep answers concise and useful.".to_string(),
    }];

    println!("Codeus CLI");
    println!("Type /exit to quit, /clear to reset the thread.");

    loop {
        print!("codeus> ");
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

        match input {
            "/exit" | "/quit" => break,
            "/clear" => {
                messages.truncate(1);
                println!("Thread cleared.");
                continue;
            }
            _ => {}
        }

        messages.push(Message {
            role: "user".to_string(),
            content: input.to_string(),
        });

        let assistant = stream_and_print(&client, &messages).await?;
        println!();
        messages.push(Message {
            role: "assistant".to_string(),
            content: assistant,
        });
    }

    Ok(())
}
