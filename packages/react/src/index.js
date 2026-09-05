// The public API: connect to the host, render React into its tab.

import { Session } from '@husklet/client';
import { Surface, reconciler } from './reconciler.js';
import { PROPS, TRIGGERS } from './protocol.js';
export { TABLE_COLUMN_LIMIT, COLUMN_KEY_BYTE_LIMIT, COLUMN_TITLE_BYTE_LIMIT } from './protocol.js';

// Match the declaration surface below: React extensions get the complete
// framework-neutral SDK, while this module's explicit `connect` export remains
// the React-aware override.
export * from '@husklet/client';
export * from './components.js';
export * from './hooks.js';
export * from './terminal-transcript.js';
export * from './command-palette.js';
export * from './json-tree.js';
export * from './confirm-action.js';
export * from './resource-state.js';

const attached = new WeakMap();
const SURFACE_LIMIT = 32;
const FRAME_BUFFER_LIMIT = 64;

export async function connect({ path, onRows, onReply, onEvent, onEventError, onClose, pendingLimit, timeout, connectTimeout } = {}) {
  let session;
  session = await Session.connect(path, {
    onRows, pendingLimit, timeout, connectTimeout, onEventError, onClose,
    onEvent: (payload, channel) => {
      deliver(session, payload);
      if (onEvent) onEvent(payload, channel);
    },
    onReply: (payload) => {
      if (!deliver(session, payload) && onReply) onReply(payload);
    },
  });
  attached.set(session, { handles: new Set(), slots: new Map(), routesEvents: true });
  return session;
}

/**
 * Renders an element tree into the extension's tab.
 *
 * The tab is opened first because the host refuses a render before one exists.
 * Returns a handle whose `update` re-renders and whose `close` tears down.
 */
export function render(element, session, { title = 'Extension', split = null, bootstrap = null } = {}) {
  let registry = attached.get(session);
  if (!registry && bootstrap !== null) {
    registry = { handles: new Set(), slots: new Map(), routesEvents: false };
    attached.set(session, registry);
  }
  if (!registry) throw new Error('render requires a session returned by connect');
  if (!registry.routesEvents) {
    session.onEvent((payload) => deliver(session, payload));
    registry.routesEvents = true;
  }
  if (split !== null && (
    typeof split !== 'object'
    || typeof split.slot !== 'string'
    || !['beside', 'below'].includes(split.division)
  )) {
    throw new TypeError('split requires a slot and a beside or below division');
  }
  if (registry.handles.size >= SURFACE_LIMIT) {
    throw new RangeError(`extension surface limit of ${SURFACE_LIMIT} is exhausted`);
  }
  const queued = [];
  let slot = bootstrap?.slot ?? null;
  let closed = false;
  let failed = null;
  let withdrawal = null;
  const deliveries = new Set();
  const transmit = (frame) => {
    if (closed || failed) return;
    if (slot === null) {
      if (queued.length >= FRAME_BUFFER_LIMIT) {
        failed = new Error(`surface frame buffer limit of ${FRAME_BUFFER_LIMIT} is exhausted`);
        return;
      }
      queued.push(frame);
      return;
    }
    const delivery = (async () => {
      const reply = await session.call('interface_render_at', { slot, frame });
      if (reply?.reply !== 'done') {
        throw new Error(`host replied ${reply?.reply ?? 'without a tag'}, expected done`);
      }
    })();
    deliveries.add(delivery);
    delivery.catch((error) => { failed = error; }).finally(() => deliveries.delete(delivery));
  };
  if (bootstrap !== null && (
    typeof bootstrap !== 'object'
    || typeof bootstrap.slot !== 'string'
    || bootstrap.sequence !== 1
    || bootstrap.nextNode !== 2
    || bootstrap.bootstrapNode !== 1
  )) throw new TypeError('bootstrap must be a token returned by bootstrapSurface');
  const surface = new Surface(transmit, bootstrap === null ? undefined : {
    sequence: bootstrap.sequence,
    next: bootstrap.nextNode,
    patches: [{ Remove: { id: bootstrap.bootstrapNode } }],
  });
  const handle = { surface };
  registry.handles.add(handle);

  const opening = bootstrap !== null
    ? Promise.resolve({ reply: 'identity', with: bootstrap.slot })
    : split === null
      ? session.call('interface_open_tab', { title })
      : session.call('interface_split', { slot: split.slot, division: split.division });
  const ready = opening.then((reply) => {
    if (reply?.reply !== 'identity' || typeof reply.with !== 'string' || (bootstrap === null && reply.with.length === 0)) {
      throw new Error(`host replied ${reply?.reply ?? 'without a tag'}, expected identity`);
    }
    if (closed) return reply.with;
    if (registry.slots.has(reply.with)) throw new Error(`host reused live surface slot ${reply.with}`);
    slot = reply.with;
    registry.slots.set(slot, handle);
    for (const frame of queued.splice(0)) transmit(frame);
    if (failed) throw failed;
    return slot;
  }).catch((error) => {
    failed = error;
    registry.handles.delete(handle);
    if (slot !== null) registry.slots.delete(slot);
    throw error;
  });
  // Existing fire-and-forget callers still get bounded cleanup on a refused
  // open; callers that need diagnostics await the same promise on the handle.
  void ready.catch(() => {});

  const container = reconciler.createContainer(surface, 0, null, false, null, '', () => {}, null);
  reconciler.updateContainer(element, container, null, null);
  Object.assign(handle, {
    ready,
    get slot() { return slot; },
    update(next) {
      reconciler.updateContainer(next, container, null, null);
    },
    async flush() {
      await ready;
      while (deliveries.size > 0) await Promise.all(deliveries);
      if (failed) throw failed;
    },
    async source(mutation) {
      const owned = await ready;
      const reply = await session.call('source_resize_at', { slot: owned, mutation });
      if (reply?.reply !== 'done') throw new Error(`host replied ${reply?.reply ?? 'without a tag'}, expected done`);
    },
    close() {
      if (closed) return withdrawal ?? Promise.resolve();
      closed = true;
      reconciler.updateContainer(null, container, null, null);
      registry.handles.delete(handle);
      if (slot !== null) registry.slots.delete(slot);
      withdrawal = ready.then(async (owned) => {
        if (owned === '') return;
        const reply = await session.call('interface_withdraw', { slot: owned });
        if (reply?.reply !== 'done') {
          throw new Error(`host replied ${reply?.reply ?? 'without a tag'}, expected done`);
        }
      });
      // Fire-and-forget callers retain the old ergonomics without creating an
      // unhandled rejection; callers that care await the returned promise.
      void withdrawal.catch(() => {});
      return withdrawal;
    },
  });
  return handle;
}

/**
 * Routes one host payload to the callback that asked for it.
 *
 * Returns whether it was an interface event at all, so anything else can go on
 * to the caller's own reply handler.
 */
export function deliver(session, payload) {
  const event = interpret(payload);
  if (event === null) return false;
  const registry = attached.get(session);
  if (!registry) return false;
  const slot = typeof payload?.slot === 'string' ? payload.slot : null;
  if (slot !== null) {
    return registry.slots.get(slot)?.surface.dispatch(event) ?? false;
  }
  if (registry.handles.size !== 1) return false;
  let delivered = false;
  for (const handle of registry.handles) {
    delivered = handle.surface.dispatch(event) || delivered;
  }
  return delivered;
}

/**
 * Reads an interface event out of whatever the host pushed.
 *
 * TODO: narrow to the single spelling once the host side of `hl_gui::Event`
 * gains its wire derive; today only the identity and the trigger are certain.
 */
function interpret(payload) {
  if (!payload || typeof payload !== 'object') return null;
  if (typeof payload.interaction !== 'string' || typeof payload.trigger !== 'string'
      || typeof payload.id !== 'string') return null;
  const wire = payload.value;
  const value = wire && typeof wire === 'object'
    ? (wire.Text ?? wire.Number ?? wire.Integer ?? wire.Flag ?? wire)
    : (wire ?? null);
  return { ...payload, value };
}

/** Every prop and handler name a component accepts, for tooling and tests. */
export const vocabulary = {
  props: [...PROPS.keys()],
  handlers: [...TRIGGERS.keys()],
};

/** Maximum Unicode characters retained by a LogView; Value patches append. */
export const LOG_VIEW_CHARACTER_LIMIT = 4_096;
