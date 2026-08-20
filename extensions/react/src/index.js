// The public API: connect to the host, render React into its tab.

import { Session, SOCKET, PROTOCOL } from './session.js';
import { Surface, reconciler } from './reconciler.js';
import { PROPS, TRIGGERS } from './protocol.js';

export { Session, SOCKET, PROTOCOL };
export * from './components.js';

/** Surfaces awaiting events, per session. */
const attached = new WeakMap();

/**
 * Connects to the workspace this extension runs in.
 *
 * The socket path comes from `HUSKLET_EXTENSION_SOCKET`, which the host mounts
 * into the container; an extension is never asked to know where it is.
 */
export async function connect({ path, onRows, onReply } = {}) {
  let session;
  session = await Session.connect(path, {
    onRows,
    onReply: (payload) => {
      if (!deliver(session, payload) && onReply) onReply(payload);
    },
  });
  attached.set(session, new Set());
  return session;
}

/**
 * Renders an element tree into the extension's tab.
 *
 * The tab is opened first because the host refuses a render before one exists.
 * Returns a handle whose `update` re-renders and whose `close` tears down.
 */
export function render(element, session, { title = 'Extension' } = {}) {
  const surface = new Surface((frame) => session.call('interface_render', { frame }));
  session.call('interface_open_tab', { title });
  const surfaces = attached.get(session);
  if (surfaces) surfaces.add(surface);

  const container = reconciler.createContainer(surface, 0, null, false, null, '', () => {}, null);
  reconciler.updateContainer(element, container, null, null);
  return {
    surface,
    update(next) {
      reconciler.updateContainer(next, container, null, null);
    },
    close() {
      reconciler.updateContainer(null, container, null, null);
      if (surfaces) surfaces.delete(surface);
    },
  };
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
  let delivered = false;
  for (const surface of attached.get(session) ?? []) {
    delivered = surface.dispatch(event) || delivered;
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
  const named = typeof payload.event === 'string' ? payload.event : undefined;
  const body = named === undefined ? (payload.event ?? payload) : payload;
  if (typeof body === 'object' && body !== null && !('id' in body)) {
    // Externally tagged: {"Invoke":{"node":2,"id":"2:Invoke"}}
    const [trigger, inner] = Object.entries(body)[0] ?? [];
    if (!inner || typeof inner !== 'object' || typeof inner.id !== 'string') return null;
    return { trigger, node: inner.node, id: inner.id, value: inner.value ?? null };
  }
  if (typeof body !== 'object' || typeof body.id !== 'string') return null;
  const trigger = body.trigger ?? named;
  if (trigger === undefined) return null;
  return { trigger, node: body.node, id: body.id, value: body.value ?? null };
}

/** Every prop and handler name a component accepts, for tooling and tests. */
export const vocabulary = {
  props: [...PROPS.keys()],
  handlers: [...TRIGGERS.keys()],
};
