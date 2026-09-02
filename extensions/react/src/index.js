// The public API: connect to the host, render React into its tab.

import { ExtensionError, Session, SOCKET, PROTOCOL } from './session.js';
import { Surface, reconciler } from './reconciler.js';
import { PROPS, TRIGGERS } from './protocol.js';

export { ExtensionError, Session, SOCKET, PROTOCOL };
export * from './components.js';

/** Surfaces awaiting events, per session. */
const attached = new WeakMap();

/**
 * Connects to the workspace this extension runs in.
 *
 * The socket path comes from `HUSKLET_EXTENSION_SOCKET`, which the host mounts
 * into the container; an extension is never asked to know where it is.
 */
export async function connect({ path, onRows, onReply, onEvent, pendingLimit, timeout } = {}) {
  let session;
  session = await Session.connect(path, {
    onRows,
    pendingLimit,
    timeout,
    onEvent: (payload, channel) => {
      deliver(session, payload);
      if (onEvent) onEvent(payload, channel);
    },
    onReply: (payload) => {
      if (!deliver(session, payload) && onReply) onReply(payload);
    },
  });
  attached.set(session, new Set());
  return session;
}

/** A typed, ergonomic view over the protocol's host calls and snapshots. */
export function workspace(session) {
  const expect = (reply, kind) => {
    if (reply?.reply !== kind) throw new Error(`host replied ${reply?.reply ?? 'without a tag'}, expected ${kind}`);
    return reply.with;
  };
  const done = async (name, argument) => expect(await session.call(name, argument), 'done');
  return {
    info: async () => expect(await session.call('workspace_info'), 'workspace'),
    list: async () => expect(await session.call('workspace_list'), 'workspaces'),
    inspect: async (name) => expect(await session.call('workspace_inspect', { name }), 'workspace_configuration'),
    create: async (configuration) => expect(await session.call('workspace_create', { configuration }), 'workspace_configuration'),
    update: async (name, configuration) => expect(await session.call('workspace_update', { name, configuration }), 'workspace_configuration'),
    delete: (name) => done('workspace_delete', { name }),
    start: (name) => done('workspace_start', { name }),
    stop: (name) => done('workspace_stop', { name }),
    restart: (name) => done('workspace_restart', { name }),
    containers: {
      list: async () => expect(await session.call('container_list'), 'containers'),
      inspect: async (id) => expect(await session.call('container_inspect', { id }), 'container'),
      create: async (image, name) => expect(await session.call('container_create', { image, name }), 'identity'),
      start: (id) => done('container_start', { id }),
      stop: (id) => done('container_stop', { id }),
      remove: (id) => done('container_remove', { id }),
    },
    images: {
      list: async () => expect(await session.call('image_list'), 'images'),
      pull: async (reference) => expect(await session.call('image_pull', { reference }), 'image'),
    },
    terminal: {
      tabs: async () => expect(await session.call('terminal_tabs'), 'tabs'),
      topology: async () => expect(await session.call('terminal_topology'), 'topology'),
      openTab: async (title) => expect(await session.call('terminal_open_tab', { title }), 'identity'),
      split: async (slot, division) => expect(await session.call('terminal_split', { slot, division }), 'identity'),
      spawn: (slot, command) => done('terminal_spawn', { slot, command }),
      read: async (slot, lines) => expect(await session.call('terminal_read_pane', { slot, lines }), 'text'),
      writeInput: (slot, input) => {
        const contents = typeof input === 'string' ? new TextEncoder().encode(input) : Uint8Array.from(input);
        if (contents.byteLength > 64 * 1024) throw new RangeError('terminal input exceeds the 65536 byte limit');
        return done('terminal_write_pane', { slot, contents: [...contents] });
      },
      resizeGrid: (slot, columns, rows) => {
        if (!Number.isInteger(columns) || !Number.isInteger(rows) || columns < 1 || rows < 1 || columns > 1000 || rows > 1000) {
          throw new RangeError('terminal grid rows and columns must be integers within 1..=1000');
        }
        return done('terminal_resize_grid', { slot, columns, rows });
      },
      close: (slot) => done('terminal_close_pane', { slot }),
      focus: (slot) => done('terminal_focus_pane', { slot }),
      ratio: (slot, ratio) => done('terminal_ratio', { slot, ratio }),
    },
    files: {
      list: async (path) => expect(await session.call('filesystem_list', { path }), 'entries'),
      read: async (path) => expect(await session.call('filesystem_read', { path }), 'contents'),
      write: (path, contents) => done('filesystem_write', { path, contents: [...contents] }),
    },
  };
}

/**
 * Renders an element tree into the extension's tab.
 *
 * The tab is opened first because the host refuses a render before one exists.
 * Returns a handle whose `update` re-renders and whose `close` tears down.
 */
export function render(element, session, { title = 'Extension' } = {}) {
  const surface = new Surface((frame) => void session.call('interface_render', { frame }).catch(() => {}));
  void session.call('interface_open_tab', { title }).catch(() => {});
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

/** Honest inventory of the current host contract; gaps are not callable APIs. */
export const protocolCoverage = Object.freeze({
  available: Object.freeze({
    workspace: ['info', 'list', 'inspect', 'create', 'update', 'delete', 'start', 'stop', 'restart'],
    containers: ['list', 'inspect', 'create', 'start', 'stop', 'remove'],
    images: ['list', 'pull'],
    terminal: ['tabs', 'topology', 'openTab', 'split', 'spawn', 'read', 'writeInput', 'resizeGrid', 'close', 'focus', 'ratio'],
    files: ['list', 'read', 'write'],
    interfaceEvents: ['invoke', 'submit', 'change', 'select'],
  }),
  unavailable: Object.freeze({
    workspace: ['renameWhileUpdating', 'mutateWhileRunning', 'controlHostingWorkspace'],
    containers: ['processes', 'exec', 'logs', 'pause', 'unpause', 'restart', 'kill'],
    terminal: ['switchOccupant'],
    events: ['hostSnapshots', 'keyboard', 'focus', 'pointer', 'drag', 'drop'],
  }),
});
