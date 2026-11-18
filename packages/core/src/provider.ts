export * as Provider from "./provider.js"

import { Context, Effect, Layer, Option, Schema, Stream, Struct } from "effect"
import { Provider } from "@devcode/schema/provider"
import { Model } from "@devcode/schema/model"
import { makeLocationNode } from "@devcode/util/effect/app-node"
import { HttpTimeout, type ProviderPackage } from "@devcode/ai/effect"
import { Npm } from "@devcode/util/npm"
import type { DeepMutable } from "./schema.js"
import { importModule, resolveModule } from "@devcode/util/runtime-import"
import { Bus } from "./bus.js"
import { Integration } from "./integration.js"
import { State } from "./state.js"
import { IntegrationConnection } from "./integration/connection.js"
import { Credential } from "@devcode/schema/credential"
import { Location } from "./location.js"
import { freeze } from "immer"
import { AISDKNative } from "./aisdk-native.js"

export { AISDK_PREFIX, aisdk, isAISDK, mergeHeaders, mergeOverlay, packageName } from "./aisdk-native.js"

export const ID = Provider.ID
export type ID = typeof ID.Type

export class LoadError extends Schema.TaggedError<LoadError>()("Provider.LoadError", {
  package: Schema.String,
  cause: Schema.Defect(),
}) {}

const packages = new Map<string, Promise<unknown>>()
const builtins = new Map<string, () => Promise<unknown>>([
  ["@devcode/ai/providers/alibaba", () => import("@devcode/ai/providers/alibaba")],
  ["@devcode/ai/providers/alibaba/chat", () => import("@devcode/ai/providers/alibaba/chat")],
  ["@devcode/ai/providers/alibaba/messages", () => import("@devcode/ai/providers/alibaba/messages")],
  ["@devcode/ai/providers/alibaba/responses", () => import("@devcode/ai/providers/alibaba/responses")],
  ["@devcode/ai/providers/alibaba/images", () => import("@devcode/ai/providers/alibaba/images")],
  ["@devcode/ai/providers/alibaba/embeddings", () => import("@devcode/ai/providers/alibaba/embeddings")],
  ["@devcode/ai/providers/amazon-bedrock", () => import("@devcode/ai/providers/amazon-bedrock")],
  ["@devcode/ai/providers/amazon-bedrock/mantle", () => import("@devcode/ai/providers/amazon-bedrock/mantle")],
  [
    "@devcode/ai/providers/amazon-bedrock/mantle/chat",
    () => import("@devcode/ai/providers/amazon-bedrock/mantle/chat"),
  ],
  [
    "@devcode/ai/providers/amazon-bedrock/mantle/messages",
    () => import("@devcode/ai/providers/amazon-bedrock/mantle/messages"),
  ],
  [
    "@devcode/ai/providers/amazon-bedrock/mantle/responses",
    () => import("@devcode/ai/providers/amazon-bedrock/mantle/responses"),
  ],
  ["@devcode/ai/providers/anthropic", () => import("@devcode/ai/providers/anthropic")],
  ["@devcode/ai/providers/anthropic-compatible", () => import("@devcode/ai/providers/anthropic-compatible")],
  ["@devcode/ai/providers/azure", () => import("@devcode/ai/providers/azure")],
  ["@devcode/ai/providers/azure/chat", () => import("@devcode/ai/providers/azure/chat")],
  ["@devcode/ai/providers/azure/responses", () => import("@devcode/ai/providers/azure/responses")],
  ["@devcode/ai/providers/azure/images", () => import("@devcode/ai/providers/azure/images")],
  ["@devcode/ai/providers/azure/embeddings", () => import("@devcode/ai/providers/azure/embeddings")],
  ["@devcode/ai/providers/azure/messages", () => import("@devcode/ai/providers/azure/messages")],
  ["@devcode/ai/providers/baseten", () => import("@devcode/ai/providers/baseten")],
  ["@devcode/ai/providers/baseten/decisions", () => import("@devcode/ai/providers/baseten/decisions")],
  ["@devcode/ai/providers/byteplus/videos", () => import("@devcode/ai/providers/byteplus/videos")],
  ["@devcode/ai/providers/cerebras", () => import("@devcode/ai/providers/cerebras")],
  ["@devcode/ai/providers/cloudflare-ai-gateway", () => import("@devcode/ai/providers/cloudflare-ai-gateway")],
  [
    "@devcode/ai/providers/cloudflare-ai-gateway/decisions",
    () => import("@devcode/ai/providers/cloudflare-ai-gateway/decisions"),
  ],
  ["@devcode/ai/providers/cloudflare-workers-ai", () => import("@devcode/ai/providers/cloudflare-workers-ai")],
  [
    "@devcode/ai/providers/cloudflare-workers-ai/decisions",
    () => import("@devcode/ai/providers/cloudflare-workers-ai/decisions"),
  ],
  [
    "@devcode/ai/providers/cloudflare-workers-ai/images",
    () => import("@devcode/ai/providers/cloudflare-workers-ai/images"),
  ],
  ["@devcode/ai/providers/cohere", () => import("@devcode/ai/providers/cohere")],
  ["@devcode/ai/providers/cohere/chat", () => import("@devcode/ai/providers/cohere/chat")],
  ["@devcode/ai/providers/cohere/embeddings", () => import("@devcode/ai/providers/cohere/embeddings")],
  ["@devcode/ai/providers/deepinfra", () => import("@devcode/ai/providers/deepinfra")],
  ["@devcode/ai/providers/deepinfra/embeddings", () => import("@devcode/ai/providers/deepinfra/embeddings")],
  ["@devcode/ai/providers/deepseek", () => import("@devcode/ai/providers/deepseek")],
  ["@devcode/ai/providers/digitalocean", () => import("@devcode/ai/providers/digitalocean")],
  ["@devcode/ai/providers/digitalocean/decisions", () => import("@devcode/ai/providers/digitalocean/decisions")],
  ["@devcode/ai/providers/digitalocean/embeddings", () => import("@devcode/ai/providers/digitalocean/embeddings")],
  ["@devcode/ai/providers/digitalocean/images", () => import("@devcode/ai/providers/digitalocean/images")],
  ["@devcode/ai/providers/digitalocean/responses", () => import("@devcode/ai/providers/digitalocean/responses")],
  ["@devcode/ai/providers/fireworks", () => import("@devcode/ai/providers/fireworks")],
  ["@devcode/ai/providers/fireworks/embeddings", () => import("@devcode/ai/providers/fireworks/embeddings")],
  ["@devcode/ai/providers/google", () => import("@devcode/ai/providers/google")],
  ["@devcode/ai/providers/google/decisions", () => import("@devcode/ai/providers/google/decisions")],
  ["@devcode/ai/providers/google/interactions", () => import("@devcode/ai/providers/google/interactions")],
  ["@devcode/ai/providers/google/embeddings", () => import("@devcode/ai/providers/google/embeddings")],
  ["@devcode/ai/providers/google/images", () => import("@devcode/ai/providers/google/images")],
  ["@devcode/ai/providers/google/videos", () => import("@devcode/ai/providers/google/videos")],
  ["@devcode/ai/providers/google-vertex", () => import("@devcode/ai/providers/google-vertex")],
  ["@devcode/ai/providers/google-vertex/chat", () => import("@devcode/ai/providers/google-vertex/chat")],
  ["@devcode/ai/providers/google-vertex/gemini", () => import("@devcode/ai/providers/google-vertex/gemini")],
  [
    "@devcode/ai/providers/google-vertex/interactions",
    () => import("@devcode/ai/providers/google-vertex/interactions"),
  ],
  ["@devcode/ai/providers/google-vertex/messages", () => import("@devcode/ai/providers/google-vertex/messages")],
  ["@devcode/ai/providers/google-vertex/mistral", () => import("@devcode/ai/providers/google-vertex/mistral")],
  ["@devcode/ai/providers/google-vertex/responses", () => import("@devcode/ai/providers/google-vertex/responses")],
  ["@devcode/ai/providers/google-vertex/embeddings", () => import("@devcode/ai/providers/google-vertex/embeddings")],
  ["@devcode/ai/providers/google-vertex/images", () => import("@devcode/ai/providers/google-vertex/images")],
  ["@devcode/ai/providers/google-vertex-messages", () => import("@devcode/ai/providers/google-vertex-messages")],
  ["@devcode/ai/providers/groq", () => import("@devcode/ai/providers/groq")],
  ["@devcode/ai/providers/meta", () => import("@devcode/ai/providers/meta")],
  ["@devcode/ai/providers/meta/chat", () => import("@devcode/ai/providers/meta/chat")],
  ["@devcode/ai/providers/meta/images", () => import("@devcode/ai/providers/meta/images")],
  ["@devcode/ai/providers/meta/messages", () => import("@devcode/ai/providers/meta/messages")],
  ["@devcode/ai/providers/meta/responses", () => import("@devcode/ai/providers/meta/responses")],
  ["@devcode/ai/providers/minimax", () => import("@devcode/ai/providers/minimax")],
  ["@devcode/ai/providers/minimax/chat", () => import("@devcode/ai/providers/minimax/chat")],
  ["@devcode/ai/providers/minimax/images", () => import("@devcode/ai/providers/minimax/images")],
  ["@devcode/ai/providers/minimax/messages", () => import("@devcode/ai/providers/minimax/messages")],
  ["@devcode/ai/providers/minimax/responses", () => import("@devcode/ai/providers/minimax/responses")],
  ["@devcode/ai/providers/minimax/videos", () => import("@devcode/ai/providers/minimax/videos")],
  ["@devcode/ai/providers/mistral", () => import("@devcode/ai/providers/mistral")],
  ["@devcode/ai/providers/moonshot", () => import("@devcode/ai/providers/moonshot")],
  ["@devcode/ai/providers/moonshot/chat", () => import("@devcode/ai/providers/moonshot/chat")],
  ["@devcode/ai/providers/moonshot/messages", () => import("@devcode/ai/providers/moonshot/messages")],
  ["@devcode/ai/providers/moonshot/responses", () => import("@devcode/ai/providers/moonshot/responses")],
  ["@devcode/ai/providers/openai", () => import("@devcode/ai/providers/openai")],
  ["@devcode/ai/providers/openai/chat", () => import("@devcode/ai/providers/openai/chat")],
  ["@devcode/ai/providers/openai/responses", () => import("@devcode/ai/providers/openai/responses")],
  ["@devcode/ai/providers/openai/images", () => import("@devcode/ai/providers/openai/images")],
  ["@devcode/ai/providers/openai/embeddings", () => import("@devcode/ai/providers/openai/embeddings")],
  ["@devcode/ai/providers/openai-compatible", () => import("@devcode/ai/providers/openai-compatible")],
  [
    "@devcode/ai/providers/openai-compatible/responses",
    () => import("@devcode/ai/providers/openai-compatible/responses"),
  ],
  ["@devcode/ai/providers/openai-compatible/images", () => import("@devcode/ai/providers/openai-compatible/images")],
  [
    "@devcode/ai/providers/openai-compatible/decisions",
    () => import("@devcode/ai/providers/openai-compatible/decisions"),
  ],
  [
    "@devcode/ai/providers/openai-compatible/embeddings",
    () => import("@devcode/ai/providers/openai-compatible/embeddings"),
  ],
  [
    "@devcode/ai/providers/openai-compatible-responses",
    () => import("@devcode/ai/providers/openai-compatible-responses"),
  ],
  ["@devcode/ai/providers/devcode/decisions", () => import("@devcode/ai/providers/devcode/decisions")],
  ["@devcode/ai/providers/openrouter", () => import("@devcode/ai/providers/openrouter")],
  ["@devcode/ai/providers/openrouter/decisions", () => import("@devcode/ai/providers/openrouter/decisions")],
  ["@devcode/ai/providers/openrouter/embeddings", () => import("@devcode/ai/providers/openrouter/embeddings")],
  ["@devcode/ai/providers/openrouter/images", () => import("@devcode/ai/providers/openrouter/images")],
  ["@devcode/ai/providers/openrouter/videos", () => import("@devcode/ai/providers/openrouter/videos")],
  ["@devcode/ai/providers/openrouter/responses", () => import("@devcode/ai/providers/openrouter/responses")],
  ["@devcode/ai/providers/togetherai", () => import("@devcode/ai/providers/togetherai")],
  ["@devcode/ai/providers/typesafe-ai/decisions", () => import("@devcode/ai/providers/typesafe-ai/decisions")],
  ["@devcode/ai/providers/venice", () => import("@devcode/ai/providers/venice")],
  ["@devcode/ai/providers/venice/responses", () => import("@devcode/ai/providers/venice/responses")],
  ["@devcode/ai/providers/volcengine/videos", () => import("@devcode/ai/providers/volcengine/videos")],
  ["@devcode/ai/providers/vercel-ai-gateway", () => import("@devcode/ai/providers/vercel-ai-gateway")],
  ["@devcode/ai/providers/vercel-ai-gateway/chat", () => import("@devcode/ai/providers/vercel-ai-gateway/chat")],
  [
    "@devcode/ai/providers/vercel-ai-gateway/messages",
    () => import("@devcode/ai/providers/vercel-ai-gateway/messages"),
  ],
  [
    "@devcode/ai/providers/vercel-ai-gateway/responses",
    () => import("@devcode/ai/providers/vercel-ai-gateway/responses"),
  ],
  [
    "@devcode/ai/providers/vercel-ai-gateway/decisions",
    () => import("@devcode/ai/providers/vercel-ai-gateway/decisions"),
  ],
  ["@devcode/ai/providers/vercel-ai-gateway/images", () => import("@devcode/ai/providers/vercel-ai-gateway/images")],
  [
    "@devcode/ai/providers/vercel-ai-gateway/embeddings",
    () => import("@devcode/ai/providers/vercel-ai-gateway/embeddings"),
  ],
  ["@devcode/ai/providers/xai", () => import("@devcode/ai/providers/xai")],
  ["@devcode/ai/providers/xai/images", () => import("@devcode/ai/providers/xai/images")],
  ["@devcode/ai/providers/xai/videos", () => import("@devcode/ai/providers/xai/videos")],
  ["@devcode/ai/providers/zai", () => import("@devcode/ai/providers/zai")],
  ["@devcode/ai/providers/zai/chat", () => import("@devcode/ai/providers/zai/chat")],
  ["@devcode/ai/providers/zai/images", () => import("@devcode/ai/providers/zai/images")],
  ["@devcode/ai/providers/zai-coding-plan", () => import("@devcode/ai/providers/zai-coding-plan")],
  ["@devcode/ai/providers/zai-coding-plan/chat", () => import("@devcode/ai/providers/zai-coding-plan/chat")],
  ["@devcode/ai/providers/zai-coding-plan/messages", () => import("@devcode/ai/providers/zai-coding-plan/messages")],
  [
    "@devcode/ai/providers/zai-coding-plan/responses",
    () => import("@devcode/ai/providers/zai-coding-plan/responses"),
  ],
])

/** Native `@devcode/ai/providers/*` entrypoints bundled with Core and loadable by name from config. */
export const builtinPackages: ReadonlySet<string> = new Set(builtins.keys())

export const loadPackage = Effect.fn("Provider.loadPackage")(function* (input: string, npm?: Npm.Interface) {
  const specifier = AISDKNative.packageName(input)
  const builtin = builtins.get(specifier)
  if (builtin) return yield* importPackage(specifier, specifier, builtin)
  const resolved = yield* Effect.sync(() => {
    if (specifier.startsWith("file://") || specifier.startsWith("@devcode/ai/")) return specifier
    try {
      return import.meta.resolve(specifier)
    } catch {
      return undefined
    }
  })
  if (resolved) return yield* importPackage(specifier, resolved)
  if (!npm) {
    return yield* new LoadError({
      package: specifier,
      cause: new Error(`Provider package ${specifier} is not installed`),
    })
  }
  const parts = specifier.split("/")
  const root = specifier.startsWith("@") ? parts.slice(0, 2).join("/") : (parts[0] ?? specifier)
  const installed = yield* npm.add(root).pipe(Effect.mapError((cause) => new LoadError({ package: specifier, cause })))
  const entrypoint = yield* Effect.try({
    try: () => resolveModule(specifier, installed.directory),
    catch: (cause) => new LoadError({ package: specifier, cause }),
  })
  return yield* importPackage(specifier, entrypoint)
})

const settingsKeys = Struct.keys(Provider.Settings.schema.fields)
const providerOnlyKeys = settingsKeys.filter((key) => !(key in Model.Settings.schema.fields))

/** The provider options: every settings key DevCode doesn't read itself. */
export function nativeSettings(settings: Settings) {
  return Struct.omit(settings, settingsKeys)
}

/** Settings a model inherits from its provider. */
export function modelSettings(settings: Settings | undefined) {
  return settings && Struct.omit(settings, providerOnlyKeys)
}

const decodeTimeout = Schema.decodeUnknownOption(HttpTimeout)

/** Applied to `headerTimeout` when provider settings leave it unset or invalid. */
export const DEFAULT_TIMEOUT_MS = 300_000

/**
 * Applied to `chunkTimeout` when provider settings leave it unset or invalid. Some providers, such as DeepSeek, have
 * specifically asked clients to allow at least 15 minutes, because their models can stream nothing for minutes while
 * they reason.
 */
export const DEFAULT_CHUNK_TIMEOUT_MS = 900_000

/** One timeout setting without a default; invalid values are dropped. */
export function timeout(value: unknown) {
  return Option.getOrUndefined(decodeTimeout(value))
}

/** Request timeouts from provider settings, resolved for every route. `timeout` has no default. */
export function timeouts(settings: Readonly<Record<string, unknown>>) {
  return {
    timeout: timeout(settings.timeout),
    headerTimeout: timeout(settings.headerTimeout) ?? DEFAULT_TIMEOUT_MS,
    chunkTimeout: timeout(settings.chunkTimeout) ?? DEFAULT_CHUNK_TIMEOUT_MS,
  }
}

const decodeEncoding = Schema.decodeUnknownOption(Provider.Encoding)

/** Request body encoding from provider or model settings; invalid values are dropped. */
export function encoding(settings: Readonly<Record<string, unknown>>) {
  return Option.getOrUndefined(decodeEncoding(settings.encoding))
}

const decodeTls = Schema.decodeUnknownOption(Provider.Tls)

/** Client certificate from provider settings. An empty `cert` or `key`, such as a missing `{file:}`, leaves it unset. */
export function tls(settings: Readonly<Record<string, unknown>>) {
  const value = Option.getOrUndefined(decodeTls(settings.tls))
  return value?.cert && value.key ? value : undefined
}

export const Request = Provider.Request
export type Request = Provider.Request

export const Compaction = Provider.Compaction
export type Compaction = Provider.Compaction

export const Cache = Provider.Cache
export type Cache = Provider.Cache

export const Transport = Provider.Transport
export type Transport = Provider.Transport

export const Settings = Provider.Settings
export type Settings = Provider.Settings

export const Info = Provider.Info
export type Info = Provider.Info

export type MutableInfo = DeepMutable<Info>

export { Event } from "@devcode/schema/provider"

/** Provider metadata owns its drafts; model definitions remain immutable inputs. */
export type Definition = {
  readonly provider: Info
  readonly models: ReadonlyMap<Model.ID, Model.Info>
  readonly sourceConnection?: IntegrationConnection.Info
}

export interface Editor {
  readonly list: () => readonly Definition[]
  readonly get: (providerID: ID) => Definition | undefined
  readonly add: (definition: {
    readonly info: Info
    readonly models: readonly Model.Info[]
    readonly sourceConnection?: IntegrationConnection.Info
  }) => void
  readonly update: (providerID: ID, update: (provider: MutableInfo) => void) => void
  readonly remove: (providerID: ID) => void
  readonly models: {
    readonly set: (providerID: ID, models: readonly Model.Info[]) => void
    readonly update: (providerID: ID, modelID: Model.ID, update: (model: DeepMutable<Model.Info>) => void) => void
    readonly remove: (providerID: ID, modelID: Model.ID) => void
  }
}

export interface Snapshot {
  readonly records: ReadonlyMap<ID, Definition>
  readonly available: readonly Definition[]
  readonly providers: readonly Info[]
}

export interface Interface extends State.Transformable<Editor> {
  readonly get: (providerID: ID) => Effect.Effect<Info | undefined>
  readonly all: () => Effect.Effect<readonly Info[]>
  readonly available: () => Effect.Effect<readonly Info[]>
  /** Internal definition/access snapshot; raw model inventories never cross the HTTP API. */
  readonly snapshot: () => Effect.Effect<Snapshot>
}

export class Service extends Context.Service<Service, Interface>()("@devcode/Provider") {}

// Every location references the same index for a shared immutable definition array.
const definitions = new WeakMap<readonly Model.Info[], Map<ID, ReadonlyMap<Model.ID, Model.Info>>>()
function index(providerID: ID, models: readonly Model.Info[]) {
  const indexes = definitions.get(models) ?? new Map<ID, ReadonlyMap<Model.ID, Model.Info>>()
  const cached = indexes.get(providerID)
  if (cached) return cached
  // Model shares these definitions without copying, so a foreign definition takes this provider's identity here.
  const result = freeze(
    new Map(models.map((model) => [model.id, model.providerID === providerID ? model : { ...model, providerID }])),
    true,
  )
  indexes.set(providerID, result)
  definitions.set(models, indexes)
  return result
}

const layer = Layer.effect(
  Service,
  Effect.gen(function* () {
    const bus = yield* Bus.Service
    const integrations = yield* Integration.Service
    const location = yield* Location.Service
    const state = State.create<
      Map<
        ID,
        {
          provider: MutableInfo
          models: ReadonlyMap<Model.ID, Model.Info>
          sourceConnection?: IntegrationConnection.Info
        }
      >,
      Editor
    >({
      name: "provider",
      initial: () => new Map(),
      editor: (records) => {
        const maps = new WeakSet<ReadonlyMap<Model.ID, Model.Info>>()
        const models = new WeakSet<Model.Info>()
        const entry = (id: ID) => {
          const current = records.get(id)
          if (current) return current
          const added = { provider: Info.empty(id) as MutableInfo, models: new Map<Model.ID, Model.Info>() }
          records.set(id, added)
          maps.add(added.models)
          return added
        }
        const writable = (id: ID) => {
          const record = entry(id)
          if (maps.has(record.models)) return record.models as Map<Model.ID, Model.Info>
          const owned = new Map(record.models)
          record.models = owned
          maps.add(owned)
          return owned
        }
        return {
          list: () => Array.from(records.values()),
          get: (id) => records.get(id),
          add: (definition) => {
            records.set(definition.info.id, {
              provider: structuredClone(definition.info) as MutableInfo,
              models: index(definition.info.id, definition.models),
              sourceConnection: definition.sourceConnection,
            })
          },
          update: (id, update) => {
            const record = entry(id)
            update(record.provider)
            record.provider.id = id
            AISDKNative.rewrite(record.provider, {
              builtins: builtinPackages,
              specifier: record.provider.package,
              providerID: id,
              canonical: record.provider.canonical,
            })
          },
          remove: (id) => {
            records.delete(id)
          },
          models: {
            set: (id, values) => {
              entry(id).models = index(id, values)
            },
            update: (providerID, modelID, update) => {
              const record = entry(providerID)
              const target = writable(providerID)
              const current = target.get(modelID)
              const model = // An earlier add/set can publish and freeze an owned model within this fold.
                (
                  current && models.has(current) && !Object.isFrozen(current)
                    ? current
                    : current
                      ? structuredClone(current)
                      : Model.Info.default(providerID, modelID)
                ) as DeepMutable<Model.Info>
              update(model)
              model.id = modelID
              model.providerID = providerID
              AISDKNative.rewrite(model, {
                builtins: builtinPackages,
                specifier: model.package ?? record.provider.package,
                providerID,
                canonical: model.canonical ?? record.provider.canonical,
                modelID: model.modelID ?? modelID,
              })
              models.add(model)
              target.set(modelID, model)
            },
            remove: (providerID, modelID) => {
              if (!records.has(providerID)) return
              writable(providerID).delete(modelID)
            },
          },
        }
      },
      notify: () => notify,
    })
    // Registrations may outlive a borrowed service layer; their later disposal must
    // not query dependencies that have already closed.
    yield* Effect.addFinalizer(() => State.shutdown(state.reload()))
    let cached: { records: Snapshot["records"]; value: Snapshot } | undefined
    const snapshot = Effect.fn("Provider.snapshot")(function* () {
      while (true) {
        const revision = integrations.revision()
        const records = state.get()
        // Connection presence decides availability; resolving OAuth tokens belongs to execution.
        const connections = yield* integrations.list()
        // Either fold can disable a plugin that also contributed to the other domain.
        if (revision !== integrations.revision() || records !== state.get()) continue
        const byID = new Map(connections.map((integration) => [integration.id, integration]))
        const available = Array.from(records.values()).filter((record) => {
          if (record.provider.activation === "disabled") return false
          const integration = byID.get(record.provider.integrationID ?? Integration.ID.make(record.provider.id))
          // Never combine the previous account's discovered endpoints/models with a new connection.
          if (
            record.sourceConnection &&
            IntegrationConnection.key(record.sourceConnection) !==
              IntegrationConnection.key(integration?.connections[0])
          )
            return false
          if (record.provider.activation === "enabled") return true
          if (integration?.connections.length) return true
          return record.provider.integrationID === undefined && !integration
        })
        // A credential change that leaves the same definitions available is not a catalog change.
        if (
          cached?.records === records &&
          cached.value.available.length === available.length &&
          cached.value.available.every((record, index) => record === available[index])
        )
          return cached.value
        const value = freeze({ records, available, providers: available.map((record) => record.provider) }, true)
        cached = { records, value }
        return value
      }
    })
    let published: Snapshot | undefined
    const notify: Effect.Effect<void> = Effect.gen(function* () {
      const value = yield* snapshot()
      if (value === published) return
      published = value
      yield* bus.publish(
        Provider.Event.Updated,
        {},
        {
          location: { directory: location.directory, workspaceID: location.workspaceID },
        },
      )
    })
    yield* bus.subscribe([Integration.Event.Updated, Credential.Event.Switched]).pipe(
      Stream.runForEach(() => notify),
      Effect.forkScoped({ startImmediately: true }),
    )
    return Service.of({
      transform: state.transform,
      reload: state.reload,
      get: Effect.fn("Provider.get")((id) => Effect.sync(() => freeze(state.get().get(id)?.provider, true))),
      all: Effect.fn("Provider.all")(() =>
        Effect.sync(() => Array.from(state.get().values(), (record) => freeze(record.provider, true))),
      ),
      available: Effect.fn("Provider.available")(() => snapshot().pipe(Effect.map((value) => value.providers))),
      snapshot,
    })
  }),
)

export const node = makeLocationNode({ service: Service, layer, deps: [Bus.node, Integration.node, Location.node] })

const importPackage = Effect.fn("Provider.importPackage")(function* (
  specifier: string,
  entrypoint: string,
  load = () => importModule(entrypoint),
) {
  const module = yield* Effect.tryPromise({
    try: () => {
      const existing = packages.get(entrypoint)
      if (existing) return existing
      const loaded = load()
      packages.set(entrypoint, loaded)
      return loaded
    },
    catch: (cause) => new LoadError({ package: specifier, cause }),
  })
  if (typeof module !== "object" || module === null || typeof (module as { model?: unknown }).model !== "function") {
    return yield* new LoadError({
      package: specifier,
      cause: new Error(`Provider package ${specifier} does not export model(modelID, settings)`),
    })
  }
  return module as ProviderPackage.Definition
})
