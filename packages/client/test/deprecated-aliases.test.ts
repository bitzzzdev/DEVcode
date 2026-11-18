import { expect, test } from "bun:test"
import { compile } from "@devcode/httpapi-codegen"
import {
  ClientApi,
  clientNamespace,
  deprecatedClientPaths,
  effectOmitEndpoints,
  groupNames,
  promiseOmitEndpoints,
} from "@devcode/protocol/client"
import { Effect } from "effect"
import { HttpClient } from "effect/http"
import { DevCode as EffectDevCode } from "../src/effect/index"
import { DevCode as PromiseDevCode } from "../src/promise/index"

// The pre-experimental paths stay until the next minor.
const aliases = (omitEndpoints: ReadonlySet<string>) =>
  compile(ClientApi, { groupNames, omitEndpoints, namespace: clientNamespace, aliases: deprecatedClientPaths }).aliases ?? []

const at = (root: unknown, path: ReadonlyArray<string>) =>
  path.reduce<unknown>((parent, key) => (parent as Record<string, unknown> | undefined)?.[key], root)

const expectSameFunctions = (client: unknown, list: ReturnType<typeof aliases>) => {
  expect(list.length).toBeGreaterThan(20)
  for (const alias of list) {
    expect(alias.to[0]).toBe("experimental")
    const target = at(client, alias.to)
    expect(target).toBeFunction()
    expect(at(client, alias.from)).toBe(target)
  }
}

test("every Promise client alias is the same function as its client.experimental path", () => {
  expectSameFunctions(PromiseDevCode.make({ baseUrl: "http://localhost:3000" }), aliases(promiseOmitEndpoints))
})

test("every Effect client alias is the same function as its client.experimental path", async () => {
  const client = await EffectDevCode.make({ baseUrl: "http://localhost:3000" }).pipe(
    Effect.provideService(
      HttpClient.HttpClient,
      HttpClient.make(() => Effect.die("unused")),
    ),
    Effect.runPromise,
  )
  expectSameFunctions(client, aliases(effectOmitEndpoints))
})
