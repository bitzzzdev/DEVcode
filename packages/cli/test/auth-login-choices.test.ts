import { expect, test } from "bun:test"
import type { IntegrationInfo } from "@devcode/client"
import { loginChoices } from "../src/commands/handlers/auth/login"

const integration = (value: Partial<IntegrationInfo> & Pick<IntegrationInfo, "id" | "name">): IntegrationInfo => ({
  methods: [{ type: "key" }],
  connections: [],
  ...value,
})

test("groups the CLI choices like /connect while keeping stable login IDs", () => {
  expect(
    loginChoices([
      integration({ id: "mistral", name: "Mistral" }),
      integration({ id: "openai", name: "OpenAI" }),
      integration({ id: "linear", name: "Linear", metadata: { source: "mcp" } }),
      integration({ id: "github", name: "GitHub", metadata: { source: "mcp" } }),
      integration({ id: "devcode", name: "DevCode Console" }),
      integration({ id: "devcode-go", name: "DevCode Go", connections: [{ type: "env", name: "GO_KEY" }] }),
      integration({ id: "unused", name: "Unused", methods: [{ type: "env", names: ["UNUSED_KEY"] }] }),
    ]),
  ).toEqual([
    { value: "github", label: "GitHub", category: "MCP", connected: false },
    { value: "linear", label: "Linear", category: "MCP", connected: false },
    { value: "devcode-go", label: "DevCode Go", category: "Popular", connected: true },
    { value: "devcode", label: "DevCode Console", category: "Popular", connected: false },
    { value: "openai", label: "OpenAI", category: "Popular", connected: false },
    { value: "mistral", label: "Mistral", category: "Services", connected: false },
  ])
})
