import { ensurePluginRuntime, provides } from "@devcode/plugin/runtime"
import { Plugin, PluginContextProvider, usePlugin } from "@devcode/plugin/tui"
import { ensureRuntimePluginSupport } from "@opentui/solid/runtime-plugin-support/configure"

ensurePluginRuntime()
ensureRuntimePluginSupport({
  additional: {
    "@devcode/plugin/tui": { Plugin, PluginContextProvider, usePlugin },
  },
  preserve: provides,
})
