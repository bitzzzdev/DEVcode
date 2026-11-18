import { NodeFileSystem } from "@effect/platform-node"
import { compile, emitEffectImported, emitEffectShape, emitPromise, write } from "@devcode/httpapi-codegen"
import {
  ClientApi,
  clientNamespace,
  deprecatedClientPaths,
  effectOmitEndpoints,
  groupNames,
  promiseOmitEndpoints,
} from "@devcode/protocol/client"
import { Agent } from "@devcode/schema/agent"
import { Artifact } from "@devcode/schema/artifact"
import { Automation } from "@devcode/schema/automation"
import { Command } from "@devcode/schema/command"
import { Config } from "@devcode/schema/config"
import { Credential } from "@devcode/schema/credential"
import { Event } from "@devcode/schema/event"
import { EventLog } from "@devcode/schema/event-log"
import { FileDiff } from "@devcode/schema/file-diff"
import { FileSystem } from "@devcode/schema/filesystem"
import { Form } from "@devcode/schema/form"
import { Generate } from "@devcode/schema/generate"
import { Instruction } from "@devcode/schema/instruction"
import { InstructionEntry } from "@devcode/schema/instruction-entry"
import { Integration } from "@devcode/schema/integration"
import { Job } from "@devcode/schema/job"
import { Location } from "@devcode/schema/location"
import { Mcp } from "@devcode/schema/mcp"
import { Model } from "@devcode/schema/model"
import { Money } from "@devcode/schema/money"
import { Permission } from "@devcode/schema/permission"
import { PermissionSaved } from "@devcode/schema/permission-saved"
import { Plugin } from "@devcode/schema/plugin"
import { Project } from "@devcode/schema/project"
import { Worktree } from "@devcode/schema/worktree"
import { AgentAttachment, FileAttachment, Prompt, PromptMention } from "@devcode/schema/prompt"
import { PromptInput } from "@devcode/schema/prompt-input"
import { Provider } from "@devcode/schema/provider"
import { Pty } from "@devcode/schema/pty"
import { PtyTicket } from "@devcode/schema/pty-ticket"
import { Question } from "@devcode/schema/question"
import { Reference } from "@devcode/schema/reference"
import { AbsolutePath, RelativePath } from "@devcode/schema/schema"
import { Session } from "@devcode/schema/session"
import { SessionMessage } from "@devcode/schema/session-message"
import { SessionInbox } from "@devcode/schema/session-inbox"
import { Shell } from "@devcode/schema/shell"
import { Skill } from "@devcode/schema/skill"
import { Snapshot } from "@devcode/schema/snapshot"
import { UsageLimits } from "@devcode/schema/usage-limits"
import { Vcs } from "@devcode/schema/vcs"
import { WebSearch } from "@devcode/schema/websearch"
import { Workspace } from "@devcode/schema/workspace"
import { Effect, Schema } from "effect"
import { fileURLToPath } from "url"

const promiseContract = compile(ClientApi, {
  groupNames,
  omitEndpoints: promiseOmitEndpoints,
  namespace: clientNamespace,
  aliases: deprecatedClientPaths,
})
const effectContract = compile(ClientApi, {
  groupNames,
  omitEndpoints: effectOmitEndpoints,
  namespace: clientNamespace,
  aliases: deprecatedClientPaths,
})
const effectTypeReferences = [
  ...namespaceTypes("Agent", "@devcode/schema/agent", Agent),
  ...namespaceTypes("Artifact", "@devcode/schema/artifact", Artifact),
  ...namespaceTypes("Automation", "@devcode/schema/automation", Automation),
  ...namespaceTypes("Command", "@devcode/schema/command", Command),
  ...namespaceTypes("Config", "@devcode/schema/config", Config),
  ...namespaceTypes("Credential", "@devcode/schema/credential", Credential),
  ...namespaceTypes("Event", "@devcode/schema/event", Event),
  ...namespaceTypes("EventLog", "@devcode/schema/event-log", EventLog),
  ...namespaceTypes("FileDiff", "@devcode/schema/file-diff", FileDiff),
  ...namespaceTypes("FileSystem", "@devcode/schema/filesystem", FileSystem),
  ...namespaceTypes("Form", "@devcode/schema/form", Form),
  ...namespaceTypes("Generate", "@devcode/schema/generate", Generate),
  ...namespaceTypes("Instruction", "@devcode/schema/instruction", Instruction),
  ...namespaceTypes("InstructionEntry", "@devcode/schema/instruction-entry", InstructionEntry),
  ...namespaceTypes("Integration", "@devcode/schema/integration", Integration),
  ...namespaceTypes("Job", "@devcode/schema/job", Job),
  typeReference("Location.PublicRef", "@devcode/schema/location", Location.PublicRef),
  typeReference("Location.PublicInfo", "@devcode/schema/location", Location.PublicInfo),
  ...namespaceTypes("Mcp", "@devcode/schema/mcp", Mcp),
  ...namespaceTypes("Model", "@devcode/schema/model", Model),
  ...namespaceTypes("Money", "@devcode/schema/money", Money),
  ...namespaceTypes("Permission", "@devcode/schema/permission", Permission),
  ...namespaceTypes("PermissionSaved", "@devcode/schema/permission-saved", PermissionSaved),
  ...namespaceTypes("Plugin", "@devcode/schema/plugin", Plugin),
  ...namespaceTypes("Project", "@devcode/schema/project", Project),
  ...namespaceTypes("Worktree", "@devcode/schema/worktree", Worktree),
  ...namespaceTypes("PromptInput", "@devcode/schema/prompt-input", PromptInput),
  ...namespaceTypes("Provider", "@devcode/schema/provider", Provider),
  ...namespaceTypes("Pty", "@devcode/schema/pty", Pty),
  ...namespaceTypes("PtyTicket", "@devcode/schema/pty-ticket", PtyTicket),
  ...namespaceTypes("Question", "@devcode/schema/question", Question),
  ...namespaceTypes("Reference", "@devcode/schema/reference", Reference),
  ...namespaceTypes("Session", "@devcode/schema/session", Session),
  ...namespaceTypes("SessionMessage", "@devcode/schema/session-message", SessionMessage),
  ...namespaceTypes("SessionInbox", "@devcode/schema/session-inbox", SessionInbox),
  ...namespaceTypes("Shell", "@devcode/schema/shell", Shell),
  ...namespaceTypes("Skill", "@devcode/schema/skill", Skill),
  ...namespaceTypes("Snapshot", "@devcode/schema/snapshot", Snapshot),
  ...namespaceTypes("UsageLimits", "@devcode/schema/usage-limits", UsageLimits),
  ...namespaceTypes("Vcs", "@devcode/schema/vcs", Vcs),
  ...namespaceTypes("WebSearch", "@devcode/schema/websearch", WebSearch),
  ...namespaceTypes("Workspace", "@devcode/schema/workspace", Workspace),
  typeReference("Prompt", "@devcode/schema/prompt", Prompt),
  typeReference("PromptMention", "@devcode/schema/prompt", PromptMention),
  typeReference("FileAttachment", "@devcode/schema/prompt", FileAttachment),
  typeReference("AgentAttachment", "@devcode/schema/prompt", AgentAttachment),
  typeReference("AbsolutePath", "@devcode/schema/schema", AbsolutePath),
  typeReference("RelativePath", "@devcode/schema/schema", RelativePath),
]

await Effect.runPromise(
  Effect.all(
    [
      write(
        emitPromise(promiseContract, {
          mutableOutputs: true,
        }),
        fileURLToPath(new URL("../src/promise/generated", import.meta.url)),
      ),
      write(
        emitEffectImported(effectContract, {
          module: "../../contract",
          api: "ClientApi",
          shapeModule: "../api/api.js",
        }),
        fileURLToPath(new URL("../src/effect/generated", import.meta.url)),
      ),
      write(
        emitEffectShape(effectContract, {
          typeReferences: effectTypeReferences,
          outputTypes: {
            "event.subscribe": {
              name: "DevCodeEvent",
              import: 'import type { DevCodeEvent } from "@devcode/protocol/groups/event"',
            },
          },
        }),
        fileURLToPath(new URL("../src/effect/api", import.meta.url)),
      ),
    ],
    { concurrency: 3, discard: true },
  ).pipe(Effect.provide(NodeFileSystem.layer)),
)

function namespaceTypes(namespace: string, module: string, values: object) {
  return Object.entries(values).flatMap(([name, schema]) =>
    Schema.isSchema(schema) ? [typeReference(`${namespace}.${name}`, module, schema)] : [],
  )
}

function typeReference(name: string, module: string, schema: Schema.Top) {
  return {
    schema,
    name,
    import: `import type { ${name.split(".")[0]} } from ${JSON.stringify(module)}`,
  }
}
