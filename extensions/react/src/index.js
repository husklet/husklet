// The public API: connect to the host, render React into its tab.

import { ExtensionError, Session, SOCKET, PROTOCOL } from './session.js';
import { Surface, reconciler } from './reconciler.js';
import { PROPS, TRIGGERS } from './protocol.js';

export { ExtensionError, Session, SOCKET, PROTOCOL };
export * from './components.js';

/** Surfaces awaiting events, per session. */
const attached = new WeakMap();
/** Reference-counted host subscriptions, keyed by session and snapshot topic. */
const subscriptions = new WeakMap();
const SNAPSHOT_TOPICS = Object.freeze(['containers', 'images', 'volumes', 'networks', 'terminal', 'pane-changes', 'workspace-events']);

/**
 * Connects to the workspace this extension runs in.
 *
 * The socket path comes from `HUSKLET_EXTENSION_SOCKET`, which the host mounts
 * into the container; an extension is never asked to know where it is.
 */
export async function connect({ path, onRows, onReply, onEvent, onEventError, pendingLimit, timeout } = {}) {
  let session;
  session = await Session.connect(path, {
    onRows,
    pendingLimit,
    timeout,
    onEventError,
    onEvent: (payload, channel) => {
      deliver(session, payload);
      if (onEvent) onEvent(payload, channel);
    },
    onReply: (payload) => {
      if (!deliver(session, payload) && onReply) onReply(payload);
    },
  });
  attached.set(session, new Set());
  subscriptions.set(session, new Map());
  return session;
}

/** A typed, ergonomic view over the protocol's host calls and snapshots. */
export function workspace(session) {
  const expect = (reply, kind) => {
    if (reply?.reply !== kind) throw new Error(`host replied ${reply?.reply ?? 'without a tag'}, expected ${kind}`);
    return reply.with;
  };
  const done = async (name, argument) => expect(await session.call(name, argument), 'done');
  const subscription = (call, topic) => {
    if (!SNAPSHOT_TOPICS.includes(topic)) throw new RangeError(`host does not publish the ${topic} snapshot topic`);
    return done(call, { topic });
  };
  const states = subscriptions.get(session) ?? new Map();
  subscriptions.set(session, states);
  const subscribe = async (topic) => {
    if (!SNAPSHOT_TOPICS.includes(topic)) throw new RangeError(`host does not publish the ${topic} snapshot topic`);
    let state = states.get(topic);
    if (!state) {
      state = { references: 0, active: false, operation: Promise.resolve() };
      states.set(topic, state);
    }
    state.references += 1;
    const operation = state.operation.then(async () => {
      if (!state.active) {
        await subscription('event_subscribe', topic);
        state.active = true;
      }
    });
    state.operation = operation.catch(() => {});
    try {
      await operation;
    } catch (error) {
      state.references -= 1;
      if (state.references === 0 && !state.active) states.delete(topic);
      throw error;
    }
  };
  const unsubscribe = async (topic) => {
    if (!SNAPSHOT_TOPICS.includes(topic)) throw new RangeError(`host does not publish the ${topic} snapshot topic`);
    const state = states.get(topic);
    if (!state || state.references === 0) return;
    state.references -= 1;
    const operation = state.operation.then(async () => {
      if (state.references === 0 && state.active) {
        await subscription('event_unsubscribe', topic);
        state.active = false;
      }
      if (state.references === 0 && !state.active) states.delete(topic);
    });
    state.operation = operation.catch(() => {});
    await operation;
  };
  const api = {
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
      processes: async (id) => expect(await session.call('container_processes', { id }), 'processes'),
      logs: async (id, { stdout = true, stderr = true } = {}) => expect(
        await session.call('container_logs', { id, stdout, stderr }), 'logs',
      ),
      execution: async (id) => expect(await session.call('execution_inspect', { id }), 'execution'),
      signalExecution: (id, signal) => done('execution_kill', { id, signal }),
      create: async (image, name) => expect(await session.call('container_create', { image, name }), 'identity'),
      start: (id) => done('container_start', { id }),
      stop: (id) => done('container_stop', { id }),
      remove: (id) => done('container_remove', { id }),
      pause: (id) => done('container_pause', { id }),
      unpause: (id) => done('container_unpause', { id }),
      restart: (id) => done('container_restart', { id }),
      kill: (id, signal) => done('container_kill', { id, signal }),
      exec: async (id, { command, user, workingDirectory } = {}) => expect(
        await session.call('container_exec', {
          id, command, user: user ?? null, working_directory: workingDirectory ?? null,
        }), 'identity',
      ),
    },
    images: {
      list: async () => expect(await session.call('image_list'), 'images'),
      pull: async (reference) => expect(await session.call('image_pull', { reference }), 'image'),
      inspect: async (reference) => expect(await session.call('image_inspect', { reference }), 'image_details'),
      remove: (reference) => done('image_remove', { reference }),
      prune: async () => expect(await session.call('image_prune'), 'image_prune'),
    },
    volumes: {
      list: async () => expect(await session.call('volume_list'), 'volumes'),
      inspect: async (name) => expect(await session.call('volume_inspect', { name }), 'volume'),
      create: async (name) => expect(await session.call('volume_create', { name }), 'volume'),
      remove: (name) => done('volume_remove', { name }),
    },
    networks: {
      list: async () => expect(await session.call('network_list'), 'networks'),
      inspect: async (reference) => expect(await session.call('network_inspect', { reference }), 'network'),
      create: async (name) => expect(await session.call('network_create', { name }), 'identity'),
      remove: (reference) => done('network_remove', { reference }),
      connect: (reference, container) => done('network_connect', { reference, container }),
      disconnect: (reference, container) => done('network_disconnect', { reference, container }),
    },
    terminal: {
      tabs: async () => expect(await session.call('terminal_tabs'), 'tabs'),
      topology: async () => expect(await session.call('terminal_topology'), 'topology'),
      openTab: async (title) => expect(await session.call('terminal_open_tab', { title }), 'identity'),
      split: async (slot, division) => expect(await session.call('terminal_split', { slot, division }), 'identity'),
      spawn: (slot, command) => done('terminal_spawn', { slot, command }),
      read: async (slot, lines) => expect(await session.call('terminal_read_pane', { slot, lines }), 'text'),
      semantics: async (slot) => expect(await session.call('pane_semantic_read', { slot }), 'semantics'),
      act: (slot, action) => {
        if (action?.value != null && new TextEncoder().encode(action.value).byteLength > 4096) {
          throw new RangeError('pane semantic action value exceeds 4096 bytes');
        }
        return done('pane_semantic_action', { slot, action });
      },
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
      mkdir: (path) => done('filesystem_mkdir', { path }),
      rename: (from, to) => done('filesystem_rename', { from, to }),
      remove: (path) => done('filesystem_remove', { path }),
    },
    subscribe,
    unsubscribe,
  };
  api.watchPaneChanges = async (listener) => {
    if (typeof listener !== 'function') throw new TypeError('pane change listener must be a function');
    const off = session.onEvent((event) => {
      if (event?.snapshot === 'pane_changes') listener(event.of);
    });
    try { await api.subscribe('pane-changes'); } catch (error) { off(); throw error; }
    return async () => { off(); await api.unsubscribe('pane-changes'); };
  };
  return api;
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
    return { ...inner, trigger, value: inner.value ?? null };
  }
  if (typeof body !== 'object' || typeof body.id !== 'string') return null;
  const legacy = typeof body.interaction === 'string'
    ? `${body.interaction[0].toUpperCase()}${body.interaction.slice(1)}`
    : undefined;
  const trigger = body.trigger ?? named ?? legacy;
  if (trigger === undefined) return null;
  return { ...body, trigger, value: body.value ?? null };
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
    containers: ['list', 'inspect', 'processes', 'logs', 'execution', 'signalExecution', 'create', 'start', 'stop', 'remove', 'pause', 'unpause', 'restart', 'kill', 'exec'],
    images: ['list', 'pull'],
    volumes: ['list', 'inspect', 'create', 'remove'],
    networks: ['list', 'inspect', 'create', 'remove', 'connect', 'disconnect'],
    terminal: ['tabs', 'topology', 'openTab', 'split', 'spawn', 'read', 'semantics', 'act', 'writeInput', 'resizeGrid', 'close', 'focus', 'ratio'],
    files: ['list', 'read', 'write', 'mkdir', 'rename', 'remove'],
    interfaceEvents: ['invoke', 'submit', 'change', 'select', 'scroll', 'close', 'context', 'key', 'focus', 'pointer'],
    workspaceEvents: ['key', 'focus', 'pointer'],
    snapshotTopics: SNAPSHOT_TOPICS,
  }),
  unavailable: Object.freeze({
    workspace: ['renameWhileUpdating', 'mutateWhileRunning', 'controlHostingWorkspace'],
    containers: [],
    terminal: ['switchOccupant'],
    events: ['extensions', 'drag', 'drop'],
  }),
});
