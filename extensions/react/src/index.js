// The public API: connect to the host, render React into its tab.

import { ExtensionError, Session, SOCKET, PROTOCOL } from './session.js';
import { Surface, reconciler } from './reconciler.js';
import { PROPS, TRIGGERS } from './protocol.js';

export { ExtensionError, Session, SOCKET, PROTOCOL };
export * from './components.js';
export * from './hooks.js';
export * from './terminal-transcript.js';
export * from './command-palette.js';
export * from './json-tree.js';
export * from './confirm-action.js';
export * from './resource-state.js';

/** Surface handles awaiting a slot or registered by their owned slot. */
const attached = new WeakMap();
const SURFACE_LIMIT = 32;
const FRAME_BUFFER_LIMIT = 64;
/** Reference-counted host subscriptions, keyed by session and snapshot topic. */
const subscriptions = new WeakMap();
const SNAPSHOT_TOPICS = Object.freeze(['containers', 'executions', 'images', 'image-pulls', 'volumes', 'networks', 'terminal', 'pane-changes', 'extensions', 'extension-acquisitions', 'workspace-lifecycle', 'workspace-events']);

function immutableIdentity(id, widths, noun) {
  if (typeof id === 'string' && widths.includes(id.length) && /^[0-9a-f]+$/.test(id)) return id;
  throw new TypeError(`${noun} operation requires the complete immutable ID returned by inspection`);
}

function exactContainerName(name) {
  if (typeof name === 'string' && /^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(name)) return name;
  throw new TypeError('container name must contain 1..128 ASCII letters, digits, underscores, periods, or hyphens and start with a letter or digit');
}

function endpointAliases(options) {
  if (options === undefined) return [];
  if (options === null || typeof options !== 'object' || Array.isArray(options)
      || Object.keys(options).some((key) => key !== 'aliases')) {
    throw new TypeError('network connect options may contain only aliases');
  }
  if (options.aliases !== undefined && !Array.isArray(options.aliases)) {
    throw new TypeError('network endpoint aliases must be an array');
  }
  const aliases = options.aliases === undefined ? [] : [...options.aliases];
  if (aliases.length > 64 || new Set(aliases).size !== aliases.length
      || aliases.some((alias) => typeof alias !== 'string' || alias.length < 1 || alias.length > 253
        || !/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(alias))) {
    throw new TypeError('network endpoint aliases must be at most 64 unique, 1..=253-byte ASCII endpoint names');
  }
  return aliases;
}

function exactPaneTitle(title) {
  if (typeof title === 'string' && title.trim().length > 0
    && new TextEncoder().encode(title).byteLength <= 256 && !/[\u0000-\u001f\u007f-\u009f]/u.test(title)) return title;
  throw new TypeError('pane title must be nonblank and contain at most 256 UTF-8 bytes without control characters');
}

function exactCommand(command) {
  if (!Array.isArray(command) || command.length < 1 || command.length > 64
    || command[0] === '' || command.some((argument) => typeof argument !== 'string'
      || new TextEncoder().encode(argument).byteLength > 4096 || argument.includes('\0'))
    || command.reduce((bytes, argument) => bytes + new TextEncoder().encode(argument).byteLength, 0) > 32768) {
    throw new TypeError('command must contain 1..64 NUL-free arguments, each at most 4096 bytes and 32768 bytes in aggregate');
  }
  return command;
}

function immutableDigest(value, noun) {
  if (!/^sha256:[0-9a-f]{64}$/.test(value)) throw new TypeError(`${noun} removal requires the complete immutable sha256 digest returned by inventory`);
  return value;
}

/**
 * Connects to the workspace this extension runs in.
 *
 * The socket path comes from `HUSKLET_EXTENSION_SOCKET`, which the host mounts
 * into the container; an extension is never asked to know where it is.
 */
export async function connect({ path, onRows, onReply, onEvent, onEventError, onClose, pendingLimit, timeout, connectTimeout } = {}) {
  let session;
  session = await Session.connect(path, {
    onRows,
    pendingLimit,
    timeout,
    connectTimeout,
    onEventError,
    onClose,
    onEvent: (payload, channel) => {
      deliver(session, payload);
      if (onEvent) onEvent(payload, channel);
    },
    onReply: (payload) => {
      if (!deliver(session, payload) && onReply) onReply(payload);
    },
  });
  attached.set(session, { handles: new Set(), slots: new Map() });
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
    get granted() { return session.granted; },
    info: async () => expect(await session.call('workspace_info'), 'workspace'),
    list: async () => expect(await session.call('workspace_list'), 'workspaces'),
    inspect: async (name) => expect(await session.call('workspace_inspect', { name }), 'workspace_configuration'),
    create: async (configuration) => expect(await session.call('workspace_create', { configuration }), 'workspace_configuration'),
    adopt: async (configuration) => expect(await session.call('workspace_adopt', { configuration }), 'workspace_configuration'),
    update: async (name, generation, configuration) => expect(await session.call('workspace_update', { name, generation: immutableIdentity(generation, [32], 'workspace generation'), configuration }), 'workspace_configuration'),
    delete: (name, generation) => done('workspace_delete', { name, generation: immutableIdentity(generation, [32], 'workspace generation') }),
    start: (name) => done('workspace_start', { name }),
    stop: (name) => done('workspace_stop', { name }),
    restart: (name) => done('workspace_restart', { name }),
    extensions: {
      list: async () => expect(await session.call('extension_list'), 'extensions'),
      inspect: async (name) => expect(await session.call('extension_inspect', { name }), 'extension'),
      enable: (name, imageDigest) => done('extension_enable', { name, image_digest: immutableDigest(imageDigest, 'extension image') }),
      disable: (name, imageDigest) => done('extension_disable', { name, image_digest: immutableDigest(imageDigest, 'extension image') }),
      remove: (name, imageDigest) => done('extension_remove', { name, image_digest: immutableDigest(imageDigest, 'extension image') }),
      startAcquisition: async (reference) => expect(await session.call('extension_acquisition_start', { reference }), 'extension_acquisition_job'),
      acquisition: async (job) => expect(await session.call('extension_acquisition_status', { job }), 'extension_acquisition'),
      cancelAcquisition: (job, revision) => done('extension_acquisition_cancel', { job, revision }),
      install: async (job, revision, granted) => expect(await session.call('extension_install', { job, revision, granted }), 'extension'),
      update: async (job, revision, granted) => expect(await session.call('extension_update', { job, revision, granted }), 'extension'),
    },
    containers: {
      list: async () => expect(await session.call('container_list'), 'containers'),
      inspect: async (id) => expect(await session.call('container_inspect', { id }), 'container'),
      processes: async (id) => expect(await session.call('container_processes', { id }), 'processes'),
      logs: async (id, { stdout = true, stderr = true } = {}) => expect(
        await session.call('container_logs', { id, stdout, stderr }), 'logs',
      ),
      execution: async (id) => expect(await session.call('execution_inspect', { id: immutableIdentity(id, [32], 'execution') }), 'execution'),
      executions: async () => expect(await session.call('execution_list'), 'executions'),
      executionLogs: async (id, { stdout = true, stderr = true } = {}) => expect(
        await session.call('execution_logs', { id: immutableIdentity(id, [32], 'execution'), stdout, stderr }), 'logs',
      ),
      waitExecution: async (id, { timeoutMs = 30_000 } = {}) => expect(
        await session.call('execution_wait', { id: immutableIdentity(id, [32], 'execution'), timeout_ms: timeoutMs }), 'execution',
      ),
      signalExecution: (id, signal) => done('execution_kill', { id: immutableIdentity(id, [32], 'execution'), signal }),
      removeExecution: (id) => done('execution_remove', {
        id: immutableIdentity(id, [32], 'execution'),
      }),
      create: async (configuration, legacyName) => {
        const spec = typeof configuration === 'string' ? {
          image: configuration, name: legacyName, hostname: null, entrypoint: null, command: [], environment: [],
          working_directory: null, user: null, labels: [], mounts: [], network: null, ports: [],
          memory_mb: null, cpus: null, pids_limit: null,
        } : {
          hostname: null, entrypoint: null, command: [], environment: [], working_directory: null, user: null,
          labels: [], mounts: [], network: null, ports: [], memory_mb: null, cpus: null,
          pids_limit: null, ...configuration,
        };
        const normalized = {
          ...spec,
          mounts: spec.mounts.map((mount) => ({ read_only: false, ...mount })),
        };
        return expect(await session.call('container_create', { spec: normalized }), 'identity');
      },
      start: (id) => done('container_start', { id: immutableIdentity(id, [32, 64], 'container') }),
      stop: (id) => done('container_stop', { id: immutableIdentity(id, [32, 64], 'container') }),
      remove: (id) => done('container_remove', { id: immutableIdentity(id, [32, 64], 'container') }),
      pause: (id) => done('container_pause', { id: immutableIdentity(id, [32, 64], 'container') }),
      unpause: (id) => done('container_unpause', { id: immutableIdentity(id, [32, 64], 'container') }),
      restart: (id) => done('container_restart', { id: immutableIdentity(id, [32, 64], 'container') }),
      rename: (id, name) => done('container_rename', {
        id: immutableIdentity(id, [32, 64], 'container'), name: exactContainerName(name),
      }),
      kill: (id, signal) => done('container_kill', { id: immutableIdentity(id, [32, 64], 'container'), signal }),
      exec: async (id, { command, user, workingDirectory } = {}) => expect(
        await session.call('container_exec', {
          id: immutableIdentity(id, [32, 64], 'container'), command,
          user: user ?? null, working_directory: workingDirectory ?? null,
        }), 'identity',
      ),
      attachTerminal: (id, command) => session.call('container_attach_terminal', {
        id: immutableIdentity(id, [32, 64], 'container'), command: exactCommand(command),
      }).then((reply) => expect(reply, 'identity')),
    },
    images: {
      list: async () => expect(await session.call('image_list'), 'images'),
      inspect: async (reference) => expect(await session.call('image_inspect', { reference }), 'image_details'),
      pull: async (reference) => expect(await session.call('image_pull', { reference }), 'image'),
      startPull: async (reference) => expect(await session.call('image_pull_start', { reference }), 'image_pull_job'),
      pullStatus: async (job) => expect(await session.call('image_pull_status', { job }), 'image_pull'),
      cancelPull: (job) => done('image_pull_cancel', { job }),
      remove: (reference) => done('image_remove', { reference: immutableDigest(reference, 'image') }),
      prune: async () => expect(await session.call('image_prune'), 'image_prune'),
    },
    volumes: {
      list: async () => expect(await session.call('volume_list'), 'volumes'),
      inspect: async (name) => expect(await session.call('volume_inspect', { name }), 'volume'),
      create: async (name) => expect(await session.call('volume_create', { name }), 'volume'),
      remove: (name, generation) => done('volume_remove', { name, generation: immutableIdentity(generation, [32], 'volume generation') }),
    },
    networks: {
      list: async () => expect(await session.call('network_list'), 'networks'),
      inspect: async (reference) => expect(await session.call('network_inspect', { reference }), 'network'),
      create: async (name) => expect(await session.call('network_create', { name }), 'identity'),
      remove: (reference) => done('network_remove', { reference: immutableIdentity(reference, [32], 'network') }),
      connect: (reference, container, options) => {
        const aliases = endpointAliases(options);
        const withValue = { reference: immutableIdentity(reference, [32], 'network'), container: immutableIdentity(container, [32, 64], 'container') };
        if (aliases.length > 0) withValue.aliases = aliases;
        return done('network_connect', withValue);
      },
      disconnect: (reference, container) => done('network_disconnect', { reference: immutableIdentity(reference, [32], 'network'), container: immutableIdentity(container, [32, 64], 'container') }),
    },
    terminal: {
      panes: async () => expect(await session.call('pane_list'), 'panes'),
      tabs: async () => expect(await session.call('terminal_tabs'), 'tabs'),
      topology: async () => expect(await session.call('terminal_topology'), 'topology'),
      openTab: async (title) => expect(await session.call('terminal_open_tab', { title }), 'identity'),
      split: async (slot, division) => expect(await session.call('terminal_split', { slot, division }), 'identity'),
      splitObserved: (slot, generation, revision, division) => {
        if (!Number.isSafeInteger(generation) || generation < 0 || !Number.isSafeInteger(revision) || revision < 0) {
          throw new TypeError('terminal split requires nonnegative safe integer generation and revision');
        }
        return session.call('terminal_split_observed', { slot, generation, revision, division })
          .then((reply) => expect(reply, 'identity'));
      },
      spawn: (slot, command) => {
        if (!Array.isArray(command) || command.length === 0 || command.length > 64
          || command.some((argument) => typeof argument !== 'string'
            || new TextEncoder().encode(argument).byteLength > 4096 || argument.includes('\0'))
          || command[0].length === 0
          || command.reduce((bytes, argument) => bytes + new TextEncoder().encode(argument).byteLength, 0) > 32 * 1024) {
          throw new RangeError('terminal command must contain 1..=64 NUL-free arguments, each at most 4096 bytes and 32768 bytes in aggregate');
        }
        return done('terminal_spawn', { slot, command: [...command] });
      },
      spawnObserved: (slot, generation, revision, command) => {
        if (!Number.isSafeInteger(generation) || generation < 0 || !Number.isSafeInteger(revision) || revision < 0) {
          throw new TypeError('terminal spawn requires nonnegative safe integer generation and revision');
        }
        if (!Array.isArray(command) || command.length === 0 || command.length > 64
          || command.some((argument) => typeof argument !== 'string'
            || new TextEncoder().encode(argument).byteLength > 4096 || argument.includes('\0'))
          || command[0].length === 0
          || command.reduce((bytes, argument) => bytes + new TextEncoder().encode(argument).byteLength, 0) > 32 * 1024) {
          throw new RangeError('terminal command must contain 1..=64 NUL-free arguments, each at most 4096 bytes and 32768 bytes in aggregate');
        }
        return done('terminal_spawn_observed', { slot, generation, revision, command: [...command] });
      },
      read: async (slot, lines) => expect(await session.call('terminal_read_pane', { slot, lines }), 'text'),
      semantics: async (slot) => expect(await session.call('pane_semantic_read', { slot }), 'semantics'),
      act: (slot, action) => {
        if (!Number.isSafeInteger(action?.generation) || action.generation < 0
          || !Number.isSafeInteger(action?.revision) || action.revision < 0
          || !Number.isSafeInteger(action?.node) || action.node < 0) {
          throw new TypeError('pane semantic action requires nonnegative safe integer generation, revision, and node');
        }
        if (action?.value != null && new TextEncoder().encode(action.value).byteLength > 4096) {
          throw new RangeError('pane semantic action value exceeds 4096 bytes');
        }
        return done('pane_semantic_action', { slot, action });
      },
      writeInput: (slot, generation, revision, input) => {
        if (!Number.isSafeInteger(generation) || generation < 0 || !Number.isSafeInteger(revision) || revision < 0) {
          throw new TypeError('terminal input requires nonnegative safe integer generation and revision');
        }
        const contents = typeof input === 'string' ? new TextEncoder().encode(input) : Uint8Array.from(input);
        if (contents.byteLength > 64 * 1024) throw new RangeError('terminal input exceeds the 65536 byte limit');
        return done('terminal_write_pane', { slot, generation, revision, contents: [...contents] });
      },
      resizeGrid: (slot, columns, rows) => {
        if (!Number.isInteger(columns) || !Number.isInteger(rows) || columns < 1 || rows < 1 || columns > 1000 || rows > 1000) {
          throw new RangeError('terminal grid rows and columns must be integers within 1..=1000');
        }
        return done('terminal_resize_grid', { slot, columns, rows });
      },
      resizeGridObserved: (slot, generation, revision, columns, rows) => {
        if (!Number.isSafeInteger(generation) || generation < 0 || !Number.isSafeInteger(revision) || revision < 0) throw new TypeError('terminal resize requires nonnegative safe integer generation and revision');
        if (!Number.isInteger(columns) || !Number.isInteger(rows) || columns < 1 || rows < 1 || columns > 1000 || rows > 1000) throw new RangeError('terminal grid rows and columns must be integers within 1..=1000');
        return done('terminal_resize_grid_observed', { slot, generation, revision, columns, rows });
      },
      close: (slot) => done('terminal_close_pane', { slot }),
      closeObserved: (slot, generation, revision) => {
        if (!Number.isSafeInteger(generation) || generation < 0 || !Number.isSafeInteger(revision) || revision < 0) {
          throw new TypeError('terminal close requires nonnegative safe integer generation and revision');
        }
        return done('terminal_close_pane_observed', { slot, generation, revision });
      },
      focus: (slot) => done('terminal_focus_pane', { slot }),
      focusObserved: (slot, generation, revision) => {
        if (!Number.isSafeInteger(generation) || generation < 0 || !Number.isSafeInteger(revision) || revision < 0) throw new TypeError('terminal focus requires nonnegative safe integer generation and revision');
        return done('terminal_focus_pane_observed', { slot, generation, revision });
      },
      retitle: (slot, title) => done('terminal_retitle_pane', { slot, title: exactPaneTitle(title) }),
      retitleObserved: (slot, generation, revision, title) => {
        if (!Number.isSafeInteger(generation) || generation < 0 || !Number.isSafeInteger(revision) || revision < 0) throw new TypeError('terminal retitle requires nonnegative safe integer generation and revision');
        return done('terminal_retitle_pane_observed', { slot, generation, revision, title: exactPaneTitle(title) });
      },
      ratio: (slot, ratio) => done('terminal_ratio', { slot, ratio }),
      ratioObserved: (slot, generation, revision, ratio) => {
        if (!Number.isSafeInteger(generation) || generation < 0 || !Number.isSafeInteger(revision) || revision < 0) {
          throw new TypeError('terminal ratio requires nonnegative safe integer generation and revision');
        }
        return done('terminal_ratio_observed', { slot, generation, revision, ratio });
      },
      switchOccupant: (slot, generation, target) => {
        if (!Number.isSafeInteger(generation) || generation < 0) throw new TypeError('pane generation must be a nonnegative safe integer');
        const terminal = target?.kind === 'terminal' && Object.keys(target).length === 1;
        const name = (value) => typeof value === 'string' && value.length <= 64 && /^[a-z0-9][a-z0-9._-]*$/.test(value);
        const surface = target?.kind === 'surface' && name(target.extension) && name(target.provider) && Object.keys(target).length === 3;
        if (!terminal && !surface) throw new TypeError('pane occupant target must be terminal or an exact extension/provider surface');
        return done('terminal_switch_occupant', { slot, generation, target: { ...target } });
      },
      switchOccupantObserved: (slot, generation, revision, target) => {
        if (!Number.isSafeInteger(generation) || generation < 0 || !Number.isSafeInteger(revision) || revision < 0) throw new TypeError('pane occupant switch requires nonnegative safe integer generation and revision');
        const terminal = target?.kind === 'terminal' && Object.keys(target).length === 1;
        const name = (value) => typeof value === 'string' && value.length <= 64 && /^[a-z0-9][a-z0-9._-]*$/.test(value);
        const surface = target?.kind === 'surface' && name(target.extension) && name(target.provider) && Object.keys(target).length === 3;
        if (!terminal && !surface) throw new TypeError('pane occupant target must be terminal or an exact extension/provider surface');
        return done('terminal_switch_occupant_observed', { slot, generation, revision, target: { ...target } });
      },
    },
    files: {
      list: async (path) => expect(await session.call('filesystem_list', { path }), 'entries'),
      read: async (path) => expect(await session.call('filesystem_read', { path }), 'contents'),
      readRange: async (path, offset = 0, limit = 65536, observed = null) => expect(
        await session.call('filesystem_read_range', { path, offset, limit, observed }),
        'file_range',
      ),
      stat: async (path) => expect(await session.call('filesystem_stat', { path }), 'entry'),
      write: (path, contents) => done('filesystem_write', { path, contents: [...contents] }),
      createObserved: async (path, contents) => expect(
        await session.call('filesystem_create_observed', { path, contents: [...contents] }),
        'identity',
      ),
      mkdir: (path) => done('filesystem_mkdir', { path }),
      rename: (from, to) => done('filesystem_rename', { from, to }),
      renameObserved: async (from, to, observed) => expect(
        await session.call('filesystem_rename_observed', { from, to, observed }),
        'identity',
      ),
      remove: (path) => done('filesystem_remove', { path }),
      removeObserved: (path, observed) => done('filesystem_remove_observed', { path, observed }),
    },
    subscribe,
    unsubscribe,
  };
  const providerCatalogue = (extensions) => {
    if (!Array.isArray(extensions) || extensions.some((extension) => typeof extension.enabled !== 'boolean' || !Array.isArray(extension.pane_providers))) {
      throw new Error('host does not expose installed provider declarations');
    }
    const all = extensions.filter(({ enabled }) => enabled).flatMap((extension) => extension.pane_providers.map((provider) => ({
      extension: extension.name,
      image_digest: extension.image_digest,
      version: extension.version ?? '',
      status: extension.status,
      id: provider.id,
      title: provider.title,
      icon: provider.icon ?? null,
    })));
    return { providers: all.slice(0, 200), truncated: all.length > 200 };
  };
  api.extensions.providers = async () => providerCatalogue(await api.extensions.list());
  api.extensions.waitForProviders = async (after, { timeoutMs = 30_000 } = {}) => {
    if (after == null || typeof after.name !== 'string' || typeof after.image_digest !== 'string' || typeof after.status !== 'string') {
      throw new TypeError('provider catalogue wait requires an exact extension name, image digest, and status cursor');
    }
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) throw new RangeError('provider catalogue wait timeout must be between 1 and 30000ms');
    let dispose; let timer; let settled = false;
    return new Promise((resolve, reject) => {
      const finish = (value, error) => {
        if (settled) return; settled = true; clearTimeout(timer);
        Promise.resolve(dispose?.()).then(() => error ? reject(error) : resolve(value), reject);
      };
      api.watchExtensions((extensions) => {
        const current = extensions.find(({ name }) => name === after.name);
        if (current?.image_digest === after.image_digest && current.status === after.status) return;
        try { finish({ changed: true, extension: current == null ? null : { name: current.name, image_digest: current.image_digest, status: current.status }, catalogue: providerCatalogue(extensions) }); }
        catch (error) { finish(undefined, error); }
      }).then((stop) => { dispose = stop; if (settled) void stop(); }, (error) => finish(undefined, error));
      timer = setTimeout(() => finish({ changed: false, after }), timeoutMs);
    });
  };
  api.watchContainers = async (listener) => {
    if (typeof listener !== 'function') throw new TypeError('container listener must be a function');
    const off = session.onEvent((event) => { if (event?.snapshot === 'containers') listener(event.of); });
    try { await api.subscribe('containers'); } catch (error) { off(); throw error; }
    return async () => { off(); await api.unsubscribe('containers'); };
  };
  api.watchPaneChanges = async (listener) => {
    if (typeof listener !== 'function') throw new TypeError('pane change listener must be a function');
    const off = session.onEvent((event) => {
      if (event?.snapshot === 'pane_changes') listener(event.of);
    });
    try { await api.subscribe('pane-changes'); } catch (error) { off(); throw error; }
    return async () => { off(); await api.unsubscribe('pane-changes'); };
  };
  api.extensions.waitForProviderMount = async (extension, provider, { state = 'mounted', after = null, timeoutMs = 30_000 } = {}) => {
    const providerName = (value) => typeof value === 'string' && value.length <= 128 && /^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(value);
    if (!providerName(extension) || !providerName(provider)) throw new TypeError('provider wait requires exact bounded extension and provider names');
    if (!['mounted', 'unmounted'].includes(state)) throw new TypeError('provider wait state must be mounted or unmounted');
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) throw new RangeError('provider wait timeout must be between 1 and 30000ms');
    if (after != null && (typeof after.slot !== 'string' || after.slot.length === 0
      || !Number.isSafeInteger(after.generation) || after.generation < 0
      || !Number.isSafeInteger(after.revision) || after.revision < 0)) throw new TypeError('provider wait cursor requires slot, generation, and revision');
    let dispose; let timer; let settled = false; let checking = false; let pending = false;
    return new Promise((resolve, reject) => {
      const finish = (value, error) => {
        if (settled) return; settled = true; clearTimeout(timer);
        Promise.resolve(dispose?.()).then(() => error ? reject(error) : resolve(value), reject);
      };
      const check = async () => {
        if (checking) { pending = true; return; }
        checking = true;
        try {
          do {
            pending = false;
            const inventory = await api.terminal.panes();
            const pane = inventory.panes.find((item) => item.kind === 'surface'
              && item.provider?.extension === extension && item.provider?.provider === provider);
            if (!pane && inventory.truncated) throw new Error('provider mount cannot be resolved from a truncated pane inventory');
            const unchanged = pane != null && after != null && pane.slot === after.slot
              && pane.generation === after.generation && pane.revision === after.revision;
            if ((state === 'mounted' && pane && !unchanged) || (state === 'unmounted' && !pane)) {
              finish({ changed: true, state, pane: pane ?? null, truncated: false }); return;
            }
          } while (pending && !settled);
        } catch (error) { finish(undefined, error); } finally { checking = false; }
      };
      api.watchPaneChanges(() => { void check(); }).then((stop) => {
        dispose = stop;
        if (settled) void stop(); else void check();
      }, (error) => finish(undefined, error));
      timer = setTimeout(() => finish({ changed: false, state, after }), timeoutMs);
    });
  };
  api.watchExecutions = async (listener) => {
    if (typeof listener !== 'function') throw new TypeError('execution listener must be a function');
    const off = session.onEvent((event) => { if (event?.snapshot === 'executions') listener(event.of); });
    try { await api.subscribe('executions'); } catch (error) { off(); throw error; }
    return async () => { off(); await api.unsubscribe('executions'); };
  };
  api.watchImagePulls = async (listener) => {
    if (typeof listener !== 'function') throw new TypeError('image pull listener must be a function');
    const off = session.onEvent((event) => { if (event?.snapshot === 'image_pulls') listener(event.of); });
    try { await api.subscribe('image-pulls'); } catch (error) { off(); throw error; }
    return async () => { off(); await api.unsubscribe('image-pulls'); };
  };
  api.watchExtensions = async (listener) => {
    if (typeof listener !== 'function') throw new TypeError('extension listener must be a function');
    const off = session.onEvent((event) => { if (event?.snapshot === 'extensions') listener(event.of); });
    try { await api.subscribe('extensions'); } catch (error) { off(); throw error; }
    return async () => { off(); await api.unsubscribe('extensions'); };
  };
  api.watchExtensionAcquisitions = async (listener) => {
    if (typeof listener !== 'function') throw new TypeError('extension acquisition listener must be a function');
    const off = session.onEvent((event) => { if (event?.snapshot === 'extension_acquisitions') listener(event.of); });
    try { await api.subscribe('extension-acquisitions'); } catch (error) { off(); throw error; }
    return async () => { off(); await api.unsubscribe('extension-acquisitions'); };
  };
  api.watchWorkspaceLifecycle = async (listener) => {
    if (typeof listener !== 'function') throw new TypeError('workspace lifecycle listener must be a function');
    const off = session.onEvent((event) => { if (event?.snapshot === 'workspace_lifecycle') listener(event.of); });
    try { await api.subscribe('workspace-lifecycle'); } catch (error) { off(); throw error; }
    return async () => { off(); await api.unsubscribe('workspace-lifecycle'); };
  };
  api.watchWorkspaceEvents = async (listener) => {
    if (typeof listener !== 'function') throw new TypeError('workspace event listener must be a function');
    const off = session.onEvent((event) => { if (event?.snapshot === 'workspace_events') listener(event.of); });
    try { await api.subscribe('workspace-events'); } catch (error) { off(); throw error; }
    return async () => { off(); await api.unsubscribe('workspace-events'); };
  };
  return api;
}

/** Mirrors Rust Request::capability for every fixed wire call used by this public facade. */
export function requestCapability(call) {
  if (['workspace_info', 'workspace_list', 'workspace_inspect'].includes(call)) return 'workspace-read';
  if (call.startsWith('workspace_')) return 'workspace-control';
  if (['extension_list', 'extension_inspect', 'extension_provider_list'].includes(call)) return 'extension-read';
  if (['extension_enable', 'extension_disable', 'extension_remove'].includes(call)) return 'extension-control';
  if (call.startsWith('extension_')) return 'extension-install';
  if (call === 'container_attach_terminal') return 'container-attach';
  if (['container_list', 'container_inspect', 'container_processes', 'container_logs', 'execution_inspect', 'execution_list', 'execution_logs', 'execution_wait'].includes(call)) return 'container-read';
  if (call.startsWith('container_') || call.startsWith('execution_')) return 'container-control';
  if (['image_list', 'image_inspect'].includes(call)) return 'image-read';
  if (call.startsWith('image_')) return 'image-write';
  if (['volume_list', 'volume_inspect'].includes(call)) return 'volume-read';
  if (call.startsWith('volume_')) return 'volume-write';
  if (['network_list', 'network_inspect'].includes(call)) return 'network-read';
  if (call.startsWith('network_')) return 'network-write';
  if (['terminal_tabs', 'terminal_topology'].includes(call)) return 'terminal-read';
  if (call === 'terminal_read_pane') return 'terminal-output';
  if (call === 'pane_list') return 'pane-observe';
  if (call === 'pane_semantic_read') return 'pane-semantic-read';
  if (call === 'pane_semantic_action') return 'pane-semantic-control';
  if (call.startsWith('terminal_')) return 'terminal-control';
  if (['filesystem_list', 'filesystem_read', 'filesystem_read_range', 'filesystem_stat'].includes(call)) return 'filesystem-read';
  if (call.startsWith('filesystem_')) return 'filesystem-write';
  if (call.startsWith('interface_') || call.startsWith('source_')) return 'interface';
  throw new RangeError(`unclassified extension request ${call}`);
}

/**
 * Renders an element tree into the extension's tab.
 *
 * The tab is opened first because the host refuses a render before one exists.
 * Returns a handle whose `update` re-renders and whose `close` tears down.
 */
export function render(element, session, { title = 'Extension', split = null } = {}) {
  const registry = attached.get(session);
  if (!registry) throw new Error('render requires a session returned by connect');
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
  let slot = null;
  let closed = false;
  let failed = null;
  let withdrawal = null;
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
    void session.call('interface_render_at', { slot, frame }).catch(() => {});
  };
  const surface = new Surface(transmit);
  const handle = { surface };
  registry.handles.add(handle);

  const opening = split === null
    ? session.call('interface_open_tab', { title })
    : session.call('interface_split', { slot: split.slot, division: split.division });
  const ready = opening.then((reply) => {
    if (reply?.reply !== 'identity' || typeof reply.with !== 'string' || reply.with.length === 0) {
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

/** Maximum Unicode characters retained by a LogView; Value patches append. */
export const LOG_VIEW_CHARACTER_LIMIT = 4_096;

/** Honest inventory of the current host contract; gaps are not callable APIs. */
export const protocolCoverage = Object.freeze({
  available: Object.freeze({
    workspace: ['info', 'list', 'inspect', 'create', 'update', 'delete', 'start', 'stop', 'restart'],
    containers: ['list', 'inspect', 'processes', 'logs', 'execution', 'executions', 'executionLogs', 'waitExecution', 'signalExecution', 'removeExecution', 'create', 'start', 'stop', 'remove', 'pause', 'unpause', 'restart', 'rename', 'kill', 'exec', 'attachTerminal'],
    images: ['list', 'inspect', 'pull', 'startPull', 'pullStatus', 'cancelPull', 'remove', 'prune'],
    volumes: ['list', 'inspect', 'create', 'remove'],
    networks: ['list', 'inspect', 'create', 'remove', 'connect', 'disconnect'],
    terminal: ['panes', 'tabs', 'topology', 'openTab', 'split', 'splitObserved', 'spawn', 'spawnObserved', 'read', 'semantics', 'act', 'writeInput', 'resizeGrid', 'resizeGridObserved', 'close', 'closeObserved', 'focus', 'focusObserved', 'retitle', 'retitleObserved', 'ratio', 'ratioObserved', 'switchOccupant', 'switchOccupantObserved'],
    files: ['list', 'read', 'readRange', 'stat', 'write', 'createObserved', 'mkdir', 'rename', 'renameObserved', 'remove', 'removeObserved'],
    extensions: ['list', 'inspect', 'enable', 'disable', 'remove', 'startAcquisition', 'acquisition', 'cancelAcquisition', 'install', 'update'],
    interfaceEvents: ['invoke', 'submit', 'change', 'select', 'scroll', 'close', 'context', 'key', 'focus', 'pointer', 'drag', 'drop'],
    workspaceEvents: ['key', 'focus', 'pointer'],
    snapshotTopics: SNAPSHOT_TOPICS,
  }),
  unavailable: Object.freeze({
    workspace: ['renameWhileUpdating', 'mutateWhileRunning', 'controlHostingWorkspace'],
    containers: [],
    images: [],
    terminal: [],
    events: [],
    extensions: [],
  }),
});
