## Codeus CLI

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

Running `codeus` with no arguments starts the interactive CLI. First launch shows the Codeus terminal surface immediately; authentication starts only when the user runs `/login`, `codeus login`, or a command that requires an account.

The default API base is:

```text
https://api.quantumclub.sh/v1
```

## Interactive chat

`codeus` and `codeus chat` open the terminal chat surface with the Codeus logo, streamed replies, and slash commands:

```text
/help, /commands       Show the command list
/status                Show model, API base, cwd, and thread size
/login [--no-browser]  Connect a QC account
/logout                Remove the stored session
/cwd [path], /cd [path] Show or change the session working directory
/model                 Show the active model
/api                   Show the active API base
/clear, /reset         Clear this thread
/logo                  Print the Codeus logo
/exit, /quit           Leave chat
```

Plain text is sent as a chat message after a QC account is connected. Changing `/cwd` also resets the thread so the model gets fresh workspace context.

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

## Vercel API proxy

The repo also includes a Rust Vercel Function for the hosted `/v1` API surface the CLI expects:

```text
/v1/auth/device/code
/v1/auth/device/token
/v1/auth/refresh
/v1/chat/completions
/v1/account
/v1/usage
```

The Vercel entrypoint is `api/index.rs`, declared as its own `[[bin]]` in `Cargo.toml` per Vercel's Rust runtime requirements. `vercel.json` rewrites `/v1/*` into that Rust function, where Axum routes the individual API paths.

Deploy this repo to Vercel, set the server-side environment variables, then point the CLI at the deployment:

```bash
vercel pull --yes --environment preview
vercel deploy
codeus config set api_base https://your-codeus-api.vercel.app/v1
codeus login
```

For local prebuilt Vercel builds, the Rust runtime invokes `cargo zigbuild` for the Linux serverless target. Install `rustup`, `zig`, and `cargo-zigbuild` before running:

```bash
vercel build --prod --yes
```

Required Vercel environment variables:

```bash
CODEUS_AUTH_BASE=https://your-qc-auth-service.example.com
CODEUS_CHAT_BASE=https://your-openai-compatible-chat-service.example.com/v1
```

Optional exact upstream URL overrides:

```bash
CODEUS_DEVICE_CODE_URL=https://your-qc-auth-service.example.com/oauth/device/code
CODEUS_DEVICE_TOKEN_URL=https://your-qc-auth-service.example.com/oauth/token
CODEUS_REFRESH_URL=https://your-qc-auth-service.example.com/oauth/token
CODEUS_ACCOUNT_URL=https://your-qc-auth-service.example.com/account
CODEUS_USAGE_URL=https://your-qc-auth-service.example.com/usage
CODEUS_CHAT_COMPLETIONS_URL=https://your-openai-compatible-chat-service.example.com/v1/chat/completions
```

Optional secret/provider settings:

```bash
CODEUS_OAUTH_CLIENT_ID=codeus-cli
CODEUS_OAUTH_SCOPE="chat account usage"
CODEUS_CHAT_API_KEY=server-side-provider-or-gateway-key
CODEUS_ACCOUNT_BASE=https://your-qc-account-service.example.com
```

If `CODEUS_CHAT_API_KEY` is set, Vercel sends it to the chat upstream and forwards the user's Codeus session as `x-codeus-session-token`. If it is not set, Vercel forwards the user's bearer token as `Authorization`.

## Roadmap

- Local workspace tools with explicit approval
- Project-aware chat history
- Shell completions and Homebrew packaging
- Shared model/catalog metadata with the desktop app
