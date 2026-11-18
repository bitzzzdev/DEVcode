// TODO: Keep additional network capabilities inside Schema and Protocol as the client grows; /effect must never import
// Core or Server. Preserve these datatype exports so internal model reorganizations do not require caller migrations.
import type { Effect } from "effect"
import type { DevCode } from "./client.js"

export * from "./generated/index"
export { DevCode } from "./client.js"
export type {
  AgentApi,
  AppApi,
  CommandApi,
  ConfigApi,
  EventApi,
  IntegrationApi,
  ModelApi,
  PluginApi,
  ProviderApi,
  ReferenceApi,
  RpcApi,
  RpcClient,
  WebSearchApi,
  SessionApi,
  SkillApi,
} from "./api.js"
export { Agent } from "@devcode/schema/agent"
export { Command } from "@devcode/schema/command"
export { Config } from "@devcode/schema/config"
export { Credential } from "@devcode/schema/credential"
export { Event } from "@devcode/schema/event"
export { EventLog } from "@devcode/schema/event-log"
export { FileSystem } from "@devcode/schema/filesystem"
export { Form } from "@devcode/schema/form"
export { Integration } from "@devcode/schema/integration"
export { Location } from "@devcode/schema/location"
export { Model } from "@devcode/schema/model"
export { Permission } from "@devcode/schema/permission"
export { PermissionSaved } from "@devcode/schema/permission-saved"
export { Project } from "@devcode/schema/project"
export { Worktree } from "@devcode/schema/worktree"
export { Vcs } from "@devcode/schema/vcs"
export { Provider } from "@devcode/schema/provider"
export { Pty } from "@devcode/schema/pty"
export { Question } from "@devcode/schema/question"
export { Reference } from "@devcode/schema/reference"
export { WebSearch } from "@devcode/schema/websearch"
export { AbsolutePath, RelativePath } from "@devcode/schema/schema"
export { Session } from "@devcode/schema/session"
export { SessionInbox } from "@devcode/schema/session-inbox"
export { SessionMessage } from "@devcode/schema/session-message"
export { Skill } from "@devcode/schema/skill"
export { Prompt } from "@devcode/schema/prompt"
export { PromptInput } from "@devcode/schema/prompt-input"
export type { DevCodeEvent } from "@devcode/protocol/groups/event"
export type DevCodeClient = Effect.Success<ReturnType<typeof DevCode.make>>
