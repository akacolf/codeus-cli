# About Codeus CLI

Codeus CLI is the terminal-native companion to QC Codeus. It gives users a small Rust binary named `codeus` for chatting with QC's hosted agent service, inspecting local projects, and using safe workspace tools without depending on the desktop app distribution path.

The CLI defaults to QC's hosted proxy at:

```text
https://api.quantumclub.sh/v1
```

Provider credentials stay server-side. The CLI may store a Codeus session token in `~/.config/codeus/config.toml`, but upstream model provider keys are never distributed to users or written into local CLI config.

## Scope

- Fast terminal chat with streamed responses.
- Browser login for Quantum Club accounts through the hosted proxy.
- Local thread persistence and resume support.
- Project-aware context from `AGENTS.md` files.
- Read-only workspace inspection tools and explicitly approved local commands.
- A distribution-friendly Rust implementation for users who cannot or do not want to install the desktop app.

## Non-goals

- It is not an Electron or desktop UI.
- It is not a place to expose upstream provider credentials.
- It should not grow shell, Node, or Bun runtime requirements unless packaging clearly needs them.
