import { Effect, Option } from "effect"
import { brotliDecompressSync } from "node:zlib"
import { AppArchive } from "./app-archive"

export type AssetMap = Readonly<Record<string, string | Uint8Array>>
/** Each asset's brotli-compressed bytes, when the build embedded them. */
export type BrotliMap = Readonly<Record<string, Uint8Array>>

export const load = Effect.fn("cli.app-assets.load")(function* () {
  const embedded = yield* Effect.tryPromise(() => import("virtual:devcode-app-assets")).pipe(
    Effect.map((module) => AppArchive.decode(module.default())),
    Effect.option,
  )
  if (Option.isSome(embedded) && Object.keys(embedded.value).length > 0)
    return {
      files: lazy(embedded.value, (key) => brotliDecompressSync(embedded.value[key]!)),
      brotli: embedded.value,
    }
  // This fork ships no web UI; the handler serves /api and returns 404 for browser requests.
  return { files: {}, brotli: undefined }
})

function lazy(assets: Readonly<Record<string, unknown>>, load: (key: string) => Uint8Array): AssetMap {
  // Immutable browser caching makes retaining decompressed copies in the server unnecessary.
  return new Proxy(
    {},
    {
      get: (_, key) => {
        if (typeof key !== "string" || assets[key] === undefined) return
        const body = load(key)
        return isText(key) ? Buffer.from(body).toString() : body
      },
    },
  )
}

function isText(file: string) {
  return file === "_headers" || /\.(?:css|html|js|json|svg|txt|webmanifest|xml)$/.test(file)
}
