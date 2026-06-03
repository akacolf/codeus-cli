# Codeus CLI

Terminal-native Codeus for working with QC's hosted agent service without the desktop app distribution surface.

This repository is intentionally small at first: it provides a Rust `codeus` binary, streams chat completions from the QC proxy, and keeps credentials in a local config file or environment variable. The desktop app can keep maturing independently while the CLI gives users a lower-friction way to try Codeus.

## Install

Rust is required.

```bash
cargo install --path .
```

## Usage

```bash
codeus "explain this repo"
codeus run "summarize the current project"
codeus chat
codeus login
codeus config show
codeus config set model sonnet-4.6
```

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

The CLI sends `Authorization: Bearer <token>` when a token is available. Until the production auth flow is finalized for CLI users, `CODEUS_SESSION_TOKEN` is the cleanest development path.

## Roadmap

- Browser/device login flow
- Local workspace tools with explicit approval
- Project-aware chat history
- Shell completions and Homebrew packaging
- Shared model/catalog metadata with the desktop app
