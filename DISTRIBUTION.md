# Distribution and Updates

Codeus CLI should be release-ready before the first public install command is advertised. The install surface is part of user trust: it verifies platform, installs the right binary, and leaves users on a supported update path.

## Recommended Release Order

1. Publish GitHub Releases with signed archives for each supported target.
2. Add install scripts that fetch those release artifacts.
3. Add Homebrew and npm wrappers that install the same artifacts.
4. Add startup update checks once a stable release metadata endpoint exists.

This lets us test one source of truth first, then layer package managers over it.

The repo now includes:

```text
scripts/install.sh
scripts/install.ps1
dist/npm
dist/homebrew/codeus.rb.template
.github/workflows/ci.yml
.github/workflows/release.yml
dist/codeus-release.example.json
```

## Channels

### Curl

Good first public path for macOS and Linux:

```bash
curl -fsSL https://install.quantumclub.sh/codeus.sh | sh
```

The script detects OS/arch, downloads the matching release archive, verifies checksum from `SHA256SUMS`, installs `codeus` into `~/.local/bin`, and prints PATH guidance.

### Homebrew

Best macOS path once releases are stable:

```bash
brew install quantumclub/tap/codeus
```

Use the formula template in `dist/homebrew/codeus.rb.template`, filled with the release version and checksums.

### npm

Useful because many developer machines already have npm:

```bash
npm install -g @quantumclub/codeus
```

Keep npm as a thin launcher/downloader package. The package in `dist/npm` downloads the native release asset during `postinstall` and shells out to that binary at runtime.

### Cargo

Good for contributors, not primary distribution while `publish = false`:

```bash
cargo install --path .
```

If we later publish to crates.io, treat it as a developer channel and keep binary releases as the main path.

## Update Mechanism

Codeus checks for updates on startup, but it does not surprise-replace itself without user consent unless the install channel supports that safely.

Recommended behavior:

- On `codeus` startup, make a short-timeout request to release metadata.
- Cache the latest-seen check time and only check once per day.
- If a newer stable version exists, print a concise notice with the command for the user's install channel.
- For Homebrew and npm installs, print `brew upgrade codeus` or `npm update -g @quantumclub/codeus`.
- Allow `CODEUS_NO_UPDATE_CHECK=1` and `config.toml` opt-out for managed environments.

The default metadata URL is:

```text
https://github.com/atomicfield/codeus-cli/releases/latest/download/codeus-release.json
```

Self-replacement for direct binary installs should wait until we add atomic replacement and rollback. Package-manager installs should defer to the package manager.
