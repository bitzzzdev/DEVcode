import { expect, test } from "bun:test"
import { Env } from "../src/env"

test("session environment omits server credentials", () => {
  const previousPassword = process.env.DEVCODE_PASSWORD
  const previousLegacyPassword = process.env.DEVCODE_SERVER_PASSWORD
  const previousValue = process.env.DEVCODE_SESSION_ENV_TEST
  process.env.DEVCODE_PASSWORD = "password"
  process.env.DEVCODE_SERVER_PASSWORD = "legacy"
  process.env.DEVCODE_SESSION_ENV_TEST = "included"

  const environment = Env.session()

  if (previousPassword === undefined) delete process.env.DEVCODE_PASSWORD
  else process.env.DEVCODE_PASSWORD = previousPassword
  if (previousLegacyPassword === undefined) delete process.env.DEVCODE_SERVER_PASSWORD
  else process.env.DEVCODE_SERVER_PASSWORD = previousLegacyPassword
  if (previousValue === undefined) delete process.env.DEVCODE_SESSION_ENV_TEST
  else process.env.DEVCODE_SESSION_ENV_TEST = previousValue

  expect(environment.DEVCODE_PASSWORD).toBeUndefined()
  expect(environment.DEVCODE_SERVER_PASSWORD).toBeUndefined()
  expect(environment.DEVCODE_SESSION_ENV_TEST).toBe("included")
})
