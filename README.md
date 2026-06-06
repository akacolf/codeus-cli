### Codeus CLI

Terminal-native Codeus for working with QC's hosted agent service without the desktop app distribution surface.

This repository is intentionally small at first: it provides a Rust `codeus` binary, streams chat completions from the QC proxy, and connects to a user's QC account with a browser login. The desktop app can keep maturing independently while the CLI gives users a lower-friction way to try Codeus.

## About

Codeus CLI is a small Rust CLI for QC Codeus. It keeps provider credentials server-side, stores only local CLI preferences and a Codeus session token, and gives users a distribution-friendly fallback when desktop signing, notarization, or installer friction gets in the way.

See [ABOUT.md](ABOUT.md) for the project scope and non-goals.

## Install

Rust is required.

```bash
cargo install --path .
```

Public distribution is prepared around signed release archives first, then curl, Homebrew, and npm wrappers over the same artifacts:

```bash
curl -fsSL https://install.quantumclub.sh/codeus.sh | sh
brew install quantumclub/tap/codeus
npm install -g @quantumclub/codeus
```

See [DISTRIBUTION.md](DISTRIBUTION.md) for the release checklist and update plan.

## Usage

```bash
codeus
codeus "explain this repo"
codeus run "summarize the current project"
codeus chat
codeus login
codeus account
codeus usage
codeus doctor
codeus harness
codeus status --output-format json
codeus state
codeus update
codeus config show
codeus config env
codeus config set model sonnet-4.6
codeus config set permission_mode read-only
codeus config set sandbox_policy policy-only
```

Running `codeus` with no arguments starts the interactive CLI. First launch shows the Codeus logo, lets users choose a named syntax-style terminal theme with a live preview, then signs in with a Quantum Club account.

The default API base is:

```text
https://api.quantumclub.sh/v1
```

## Interactive chat

`codeus` and `codeus chat` open the terminal chat surface with the Codeus logo, streamed replies, and slash commands:

```text
/help, /commands       Show the command list
/status                Show account, model, project, and context
/resume [id|latest]    Resume a saved chat
/swap                  Show Omniston v1beta8 swap helper
/theme                 Change the terminal theme
/model                 Show available models
/clear, /reset         Clear context and start fresh
/exit, /quit           Leave chat
```

Plain text is sent as a chat message after a QC account is connected. Typing `/` opens the command menu underneath the prompt, filters as you type, and lets Tab complete the highlighted command.

Each interactive session is recorded as a JSONL saved chat under:

```text
~/.config/codeus/threads
```

Use `/resume latest` or `/resume <id>` to continue one. Codeus builds model context deterministically on each turn from the current working directory, any compacted conversation summary, and `AGENTS.override.md` / `AGENTS.md` files from the project root down to the current directory. Repository instruction files are capped to a 32 KiB budget so they do not crowd out the conversation. Codeus also auto-compacts once the active transcript grows beyond the local byte budget, keeping the newest messages live.

## Internal tools and approvals

The model can request local tools during a turn through native Chat Completions tool calls. Codeus sends the local tool schema with each chat request, executes returned tool calls with the same workspace and approval rules, records tool observations, then asks the model to continue. A hidden `codeus_tool_call` block remains as a fallback for upstreams that ignore native tool schemas. A turn is capped at four model-requested tool steps to avoid loops.

Supported model-requested tool names are:

```text
read_file
list_files
search_text
replace_in_file
apply_patch
git_status
git_diff
run_command
```

`apply_patch` accepts structured operations and validates the full patch before writing:

```json
{
  "operations": [
    { "op": "add", "path": "notes.txt", "content": "hello\n" },
    { "op": "replace", "path": "src/main.rs", "old_text": "old", "new_text": "new" },
    { "op": "delete", "path": "tmp.txt" }
  ]
}
```

`run_command` does not invoke a shell. It parses shell-style quoting into an argv vector and runs it only when the structured command policy matches an approved rule. Built-in prefixes are:

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

Command execution currently uses a `policy-only` sandbox policy: Codeus enforces argv policy, workspace permission mode, timeouts, and output limits, but it does not claim native macOS Seatbelt or Linux namespace isolation yet.

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
CODEUS_SANDBOX_POLICY=policy-only
CODEUS_NO_UPDATE_CHECK=1
CODEUS_UPDATE_ENDPOINT=https://github.com/atomicfield/codeus-cli/releases/latest/download/codeus-release.json
```

The CLI sends `Authorization: Bearer <token>` after the QC browser login completes. Browser login uses the website-backed device-link API:

```text
/device/link/init
/device/link/poll
```

The same `CODEUS_API_BASE` is used for chat, account, and usage:

```text
/chat/completions
/auth/whoami
/balance
```

`CODEUS_SESSION_TOKEN` and `codeus login --token ...` remain available for development and support, but users should not need to paste tokens for normal sessions.

Set `CODEUS_SESSION_TOKEN` only to a Codeus session token issued by QC's hosted proxy or support tooling. Do not set it to an upstream provider API key; provider credentials stay server-side.

The first-run setup stores only local CLI preferences and the Codeus session:

```toml
theme = "auto"
onboarded = true
```

`codeus` checks for CLI updates at startup at most once per day. The default update metadata is the `codeus-release.json` asset attached to the latest GitHub Release. Disable checks with:

```toml
update_check_disabled = true
```

or:

```bash
CODEUS_NO_UPDATE_CHECK=1
```

## Diagnostics and automation

Run `codeus doctor` before first use or when support needs a health check. It validates the resolved config, auth token state, API base, model, workspace access, saved-chat storage, telemetry path, permission mode, approved command prefixes, and authenticated account route when a token is available.

Core inspection commands support machine-readable output:

```bash
codeus --output-format json doctor
codeus --output-format json status
codeus --output-format json state
codeus --output-format json config show
codeus --output-format json config env
codeus --output-format json run "summarize this repo"
```

`status` reports the resolved model, auth source, permission mode, current workspace, loaded `AGENTS.md` stats, saved chat count, trace path, and approved command prefixes. `state` reports the latest saved local chat.

Permission modes can be set with `--permission-mode`, `CODEUS_PERMISSION_MODE`, or `config.toml`:

```bash
codeus --permission-mode read-only "summarize this repo"
codeus config set permission_mode workspace-write
```

`read-only` allows workspace inspection tools and read-only command prefixes such as `git status`, `git diff`, `ls`, `pwd`, and `rg`. `workspace-write` is the default and allows exact-snippet file edits plus approved command prefixes.

## Development

This is a Rust CLI, so Cargo is the build and debugging toolchain:

```bash
cargo fmt
cargo check
cargo test
cargo run -- "explain this repo"
cargo run -- chat
cargo build --release
```

For hot reload during CLI development, install `cargo-watch` and run:

```bash
cargo install cargo-watch
cargo watch -x 'run -- chat'
cargo watch -x 'run -- "explain this repo"'
```

The test suite includes a deterministic mock proxy harness that starts a local `/v1` API, exercises `doctor`, `status --output-format json`, and a streamed `run`, and does not require real QC credentials.

## Vercel API proxy

Production login and billing live in the Quantum Club website API (`/api/codeus/v1/*`), which is rewritten for `https://api.quantumclub.sh/v1`. The repo also includes a Rust Vercel Function for standalone proxy experiments, but the hosted website API is the preferred production deployment because Vercel does not provide a first-class Rust project preset.

The optional Rust function can expose:

```text
/v1/device/link/init
/v1/device/link/poll
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

If deploying this repository as a standalone Vercel project with the `Other` preset, verify that `/v1/device/link/init` returns JSON. If Vercel serves a 404 before this function runs, use the website API host instead of the standalone Rust project.

For local prebuilt Vercel builds, the Rust runtime invokes `cargo zigbuild` for the Linux serverless target. Install `rustup`, `zig`, and `cargo-zigbuild` before running:

```bash
vercel build --prod --yes
```

Required Vercel environment variables:

```bash
CODEUS_AUTH_BASE=https://your-qc-auth-service.example.com
CODEUS_CHAT_BASE=https://your-openai-compatible-chat-service.example.com/v1
```

For browser login, the proxy tries the website-style device-link paths first:

```text
CODEUS_AUTH_BASE/device/link/init
CODEUS_AUTH_BASE/device/link/poll
```

It falls back to OAuth-style and older local defaults:

```text
CODEUS_AUTH_BASE/oauth/device/code
CODEUS_AUTH_BASE/oauth/token
CODEUS_AUTH_BASE/device/code
CODEUS_AUTH_BASE/device/token
CODEUS_AUTH_BASE/refresh
```

If the QC auth service uses different routes, set the exact `CODEUS_DEVICE_CODE_URL`, `CODEUS_DEVICE_TOKEN_URL`, and `CODEUS_REFRESH_URL` overrides below.

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

- Signed release archives and checksum verification
- Curl, Homebrew, and npm install channels
- Startup update notices and `codeus update` for direct binary installs
- Shell completions
- Shared model/catalog metadata with the desktop app
