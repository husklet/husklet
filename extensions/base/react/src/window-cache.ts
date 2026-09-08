/** A small LRU cache for expensive virtualized row windows. */
export interface WindowCache<Value> {
  readonly size: number;
  get(key: string): Value | undefined;
  set(key: string, value: Value): void;
  clear(): void;
}

/**
 * Retains only the most recently used row windows. Keys are application-defined query cursors;
 * values are never serialized or sent to the host.
 */
export function createWindowCache<Value>(maxEntries = 32): WindowCache<Value> {
  if (!Number.isSafeInteger(maxEntries) || maxEntries < 1 || maxEntries > 1_024) {
    throw new RangeError('window cache maxEntries must be between 1 and 1024');
  }
  const entries = new Map<string, Value>();
  return {
    get size() {
      return entries.size;
    },
    get(key) {
      if (typeof key !== 'string') throw new TypeError('window cache key must be a string');
      const value = entries.get(key);
      if (value !== undefined) {
        entries.delete(key);
        entries.set(key, value);
      }
      return value;
    },
    set(key, value) {
      if (typeof key !== 'string') throw new TypeError('window cache key must be a string');
      entries.delete(key);
      entries.set(key, value);
      while (entries.size > maxEntries) entries.delete(entries.keys().next().value!);
    },
    clear() {
      entries.clear();
    },
  };
}
