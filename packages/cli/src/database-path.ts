import path from "node:path"
import { DEVCODE_CHANNEL } from "./version"

export function databasePath(data: string) {
  const filename =
    process.env.DEVCODE_DB ??
    (["latest", "dev", "beta", "next", "prod"].includes(DEVCODE_CHANNEL) ||
    process.env.DEVCODE_DISABLE_CHANNEL_DB === "1" ||
    process.env.DEVCODE_DISABLE_CHANNEL_DB === "true"
      ? "devcode.db"
      : `devcode-${DEVCODE_CHANNEL.replace(/[^a-zA-Z0-9._-]/g, "-")}.db`)
  return filename === ":memory:" ? filename : path.resolve(data, filename)
}
