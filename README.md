# Codeus CLI

Terminal-native Codeus for working with QC's hosted agent service without the desktop app distribution surface.

This repository is intentionally small at first: it provides a Rust `codeus` binary, streams chat completions from the QC proxy, and connects to a user's QC account with a browser login. The desktop app can keep maturing independently while the CLI gives users a lower-friction way to try Codeus.

## Install

Rust is required.

```bash
cargo install --path .
```

## Usage

```bash
codeus
codeus "explain this repo"
codeus run "summarize the current project"
codeus chat
codeus login
codeus account
codeus usage
codeus config show
codeus config set model sonnet-4.6
```

Running `codeus` with no arguments starts the interactive CLI. If no QC account is connected yet, the CLI starts the browser login flow automatically.

The default API base is:

```text
https://api.quantumclub.sh/v1
```

## Configuration

Config is stored at:

```text
~/.config/codeus/config.toml
```

Supported environment overrides:

```bash
CODEUS_API_BASE=https://api.quantumclub.sh/v1
CODEUS_MODEL=sonnet-4.6
CODEUS_SESSION_TOKEN=...
```

The CLI sends `Authorization: Bearer <token>` after the QC browser login completes. `CODEUS_SESSION_TOKEN` and `codeus login --token ...` remain available for development and support, but users should not need to paste tokens for normal sessions.

## Development

This is a Rust CLI, so Cargo is the build and debugging toolchain:

```bash
cargo fmt
cargo check
cargo run -- "explain this repo"
cargo run -- chat
cargo build --release
```

Bun is useful for TypeScript projects, but it is not needed here unless the project later adds a separate web UI or packaging tool that justifies a JavaScript runtime.

## Roadmap

- Local workspace tools with explicit approval
- Project-aware chat history
- Shell completions and Homebrew packaging
- Shared model/catalog metadata with the desktop app
