# Security

## Threat Model

### Overview

DEVcode is an AI-powered coding assistant that runs locally on your machine. It provides an agent system with access to powerful tools including shell execution, file operations, and web access.

### No Sandbox

DEVcode does **not** sandbox the agent. The permission system exists as a UX feature to help users stay aware of what actions the agent is taking - it prompts for confirmation before executing commands, writing files, etc. However, it is not designed to provide security isolation.

If you need true isolation, run DEVcode inside a Docker container or VM.

### Server Mode

Server mode is opt-in only. When enabled, set `DEVCODE_SERVER_PASSWORD` to require HTTP Basic Auth. Without this, the server runs unauthenticated (with a warning). It is the end user's responsibility to secure the server - any functionality it provides is not a vulnerability.

### Out of Scope

| Category                        | Rationale                                                               |
| ------------------------------- | ----------------------------------------------------------------------- |
| **Server access when opted-in** | If you enable server mode, API access is expected behavior              |
| **Sandbox escapes**             | The permission system is not a sandbox (see above)                      |
| **LLM provider data handling**  | Data sent to your configured LLM provider is governed by their policies |
| **MCP server behavior**         | External MCP servers you configure are outside our trust boundary       |
| **Malicious config files**      | Users control their own config; modifying it is not an attack vector    |

## Reporting Security Issues

This is a private fork; there is no upstream security team for this codebase, and upstream does not handle issues found here.

To report a security issue, open a confidential issue at
[gitlab.com/bitzzdev/DEVcode](https://gitlab.com/bitzzdev/DEVcode) describing the vulnerability and how to reproduce it, and keep it private to you and the maintainer until it is resolved.

## Escalation

If you cannot use confidential issues, contact the maintainer through the address on their GitLab profile. There is no response-time guarantee.
