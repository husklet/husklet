import type { HostEvent } from '@husklet/client';

/** Tiny host-event bridge kept outside React so connection can precede render. */
export function selections(): {
  publish(value: HostEvent): void;
  subscribe(listener: (value: HostEvent) => void): () => void;
} {
  const listeners = new Set<(value: HostEvent) => void>();
  return {
    publish(value: HostEvent) { for (const listener of listeners) listener(value); },
    subscribe(listener: (value: HostEvent) => void) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}
