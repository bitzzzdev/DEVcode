import { describe, expect, test } from "bun:test"
import { Effect } from "effect"
import { Plugin } from "@devcode/core/plugin"
import { PluginHost } from "@devcode/core/plugin/host"
import { ProviderPlugins } from "@devcode/core/plugin/provider"
import { NvidiaPlugin } from "@devcode/core/plugin/provider/nvidia"
import { Provider } from "@devcode/core/provider"
import { testEffect } from "../lib/effect"
import { PluginTestLayer } from "./fixture"

const it = testEffect(PluginTestLayer)

const addPlugin = Effect.fn(function* () {
  const plugin = yield* Plugin.Service
  const host = yield* PluginHost.make(plugin, "test")
  yield* NvidiaPlugin.effect(host)
})

describe("NvidiaPlugin", () => {
  test("is registered so legacy referer headers can be applied", () => {
    expect(ProviderPlugins.map((item) => item.id)).toContain("devcode.provider.nvidia")
  })

  it.effect("applies NVIDIA tracking headers only to nvidia", () =>
    Effect.gen(function* () {
      const catalog = yield* Provider.Service
      yield* catalog.transform((catalog) => {
        catalog.update(Provider.ID.make("nvidia"), (provider) => {
          provider.package = "@devcode/ai/providers/openai-compatible"
          provider.settings = { baseURL: "https://integrate.api.nvidia.com/v1" }
          provider.headers = { Existing: "value" }
        })
        catalog.update(Provider.ID.openrouter, () => {})
      })
      yield* addPlugin()
      expect((yield* catalog.get(Provider.ID.make("nvidia")))?.headers).toEqual({
        Existing: "value",
        "HTTP-Referer": "https://devcode.ai/",
        "X-Title": "devcode",
        "X-BILLING-INVOKE-ORIGIN": "DevCode",
      })
      expect((yield* catalog.get(Provider.ID.openrouter))?.headers).toBeUndefined()
    }),
  )

  it.effect("adds billing origin for custom NVIDIA endpoints", () =>
    Effect.gen(function* () {
      const catalog = yield* Provider.Service
      yield* catalog.transform((catalog) => {
        catalog.update(Provider.ID.make("nvidia"), (provider) => {
          provider.package = "@devcode/ai/providers/openai-compatible"
          provider.settings = { baseURL: "https://integrate.api.nvidia.com/v1" }
        })
      })
      yield* addPlugin()

      expect((yield* catalog.get(Provider.ID.make("nvidia")))?.headers).toEqual({
        "HTTP-Referer": "https://devcode.ai/",
        "X-Title": "devcode",
        "X-BILLING-INVOKE-ORIGIN": "DevCode",
      })
    }),
  )

  it.effect("preserves an explicit NVIDIA billing origin header", () =>
    Effect.gen(function* () {
      const catalog = yield* Provider.Service
      yield* catalog.transform((catalog) => {
        catalog.update(Provider.ID.make("nvidia"), (provider) => {
          provider.package = "@devcode/ai/providers/openai-compatible"
          provider.settings = { baseURL: "https://integrate.api.nvidia.com/v1" }
          provider.headers = { "X-BILLING-INVOKE-ORIGIN": "CustomOrigin" }
        })
      })
      yield* addPlugin()

      expect((yield* catalog.get(Provider.ID.make("nvidia")))?.headers).toEqual({
        "HTTP-Referer": "https://devcode.ai/",
        "X-Title": "devcode",
        "X-BILLING-INVOKE-ORIGIN": "CustomOrigin",
      })
    }),
  )
})
