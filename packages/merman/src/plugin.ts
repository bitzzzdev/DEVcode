import { Plugin } from "@devcode/plugin/tui"
import { createMermaidCodeBlockRenderer } from "./markdown.js"
import { resolveDevCodeDiagramPalette } from "./palette.js"

export default Plugin.define({
  id: "devcode.merman",
  setup(context) {
    context.markdown.registerCodeBlockRenderer(
      "mermaid",
      createMermaidCodeBlockRenderer(context.renderer, () => ({
        colors: resolveDevCodeDiagramPalette(context.theme, context.themeMode),
      })),
    )
  },
})
