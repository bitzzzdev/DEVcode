# Contributing to DevCode

DEVcode is a private fork of [opencode](https://github.com/anomalyco/opencode). Changes come from the fork owner;
upstream fixes still land upstream, and this fork re-syncs from there.

New providers should rarely require DevCode changes. Add the provider to [models.dev](https://github.com/anomalyco/models.dev) first.

## Development

DEVcode requires the Bun version in the `packageManager` field of [`package.json`](./package.json) or newer. From the repository root:

```bash
bun install
bun dev [directory]
```

`bun dev` runs the V2 CLI and TUI. Pass a directory to open another project, or `.` to open this repository.

### Packages

- `packages/schema`: shared wire and storage contracts
- `packages/core`: domain behavior and persistence
- `packages/protocol`: public API definitions
- `packages/server`: HTTP server and runtime composition
- `packages/client`: generated TypeScript clients
- `packages/cli`: command-line entrypoint and service lifecycle
- `packages/tui`: terminal interface
- `packages/plugin`: plugin API
- `packages/plugins/*`: built-in plugins
- `packages/util`: shared utilities

## Verification

Run typechecks and tests from the affected package directory, never the repository root:

```bash
cd packages/core
bun run test
bun typecheck
```

The canonical full lint and typecheck is `bun run check` from the repository root.

After changing the public Protocol or Server `HttpApi`, run `bun run generate` from `packages/client`; never edit generated client files directly.

Follow the repository [style guide](./AGENTS.md).
