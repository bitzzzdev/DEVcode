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

- `devcode` binary: `--version`, `serve [--service]`, `service <status|start|stop|restart|get|set|unset>`, `run [--model provider/model[#variant]] [--session ID] [--workdir PATH] [--max-steps N] <prompt>` (headless agent loop)
- `/api/info` (basic auth, user `devcode`) and a minimal `/openapi.json`
- `devcode-service`: managed-service config read/write (unknown keys round-trip), atomic 0600 registration file, default ports (`latest`/`dev`/`beta`/`next` 0xc0e0/49376, `local` 0xc0e1/49377, win32 0x70e0/0x70e1), `/api/info` health probe, constant-time password compare
- `devcode-util`: XDG path resolution (same `DEVCODE_CONFIG_DIR`/`DEVCODE_TEST_HOME` overrides as the Bun build)
- `devcode-core`: SQLite storage (`devcode.db`, 0600, WAL, foreign keys). Fresh databases get the full initial schema verbatim from `packages/core/src/database/schema.gen.ts` and record the 62 legacy Drizzle migration ids so the Bun build never replays them. Existing Bun-created databases are opened without touching their schema. `serve` opens the database at startup like the Bun build does. Durable per-aggregate event log (`append_durable`, `read_log`, `reserve_sequence`) mirrors `packages/core/src/bus.ts` storage semantics: sequences start at 0, `event_sequence` keeps max, `event` rows cascade on aggregate delete.
- `devcode-ai` + `devcode-tools` are wired into `DEVcode run`: the headless agent loop creates a session (or continues `--session`), enqueues the prompt as a durable `session.inbox.enqueued` item and delivers it (`session.inbox.delivered` → user message row), then streams steps: history is rebuilt from `session_message` + `session_part` (parts re-attached in ordinal order, skipping system/model-switch rows), the assistant row is written by `session.step.started`, text deltas print to stdout, tool calls execute through `devcode-tools` (cwd = session directory), and `session.message.content.updated`/`session.step.ended`/`session.execution.*` land as durable events with the message rows updated in projection transactions. Verified end-to-end against a local mock SSE server (tool call round-trip, `--session` continuation, connection-refused failure path: session marked `failed`, `session.execution.failed` recorded, exit 1).
- `devcode-tools`: the six core model-facing tools ported from `packages/core/src/tool/plugin/` with TS-compatible semantics: `read` (1-based line numbers, `<line>: <content>`, offset/limit, limit cap 2000; directories list entries one per line; binary/PDF/skill-URI support not ported), `write` (creates parent dirs, reports Created/Wrote), `edit` (exact-match replace; unique or replaceAll; ambiguous count error names `replaceAll`; the Bun build's whitespace/quote-normalizing fallback tiers are not ported), `glob` and `grep` (ignore-aware walking, include filter, literal mode, case sensitivity, TS model-content format `Found N matches` / `Line N:`, cap 100 matches), `shell` (`sh -c`, default 120s timeout that kills the process; `DEVCODE_SESSION_ID` passthrough; full output saved to `tool-output/` when truncated). Output caps follow `tool-output.ts`: 50 KiB / 2000 lines, overflow retained on disk.
- Project resolution + persistence (`resolve_project`/`persist_project`): git repos (sha1 of `git-remote:<normalized origin>`, falling back to the earliest root commit, else `global`; linked worktrees resolve to the main worktree as canonical), hg roots, plain directories (sha1 of `directory:<path>`). `persist` upserts the `project` row (canonical/vcs refreshed on every resolve).
- `devcode-ai`: provider layer ported from the Bun build's provider layer. Anthropic Messages API (`POST /v1/messages`, `anthropic-version: 2023-06-01`, env `ANTHROPIC_API_KEY`/`ANTHROPIC_BASE_URL`) and OpenAI-compatible chat completions (`/chat/completions`, `stream_options.include_usage`, env `OPENAI_API_KEY`/`OPENAI_BASE_URL`); unknown provider ids fall through to the OpenAI-compatible endpoint. Sync (`ureq`, blocking SSE via a hand-rolled parser); no async runtime. Model refs parse as `providerID/model[#variant]`. Not ported: models.dev catalog, pricing tables, OAuth credential stores, retries, rate limiting.
- Session creation (`create_session`, `create_session_for_directory`, `get_session`, `list_sessions`): commits the durable `session.created` event and the `session_v2` row in one transaction (the bun build splits this into event + projector; single-process atomicity instead). Ids are `ses_`+descending / `evt_`+ascending / `msg_` ascending later; the adjective-noun slug list is ported verbatim. `session.created` payload mirrors `session-event.ts` (`sessionID`, `projectID`, `location`, `slug`, `version`, optional `parentID`/`title`/`agent`/`model`/`metadata`/`permissions`).

## Deliberate deviations

- Events are appended first and projections written afterwards in a separate transaction (the Bun build shares this split); a crash in between loses the projection, not the event. Session creation is the one exception: `session.created` + `session_v2` commit in one transaction.
- Assistant rows always carry an empty `content: []` envelope; parts live in `session_part` rows (the `experimental.part_rows` path of the Bun build).
- `session_message.data` mirrors the Bun build's JSON shape (assistant keeps `content: []` and gains `finish`/`cost`/`tokens`/`time.completed` at step end; user rows carry `text`/optional `files`/`agents`/`skills`/`metadata`).
- Static system prompt; AGENTS.md instruction discovery is not ported. Cost is recorded as 0 until a pricing table exists; token usage is recorded. `DEVcode run` executes tools after the stream completes; no streaming tool loop, no retries, no permission prompts, no compaction. The Bun build's provider-agnostic Anthropic/OpenAI request path is approximated directly (no Bedrock/Vertex/Copilot OAuth yet).

- Port default is 49376 (Bun build: 49375, upstream opencode: 49374) so the Bun and Rust services can run side by side
- A healthy incumbent whose version does not contain `rust` (e.g. the Bun build) is never replaced or killed by this build; `service stop --force` overrides
- `service remote` is not supported yet

## Not ported yet

TUI (solid-js/opentui, to become ratatui), the HTTP layer for sessions/messages (`run` is the only agent entry point), the rest of session core (compaction, inbox steer/queue at boundaries, assistant `time.firstToken` streaming event parity, fork/revert), MCP, LSP, permission prompts, instruction discovery, OAuth credential stores, pricing tables, the JavaScript plugin host (to become native Rust plugins), `sdk`/`protocol` codegen. See .progress.md (gitignored) for the working ledger.
