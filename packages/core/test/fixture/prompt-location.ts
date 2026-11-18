import type { FileSystem } from "@devcode/core/filesystem"
import { Bus } from "@devcode/core/bus"
import { Image } from "@devcode/core/image"
import { LocationServiceMap } from "@devcode/core/location-service-map"
import type { LocationServices } from "@devcode/core/location-services"
import { Plugin } from "@devcode/core/plugin"
import { PluginHooks } from "@devcode/core/plugin/hooks"
import { Skill } from "@devcode/core/skill"
import type { Location } from "@devcode/schema/location"
import { makeGlobalNode } from "@devcode/util/effect/app-node"
import { LayerNode } from "@devcode/util/effect/layer-node"
import { Effect, Layer, LayerMap } from "effect"

// Plain-prompt unit fixtures use virtual directories.
export const promptLocationNode = makeGlobalNode({
  service: LocationServiceMap.Service,
  layer: Layer.effect(
    LocationServiceMap.Service,
    Effect.gen(function* () {
      const bus = yield* Bus.Service
      return yield* LayerMap.make(
        (_ref: Location.Ref) =>
          LayerNode.compile(LayerNode.group([PluginHooks.node, Image.node, Skill.node, Plugin.node]), {
            replacements: [
              Bus.node.replace(Layer.succeed(Bus.Service, bus)),
              Plugin.node.replace(Layer.mock(Plugin.Service, { awaitActivation: Effect.void })),
            ],
          }) as unknown as Layer.Layer<LocationServices, FileSystem.DirectoryNotFoundError>,
      )
    }),
  ),
  deps: [Bus.node],
})
