# Contributing to DevCode

DEVcode is a fork of [opencode](https://github.com/anomalyco/opencode). Changes come from the fork owner;
upstream fixes still land upstream, and this fork re-syncs from there.

New providers should rarely require DevCode changes. Add the provider to [models.dev](https://github.com/anomalyco/models.dev) first.

## Development

The active port is the Rust workspace under `crates/`. The TypeScript build under `packages/` stays frozen
at v0.0.1 for compatibility; all new work happens on the `rust-port` branch.

The Rust port needs [Bun](https://bun.sh) 1.4.4 (for the frozen TypeScript tooling) and
[Rust](https://rustup.rs) 1.99.1 with the `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-gnu`, and
`aarch64-unknown-linux-musl` targets for cross builds.

From the repository root:

```bash
bun install
cd crates
cargo build --locked
```

`DEVcode run --help` prints the CLI surface; `DEVcode serve --help` the service flags.

### Crates

- `devcode`: binary, `serve`, `service`, `tui`, `run`, `mcp auth` subcommands
- `devcode-core`: session core, durable event log, SQLite storage
- `devcode-runner`: headless agent loop, tools, model streaming
- `devcode-ai`: Anthropic and OpenAI-compatible provider clients
- `devcode-mcp`: MCP client (stdio + Streamable HTTP), OAuth
- `devcode-plugin`: native plugin host (cdylib + plugin.json)
- `devcode-skills`: skill discovery and frontmatter
- `devcode-tools`: builtin tools (read, write, edit, glob, grep, shell)
- `devcode-tui`: ratatui terminal interface
- `devcode-util`: XDG paths, helpers
- `devcode-service`: managed-service config and registration
- `devcode-plugin-figma`: Figma plugin cdylib

## Verification

Run tests and typecheck from the affected crate directory, never the repository root:

```bash
cd crates
cargo test --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

The canonical full check is `cargo test --workspace --locked` from `crates/`. TypeScript typechecks remain
under `packages/` via `bun run check` but are frozen.

After changing the public Protocol or Server `HttpApi`, regenerate the SDK with `bun run generate` from
`packages/client`; never edit generated client files directly. The Rust port does not ship an SDK; it serves
the wire shapes directly from `serve.rs`.

Follow the repository [style guide](./AGENTS.md) and the house rules in `crates/README.md`.
