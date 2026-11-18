import { Agent } from "@devcode/core/agent"
import { App } from "@devcode/core/app"
import { Artifact } from "@devcode/core/artifact"
import { AISDK } from "@devcode/core/aisdk"
import { Command } from "@devcode/core/command"
import { Config } from "@devcode/core/config"
import { Credential } from "@devcode/core/credential"
import { Database } from "@devcode/core/database/database"
import { Environment } from "@devcode/core/environment/index"
import { FileAccess } from "@devcode/core/file-access"
import { Flag } from "@devcode/core/flag"
import { LayerNodePlatform } from "@devcode/util/effect/app-node-platform"
import { AppProcess } from "@devcode/util/process"
import { LayerNode } from "@devcode/util/effect/layer-node"
import { Bus } from "@devcode/core/bus"
import { FileSystem } from "@devcode/core/filesystem"
import { FSUtil } from "@devcode/util/fs-util"
import { Global } from "@devcode/util/global"
import { Form } from "@devcode/core/form"
import { Generate } from "@devcode/core/generate"
import { InstructionDiscovery } from "@devcode/core/instruction-discovery"
import { Integration } from "@devcode/core/integration"
import { Job } from "@devcode/core/job"
import { KV } from "@devcode/core/kv"
import { Location } from "@devcode/core/location"
import { Mcp } from "@devcode/core/mcp/index"
import { Model } from "@devcode/core/model"
import { Npm } from "@devcode/util/npm"
import { Plugin } from "@devcode/core/plugin"
import { PluginHooks } from "@devcode/core/plugin/hooks"
import { Provider } from "@devcode/core/provider"
import { Session } from "@devcode/core/session"
import { SharedCache } from "@devcode/core/shared-cache"
import { InstructionEntry } from "@devcode/core/session/instruction-entry"
import { PersistentPty } from "@devcode/core/persistent-pty"
import { LocationServiceMap } from "@devcode/core/location-service-map"
import { AppNodeBuilder } from "@devcode/core/effect/app-node-builder"
import { Permission } from "@devcode/core/permission"
import { Reference } from "@devcode/core/reference"
import { Rpc } from "@devcode/core/rpc"
import { Skill } from "@devcode/core/skill"
import { SkillDiscovery } from "@devcode/core/skill/discovery"
import { Watcher } from "@devcode/core/filesystem/watcher"
import { Tool } from "@devcode/core/tool"
import { Vcs } from "@devcode/core/vcs"
import { WebSearch } from "@devcode/core/websearch"
import { Worktree } from "@devcode/core/worktree"
import { WorktreeStrategies } from "@devcode/core/worktree/strategies"
import { Effect, Layer } from "effect"
import { tempLocationLayer } from "../fixture/location"
import { emptyMcpLayer } from "../fixture/mcp"
import { offlineModels } from "../fixture/models"

const npmLayer = Layer.succeed(
  Npm.Service,
  Npm.Service.of({
    add: (name) => Effect.succeed({ directory: "", name }),
    resolve: (name) => Effect.succeed({ directory: "", name }),
    check: () => Effect.succeed(false),
    update: (name) => Effect.succeed({ directory: "", name }),
    which: () => Effect.undefined,
  }),
)

const generateLayer = Layer.succeed(
  Generate.Service,
  Generate.Service.of({
    text: () => Effect.succeed(""),
    image: () => Effect.die("unused"),
    decisions: () => Effect.die("unused"),
    video: {
      start: () => Effect.die("unused"),
      status: () => Effect.die("unused"),
      result: () => Effect.die("unused"),
    },
  }),
)

const configLayer = Config.testLayer()

const permissionLayer = Layer.succeed(
  Permission.Service,
  Permission.Service.of({
    close: Effect.void,
    ask: (input) => Effect.succeed({ id: input.id ?? Permission.ID.create(), effect: "ask" }),
    assert: () => Effect.void,
    assertNotDenied: () => Effect.void,
    allows: () => Effect.succeed(false),
    reply: () => Effect.void,
    get: () => Effect.succeed(undefined),
    forSession: () => Effect.succeed([]),
    list: () => Effect.succeed([]),
    nested: () => Effect.void,
  }),
)

export const makePluginTestLayer = (permission = permissionLayer, generate = generateLayer) =>
  AppNodeBuilder.build(
    LayerNode.group([
      App.node,
      AppProcess.node,
      FileSystem.node,
      FSUtil.node,
      Location.node,
      Npm.node,
      Credential.node,
      Config.node,
      Flag.node,
      Database.node,
      Environment.node,
      FileAccess.node,
      Global.node,
      Bus.node,
      Form.node,
      Generate.node,
      LayerNodePlatform.httpClient,
      Plugin.node,
      Agent.node,
      Artifact.node,
      AISDK.node,
      Provider.node,
      Model.node,
      Command.node,
      InstructionDiscovery.node,
      Integration.node,
      Job.node,
      KV.node,
      Mcp.node,
      Session.node,
      SharedCache.node,
      InstructionEntry.node,
      PersistentPty.node,
      LocationServiceMap.node,
      Permission.node,
      PluginHooks.node,
      Reference.node,
      Rpc.node,
      Skill.node,
      SkillDiscovery.node,
      Tool.node,
      Vcs.node,
      Watcher.node,
      WebSearch.node,
      Worktree.node,
      WorktreeStrategies.node,
    ]),
    [
      Location.node.replace(tempLocationLayer),
      Npm.node.replace(npmLayer),
      Config.node.replace(configLayer),
      Mcp.node.replace(emptyMcpLayer),
      Generate.node.replace(generate),
      Permission.node.replace(permission),
      offlineModels,
    ],
  ).pipe(Layer.provideMerge(configLayer))

export const PluginTestLayer = makePluginTestLayer()
