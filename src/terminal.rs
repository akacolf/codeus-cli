use std::io::{self, IsTerminal};

pub struct Terminal {
    color: bool,
}

impl Terminal {
    pub fn detect() -> Self {
        let color = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Self { color }
    }

    pub fn logo(&self) -> String {
        let raw = [
            "  ______          __",
            " / ____/___  ____/ /__  __  ______",
            "/ /   / __ \\/ __  / _ \\/ / / / ___/",
            "/ /___/ /_/ / /_/ /  __/ /_/ (__  )",
            "\\____/\\____/\\__,_/\\___/\\__,_/____/",
        ]
        .join("\n");

        self.accent(&raw)
    }

    pub fn accent(&self, value: &str) -> String {
        if self.color {
            format!("\x1b[36m{value}\x1b[0m")
        } else {
            value.to_string()
        }
    }

    pub fn muted(&self, value: &str) -> String {
        if self.color {
            format!("\x1b[2m{value}\x1b[0m")
        } else {
            value.to_string()
        }
    }

    pub fn success(&self, value: &str) -> String {
        if self.color {
            format!("\x1b[32m{value}\x1b[0m")
        } else {
            value.to_string()
        }
    }

    pub fn warn(&self, value: &str) -> String {
        if self.color {
            format!("\x1b[33m{value}\x1b[0m")
        } else {
            value.to_string()
        }
    }
}
