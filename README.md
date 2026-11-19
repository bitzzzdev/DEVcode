<p align="center">
  <b>DEVcode</b>
</p>

<p align="center">The open source AI coding agent, terminal-only.</p>

<p align="center">
  <a href="https://gitlab.com/bitzzdev/DEVcode">GitLab</a> ·
  <a href="https://gitlab.com/bitzzdev/DEVcode/-/releases">Releases</a> ·
  <a href="./CONTRIBUTING.md">Contributing</a> ·
  <a href="./SECURITY.md">Security</a>
</p>

DEVcode is a lightweight fork of [opencode](https://github.com/anomalyco/opencode). It keeps every upstream
command, agent, model, and provider, and ships only the terminal UI: the web app, desktop app, site, and cloud
services were removed to keep the binary small and the machine quiet.

## What is different in this fork

- **Terminal UI only.** No browser UI, no desktop app, no hosted services.
- **Keyboard-first.** Mouse capture defaults to off. Turn it on with `"mouse": true` in the TUI config if you
  want it back.
- **Rust native.** The binary is built with Rust (crates/), not TypeScript (packages/cli/), for a smaller,
  faster footprint (1.3 MB vs 57 MB). The TypeScript (v0.0.1) is still shipped for compatibility but no longer
  powers the service.
- **Artifacts, not package managers.** Binaries come from CircleCI build artifacts. There is no npm package,
  brew formula, winget entry, or install script for this fork, and the built-in update check reports the
  [releases page](https://gitlab.com/bitzzdev/DEVcode/-/releases) instead of phoning a dead update service.
- Everything else (CLI commands, agents, the models.dev provider catalog, MCP, plugins, the Effect-based
  server) is upstream's, unchanged on purpose.

## Installation

Binaries are built and stored as artifacts by CircleCI (see the `rust-dist` workflow in `.circleci/config.yml`). Each build
uploads `DEVcode-rust-<target>`; download the artifact for your platform:

| Platform            | Artifact directory          |
| ------------------- | --------------------------- |
| Linux x64 (glibc)   | `DEVcode-rust-linux-x64`         |
| Linux x64 (baseline CPU, no AVX2) | `DEVcode-rust-linux-x64-baseline` |
| Linux x64 (musl)  | `DEVcode-rust-linux-x64-musl`    |
| Linux arm64 (glibc) | `DEVcode-rust-linux-arm64`       |
| Linux arm64 (musl)  | `DEVcode-rust-linux-arm64-musl`  |

Place the `DEVcode` binary on your `$PATH` and run `DEVcode` inside a project directory.

### Quick install (recommended)

```sh
# The repository is public, so downloads are anonymous. The script detects
# the platform, downloads the matching binary, verifies SHA-256, and
# installs it.
curl -fsSL https://gitlab.com/bitzzdev/DEVcode/-/raw/rust-port/install.sh | sh -s -- 0.1.0-rust
```

### Building from source

Requires [Bun](https://bun.sh) 1.4.4 and [Rust](https://rustup.rs) 1.99.1.

```bash
git clone https://gitlab.com/bitzzdev/DEVcode.git
cd DEVcode
bun install
cd crates
cargo build --release --locked --target x86_64-unknown-linux-gnu
```

The Rust binary lives at `crates/target/<triple>/release/DEVcode`. `cargo build --release --locked` builds for the
host triple; pass `--target` for cross-compilation (see `crates/Cargo.toml` for the release profile). The
`rust-dist` CI job builds all six targets on push to `rust-port`.

## Agents

DEVcode includes two built-in agents you can switch between with the `Tab` key.

- **build** - Default, full-access agent for development work
- **plan** - Read-only agent for analysis and code exploration
  - Denies file edits by default
  - Asks permission before running bash commands
  - Ideal for exploring unfamiliar codebases or planning changes

Also included is a **general** subagent for complex searches and multistep tasks.
This is used internally and can be invoked using `@general` in messages.

## Documentation

- [Contributing](./CONTRIBUTING.md)
- [Security](./SECURITY.md)
- [Credits](./CREDITS.md)
- Upstream opencode docs live at https://opencode.ai/docs and describe the features this fork keeps.

## License

XEH Product License. See [LICENSE](./LICENSE). Portions of this repository are derived from upstream
opencode (MIT); the upstream MIT copyright notice is preserved in that file. Third-party attributions are in
[CREDITS.md](./CREDITS.md).
