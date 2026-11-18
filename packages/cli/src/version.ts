import { Option, Schema } from "effect"

declare const DEVCODE_VERSION: string
declare const DEVCODE_CHANNEL: string
declare const DEVCODE_ARTIFACT: string
declare const DEVCODE_RELEASE_HIGHLIGHTS: string

const version = typeof DEVCODE_VERSION === "string" ? DEVCODE_VERSION : "local"
const channel = typeof DEVCODE_CHANNEL === "string" ? DEVCODE_CHANNEL : "local"
const artifact = typeof DEVCODE_ARTIFACT === "string" ? DEVCODE_ARTIFACT : "cli"
// Stable release builds embed the release notes' Highlights; every other build has none.
const highlights =
  typeof DEVCODE_RELEASE_HIGHLIGHTS === "string"
    ? Option.getOrElse(
        Schema.decodeUnknownOption(
          Schema.fromJsonString(Schema.Array(Schema.Struct({ title: Schema.String, body: Schema.String }))),
        )(DEVCODE_RELEASE_HIGHLIGHTS),
        () => [],
      )
    : []

export {
  version as DEVCODE_VERSION,
  channel as DEVCODE_CHANNEL,
  artifact as DEVCODE_ARTIFACT,
  highlights as DEVCODE_HIGHLIGHTS,
}
export const DEVCODE_LOCAL = channel === "local"
