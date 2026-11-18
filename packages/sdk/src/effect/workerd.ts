export * as DevCodeWorkerd from "./workerd"

import { Layer } from "effect"
import type { Config, Scope } from "effect"
import { WorkerdProfile } from "../internal/workerd"
import { DevCode } from "./devcode"

export type Configuration = WorkerdProfile.Configuration

export interface CreateOptions<R = never> extends WorkerdProfile.Options {
  readonly log?: DevCode.CreateOptions["log"]
  readonly plugins?: DevCode.CreateOptions["plugins"]
  readonly workspaceProviders?: DevCode.CreateOptions["workspaceProviders"]
  readonly instances?: DevCode.CreateOptions<R>["instances"]
}

export const create = <R = never>({ log, plugins, workspaceProviders, instances, ...options }: CreateOptions<R>) => {
  const profile = WorkerdProfile.make(options)
  return DevCode.create(
    { ...profile.options, log, plugins, workspaceProviders, instances },
    { overrides: profile.replacements },
  )
}

export const layer = <R = never>(
  options: CreateOptions<R>,
): Layer.Layer<DevCode.Service, Config.ConfigError | Error, Exclude<R, Scope.Scope>> =>
  Layer.effect(DevCode.Service, create(options))

export type Interface = DevCode.Interface
export type Requirements = Scope.Scope
