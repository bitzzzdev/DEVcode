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
- **Artifacts, not package managers.** Binaries come from CircleCI build artifacts. There is no npm package,
  brew formula, winget entry, or install script for this fork, and the built-in update check reports the
  [releases page](https://gitlab.com/bitzzdev/DEVcode/-/releases) instead of phoning a dead update service.
- Everything else (CLI commands, agents, the models.dev provider catalog, MCP, plugins, the Effect-based
  server) is upstream's, unchanged on purpose.

## Installation

Binaries are built and stored as artifacts by CircleCI (see the `build` workflow in `.circleci/config.yml`). Each build
uploads `packages/cli/dist/DEVcode-<target>`; download the artifact for your platform:

| Platform            | Artifact directory          |
| ------------------- | --------------------------- |
| Linux x64 (glibc)   | `DEVcode-linux-x64`         |
| Linux x64 (baseline CPU, no AVX2) | `DEVcode-linux-x64-baseline` |
| Linux x64 (musl)  | `DEVcode-linux-x64-musl`    |
| Linux arm64 (glibc) | `DEVcode-linux-arm64`       |
| Linux arm64 (musl)  | `DEVcode-linux-arm64-musl`  |

Place the `DEVcode` binary on your `$PATH` and run `DEVcode` inside a project directory.

### Quick install (recommended)

```sh
# Use a gitlab.com token with read_api scope (GITLAB_TOKEN env var, glab's
# stored token, or an interactive prompt). The script detects the platform,
# downloads the matching binary, verifies SHA-256, and installs it.
curl -fsSL -H "PRIVATE-TOKEN: $GITLAB_TOKEN" \
  https://gitlab.com/bitzzdev/DEVcode/-/raw/dev/install.sh | sh -s -- v0.0.1
```

### Building from source

Requires [Bun](https://bun.sh) 1.4.4.

```bash
git clone https://gitlab.com/bitzzdev/DEVcode.git
cd DEVcode
bun install
cd packages/cli
DEVCODE_TARGET=linux-x64 bun run script/build.ts
```

`DEVCODE_TARGET` selects one output from `packages/cli/script/build.ts` (`linux-x64`, `linux-arm64`, the musl
and baseline variants above); `bun run script/build.ts --single` builds for the machine you are on.

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

MIT, as upstream. See [LICENSE](./LICENSE) and [CREDITS.md](./CREDITS.md) for third-party attributions that must
be preserved.
