import { expect, test } from "bun:test"
import { selectTheme, selectThemeMode, supportsThemeMode, themeModes } from "@devcode/theme/tui"
import { getDevCodeTheme } from "../../../src/theme"

test("selects complete light and dark themes independently", () => {
  const light = selectTheme(getDevCodeTheme(), "light")
  const dark = selectTheme(getDevCodeTheme(), "dark")
  expect(selectTheme(getDevCodeTheme())).toEqual(light)
  expect(light.hue).toEqual(getDevCodeTheme().light.hue)
  expect(dark.text).toEqual(getDevCodeTheme().base.text)
  expect(dark.hue).toEqual(getDevCodeTheme().dark.hue)
  expect(selectThemeMode(getDevCodeTheme(), "dark")).toEqual({ theme: dark, mode: "dark" })
})

test("selects the available mode when the requested mode is missing", () => {
  const lightOnly = { base: getDevCodeTheme().base, light: getDevCodeTheme().light } as const
  const darkOnly = { base: getDevCodeTheme().base, dark: getDevCodeTheme().dark } as const

  expect(themeModes(lightOnly)).toEqual(["light"])
  expect(themeModes(darkOnly)).toEqual(["dark"])
  expect(supportsThemeMode(lightOnly, "light")).toBeTrue()
  expect(supportsThemeMode(lightOnly, "dark")).toBeFalse()
  expect(selectThemeMode(lightOnly, "dark")).toEqual({ theme: selectTheme(getDevCodeTheme(), "light"), mode: "light" })
  expect(selectThemeMode(darkOnly, "light")).toEqual({ theme: selectTheme(getDevCodeTheme(), "dark"), mode: "dark" })
})
