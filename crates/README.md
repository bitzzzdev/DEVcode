# Rust port of DEVcode

Ground-up port of the TypeScript build (packages/*) to Rust. Branch `rust-port`. The Bun build ships v0.0.1 as `DEVcode` and stays the supported binary until this build reaches parity; this build's version always carries `rust` (currently `0.1.0-rust`).

## Build and run

```bash
cd crates
cargo build --release
./target/release/DEVcode --version
./target/release/DEVcode service start
```

State directories are XDG-rooted like the Bun build (`XDG_CONFIG_HOME/devcode`, `XDG_STATE_HOME/devcode`, `XDG_DATA_HOME/devcode`, `XDG_CACHE_HOME/devcode`, `/tmp/devcode`), so `service.json` config and registration files keep the same format. Do not edit them by hand while either build is running.

## What exists today

- `devcode` binary: `--version`, `serve [--service]`, `service <status|start|stop|restart|get|set|unset>`
- `/api/info` (basic auth, user `devcode`) and a minimal `/openapi.json`
- `devcode-service`: managed-service config read/write (unknown keys round-trip), atomic 0600 registration file, default ports (`latest`/`dev`/`beta`/`next` 0xc0e0/49376, `local` 0xc0e1/49377, win32 0x70e0/0x70e1), `/api/info` health probe, constant-time password compare
- `devcode-util`: XDG path resolution (same `DEVCODE_CONFIG_DIR`/`DEVCODE_TEST_HOME` overrides as the Bun build)

## Deliberate deviations

- Port default is 49376 (Bun build: 49375, upstream opencode: 49374) so the Bun and Rust services can run side by side
- A healthy incumbent whose version does not contain `rust` (e.g. the Bun build) is never replaced or killed by this build; `service stop --force` overrides
- `service remote` is not supported yet

## Not ported yet

Everything else: the TUI (solid-js/opentui, to become ratatui), the full server and session core, tools (shell, file, patch, grep), MCP, LSP, providers, the JavaScript plugin host (to become native Rust plugins), `process-daemon` (already Rust), `sdk`/`protocol` codegen. See .progress.md (gitignored) for the working ledger.
