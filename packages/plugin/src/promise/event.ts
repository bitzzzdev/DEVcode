import type { DevCodeEvent } from "@devcode/client"
import type { SelectedEvent, SubscribeOptions } from "@devcode/client/promise/api"

type EventType = Exclude<DevCodeEvent["type"], "server.connected">

export interface EventDomain {
  readonly subscribe: <Type extends EventType = EventType>(
    options?: SubscribeOptions<Type>,
  ) => AsyncIterable<SelectedEvent<DevCodeEvent, Type>>
}
