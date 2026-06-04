### Codeus CLI

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

Running `codeus` with no arguments starts the interactive CLI. First launch shows the Codeus logo, lets users choose a named syntax-style terminal theme and coding font preference with arrow keys and a live preview, then signs in with a Quantum Club account through the hosted Codeus proxy. Users can skip sign-in and connect later with `/login` or `codeus login`.

The default API base is:

```text
https://api.quantumclub.sh/v1
```

## Interactive chat

`codeus` and `codeus chat` open the terminal chat surface with the Codeus logo, streamed replies, and slash commands:

```text
/help, /commands       Show the command list
/status                Show model, API base, cwd, and thread size
/threads               List saved local threads
/resume [id|latest]    Resume a saved thread
/compact               Compact older messages into summary context
/read <path>           Read a workspace file into the thread
/search <pattern> [path] Search workspace text
/git status|diff       Run safe git inspection commands
/run <command>         Run an approved command prefix
/approvals             Show approved command prefixes
/trace                 Show local telemetry status
/theme                 Change the terminal theme and font
/font                  Change the coding font preference
/login [--no-browser]  Connect a QC account
/logout                Remove the stored session
/cwd [path], /cd [path] Show or change the session working directory
/model                 Show the active model
/api                   Show the active API base
/clear, /reset         Clear this thread
/logo                  Print the Codeus logo
/exit, /quit           Leave chat
```

Plain text is sent as a chat message after a QC account is connected. Typing `/` opens the command menu underneath the prompt, filters as you type, and lets Tab complete the highlighted command. Changing `/cwd` resets the active transcript so the model gets fresh workspace context.

Each interactive session is recorded as a JSONL thread under:

```text
~/.config/codeus/threads
```

Use `/threads` to list recent threads and `/resume latest` or `/resume <id>` to continue one. Codeus builds model context deterministically on each turn from the current working directory, any compacted conversation summary, and `AGENTS.override.md` / `AGENTS.md` files from the project root down to the current directory. Repository instruction files are capped to a 32 KiB budget so they do not crowd out the conversation.

Use `/compact` to fold older messages into a local summary. Codeus also auto-compacts once the active transcript grows beyond the local byte budget, keeping the newest messages live.

## Local tools and approvals

Codeus exposes typed local tools through slash commands. `/read` and `/search` are restricted to the active workspace, skip large generated directories during search, and truncate large outputs before saving them into the thread. `/git status` and `/git diff` are read-only inspection commands.

The model can also request local tools during a turn through native Chat Completions tool calls. Codeus sends the local tool schema with each chat request, executes returned tool calls with the same workspace and approval rules, records tool observations, then asks the model to continue. A hidden `codeus_tool_call` block remains as a fallback for upstreams that ignore native tool schemas. A turn is capped at four model-requested tool steps to avoid loops.

Supported model-requested tool names are:

```text
read_file
search_text
git_status
git_diff
run_command
```

`/run` does not invoke a shell. It parses the command into an argv vector and runs it only when it matches an approved prefix. Built-in prefixes are:

```text
cargo check
cargo fmt
git status
git diff
ls
pwd
rg
```

Add extra prefixes manually in `config.toml` when needed:

```toml
approved_command_prefixes = [
  ["cargo", "test"],
]
```

Use `/approvals` to see the effective prefix list.

Set `CODEUS_TRACE=1` to write local JSONL telemetry events to:

```text
~/.config/codeus/events.jsonl
```

Trace events currently cover local tool observations and model turn latency. They are disabled by default.

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

Set `CODEUS_SESSION_TOKEN` only to a Codeus session token issued by QC's hosted proxy or support tooling. Do not set it to an upstream provider API key; provider credentials stay server-side.

The first-run setup stores only local CLI preferences and the Codeus session:

```toml
theme = "auto"
font = "jetbrains-mono"
onboarded = true
```

The font preference records what the user wants Codeus to use in terminal-themed surfaces. Terminal emulators still control the actual rendered font, so users may need to select the matching font in their terminal profile.

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
