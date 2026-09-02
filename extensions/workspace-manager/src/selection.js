/** Tiny host-event bridge kept outside React so connection can precede render. */
export function selections() {
  const listeners = new Set();
  return {
    publish(value) { for (const listener of listeners) listener(value); },
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
  };
}
