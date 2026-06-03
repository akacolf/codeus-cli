# AGENTS.md

Codeus CLI is the terminal-native companion to QC Codeus. Keep it focused, fast, and safe to distribute.

## Product posture

- The binary name is `codeus`.
- This is a Rust CLI, not an Electron surface.
- Default to QC's hosted proxy at `https://api.quantumclub.sh/v1`.
- Do not expose upstream provider keys. The CLI may store a Codeus session token, but provider credentials stay server-side.
- Treat this as a distribution mitigation path for users who hit desktop signing, notarization, or installer friction.

## Development

```bash
cargo fmt
cargo check
cargo run -- "explain this repo"
cargo run -- chat
```

Rust is the source of truth. Avoid adding shell, Node, or Bun runtime requirements unless there is a strong packaging reason.

## Style

- Keep command behavior explicit and boring.
- Prefer small modules over a giant `main.rs`.
- Use `anyhow` for application errors and typed structs for protocol payloads.
- Keep config backward-compatible once users may have `~/.config/codeus/config.toml`.
