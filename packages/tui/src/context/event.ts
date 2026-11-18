import type { DevCodeEvent } from "@devcode/client"
import { useClient } from "./client"

type EventMetadata = {
  directory: string | undefined
}
type DevCodeEventMap = { [Type in DevCodeEvent["type"]]: Extract<DevCodeEvent, { type: Type }> }

export function useEvent() {
  const client = useClient()

  function subscribe(handler: (event: DevCodeEvent, metadata: EventMetadata) => void) {
    return client.event.listen(({ details }) => {
      if (details.type === "server.connected") return
      handler(details, { directory: details.location?.directory })
    })
  }

  function on<T extends DevCodeEvent["type"]>(
    type: T,
    handler: (event: DevCodeEventMap[T], metadata: EventMetadata) => void,
  ) {
    return client.event.on(type, (event) => {
      handler(event, { directory: event.location?.directory })
    })
  }

  return {
    subscribe,
    on,
  }
}
