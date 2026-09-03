export { ExtensionError, Session, SOCKET, PROTOCOL, validateUiEvent } from './session.js';
export {
  PROTOCOL_SPECIFICATION_VERSION, PROTOCOL_VERSION, PROTOCOL_BOUNDS,
  PROTOCOL_CAPABILITIES, PROTOCOL_TOPICS, PROTOCOL_REPLIES, PROTOCOL_REQUEST_CAPABILITIES, encodeRequest,
  validateRequest, validateReply, validateReplyFor, validateFailure, validateSnapshot,
} from './generated-protocol.js';
import { semanticXml } from './semantic.js';
export { semanticXml };
import { Session } from './session.js';
import { PROTOCOL_REPLIES, PROTOCOL_REQUEST_CAPABILITIES, PROTOCOL_TOPICS } from './generated-protocol.js';

/** Reference-counted host subscriptions, keyed by session and snapshot topic. */
const subscriptions = new WeakMap();
const SNAPSHOT_TOPICS = Object.freeze(['containers', 'container-inventory', 'executions', 'images', 'image-pulls', 'volumes', 'networks', 'terminal', 'pane-changes', 'extensions', 'extension-acquisitions', 'workspace-lifecycle', 'workspace-events']);

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

function exactOccupantTarget(target) {
  const terminal = target?.kind === 'terminal' && Object.keys(target).length === 1;
  const name = (value) => typeof value === 'string' && value.length <= 64 && /^[a-z0-9][a-z0-9._-]*$/.test(value);
  const surface = target?.kind === 'surface' && name(target.extension) && name(target.provider) && Object.keys(target).length === 3;
  if (!terminal && !surface) throw new TypeError('pane occupant target must be terminal or an exact extension/provider surface');
  return { ...target };
}

function exactSemanticAction(action) {
  if (!Number.isSafeInteger(action?.generation) || action.generation < 0
    || !Number.isSafeInteger(action?.revision) || action.revision < 0
    || !Number.isSafeInteger(action?.node) || action.node < 0) {
    throw new TypeError('pane semantic action requires nonnegative safe integer generation, revision, and node');
  }
  if (action?.value != null && new TextEncoder().encode(action.value).byteLength > 4096) {
    throw new RangeError('pane semantic action value exceeds 4096 bytes');
  }
  return action;
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

export async function connect(options = {}) {
  return Session.connect(options.path, options);
}

export function workspace(session, { signal } = {}) {
  const hostSession = session;
  if (signal !== undefined) {
    session = new Proxy(hostSession, {
      get(target, property) {
        if (property === 'call') return (name, argument) => target.call(name, argument, { signal });
        const value = Reflect.get(target, property, target);
        return typeof value === 'function' ? value.bind(target) : value;
      },
    });
  }
  const expect = (reply, kind) => {
    if (reply?.reply !== kind) throw new Error(`host replied ${reply?.reply ?? 'without a tag'}, expected ${kind}`);
    return reply.with;
  };
  const done = async (name, argument) => expect(await session.call(name, argument), 'done');
  const subscription = (call, topic) => {
    if (!SNAPSHOT_TOPICS.includes(topic)) throw new RangeError(`host does not publish the ${topic} snapshot topic`);
    return done(call, { topic });
  };
  const states = subscriptions.get(hostSession) ?? new Map();
  subscriptions.set(hostSession, states);
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
        await expect(await hostSession.call('event_unsubscribe', { topic }), 'done');
        state.active = false;
      }
      if (state.references === 0 && !state.active) states.delete(topic);
    });
    state.operation = operation.catch(() => {});
    await operation;
  };
  const api = {
    get granted() { return session.grantedCapabilities ?? session.granted; },
    get grantedCapabilities() { return session.grantedCapabilities ?? session.granted; },
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
      retry: (name, imageDigest) => done('extension_retry', { name, image_digest: immutableDigest(imageDigest, 'extension image') }),
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
      /** Converts either a terminal or a native UI pane into bounded agent-readable text. */
      toText: async (slot, { lines } = {}) => {
        const inventory = expect(await session.call('pane_list'), 'panes');
        const pane = inventory.panes.find((candidate) => candidate.slot === slot);
        if (!pane) {
          const detail = inventory.truncated
            ? 'pane cannot be resolved from a truncated inventory'
            : 'pane does not exist';
          throw new Error(`${detail}: ${slot}`);
        }
        if (pane.kind === 'terminal') {
          const snapshot = expect(await session.call('terminal_read_pane', { slot, lines }), 'text');
          return { kind: 'terminal', text: snapshot.lines.join('\n'), snapshot };
        }
        const snapshot = expect(await session.call('pane_semantic_read', { slot }), 'semantics');
        return { kind: 'ui', text: semanticXml(snapshot), snapshot };
      },
      act: (slot, action) => {
        return done('pane_semantic_action', { slot, action: exactSemanticAction(action) });
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
        return done('terminal_switch_occupant', { slot, generation, target: exactOccupantTarget(target) });
      },
      switchOccupantObserved: (slot, generation, revision, target) => {
        if (!Number.isSafeInteger(generation) || generation < 0 || !Number.isSafeInteger(revision) || revision < 0) throw new TypeError('pane occupant switch requires nonnegative safe integer generation and revision');
        return done('terminal_switch_occupant_observed', { slot, generation, revision, target: exactOccupantTarget(target) });
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
  Object.defineProperty(api, 'withSignal', {
    value: (nextSignal) => workspace(hostSession, { signal: nextSignal }),
    enumerable: false,
  });
  const watch = async (topic, snapshot, listener, label) => {
    if (typeof listener !== 'function') throw new TypeError(`${label} listener must be a function`);
    const off = hostSession.onEvent((event) => { if (event?.snapshot === snapshot) listener(event.of); });
    try { await subscribe(topic); } catch (error) { off(); throw error; }
    let stopping;
    const stop = () => stopping ??= (async () => {
      signal?.removeEventListener('abort', onAbort);
      off();
      await unsubscribe(topic);
    })();
    const onAbort = () => { void stop(); };
    signal?.addEventListener('abort', onAbort, { once: true });
    if (signal?.aborted) await stop();
    return stop;
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
  api.watchContainers = (listener) => watch('containers', 'containers', listener, 'container');
  api.watchContainerInventory = (listener) => watch('container-inventory', 'container_inventory', listener, 'container inventory');
  api.containers.startAndWait = async (id, { timeoutMs = 30_000 } = {}) => {
    const identity = immutableIdentity(id, [32, 64], 'container');
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
      throw new RangeError('container start wait timeout must be between 1 and 30000ms');
    }
    let sequence = 0; let baseline = 0; let started = false; let observed; let timer;
    const running = new Promise((resolve) => {
      observed = (containers) => {
        sequence += 1;
        if (!started || sequence <= baseline) return;
        const current = containers.find((container) => container.id === identity);
        if (current?.state === 'running') resolve(current);
      };
    });
    const stop = await api.watchContainers(observed);
    baseline = sequence;
    try {
      started = true;
      await api.containers.start(identity);
      const container = await Promise.race([
        running,
        new Promise((resolve) => { timer = setTimeout(() => resolve(null), timeoutMs); }),
      ]);
      return container === null ? { changed: false, id: identity, state: 'running' } : { changed: true, container };
    } finally {
      clearTimeout(timer);
      await stop();
    }
  };
  api.containers.stopAndWait = async (id, { timeoutMs = 30_000 } = {}) => {
    const identity = immutableIdentity(id, [32, 64], 'container');
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
      throw new RangeError('container stop wait timeout must be between 1 and 30000ms');
    }
    let sequence = 0; let baseline = 0; let stopped = false; let observed; let timer;
    const exited = new Promise((resolve) => {
      observed = (containers) => {
        sequence += 1;
        if (!stopped || sequence <= baseline) return;
        const current = containers.find((container) => container.id === identity);
        if (current?.state === 'exited') resolve(current);
      };
    });
    const stopWatching = await api.watchContainers(observed);
    baseline = sequence;
    try {
      stopped = true;
      await api.containers.stop(identity);
      const container = await Promise.race([
        exited,
        new Promise((resolve) => { timer = setTimeout(() => resolve(null), timeoutMs); }),
      ]);
      return container === null ? { changed: false, id: identity, state: 'exited' } : { changed: true, container };
    } finally {
      clearTimeout(timer);
      await stopWatching();
    }
  };
  api.containers.removeAndWait = async (id, { timeoutMs = 30_000 } = {}) => {
    const identity = immutableIdentity(id, [32, 64], 'container');
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) throw new RangeError('container remove wait timeout must be between 1 and 30000ms');
    let sequence = 0; let baseline = 0; let removing = false; let observed; let timer;
    const absent = new Promise((resolve) => {
      observed = (inventory) => {
        sequence += 1;
        if (!removing || sequence <= baseline || !inventory.complete) return;
        if (!inventory.containers.some((container) => container.id === identity)) resolve();
      };
    });
    const stopWatching = await api.watchContainerInventory(observed);
    baseline = sequence;
    try {
      removing = true;
      await api.containers.remove(identity);
      const removed = await Promise.race([absent.then(() => true), new Promise((resolve) => { timer = setTimeout(() => resolve(false), timeoutMs); })]);
      return { changed: removed, id: identity };
    } finally {
      clearTimeout(timer);
      await stopWatching();
    }
  };
  api.watchImages = (listener) => watch('images', 'images', listener, 'image');
  api.watchVolumes = (listener) => watch('volumes', 'volumes', listener, 'volume');
  api.watchNetworks = (listener) => watch('networks', 'networks', listener, 'network');
  api.watchTerminal = (listener) => watch('terminal', 'terminal', listener, 'terminal');
  api.watchPaneChanges = (listener) => watch('pane-changes', 'pane_changes', listener, 'pane change');
  api.terminal.waitForText = async (slot, after, { lines, timeoutMs = 30_000 } = {}) => {
    if (typeof slot !== 'string' || slot.length === 0) throw new TypeError('pane text wait requires a nonempty slot');
    if (after == null || !Number.isSafeInteger(after.generation) || after.generation < 0
      || !Number.isSafeInteger(after.revision) || after.revision < 0) {
      throw new TypeError('pane text wait requires an exact nonnegative generation and revision cursor');
    }
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
      throw new RangeError('pane text wait timeout must be between 1 and 30000ms');
    }
    let dispose; let timer; let settled = false; let reading = false; let pending;
    return new Promise((resolve, reject) => {
      const finish = (value, error) => {
        if (settled) return;
        settled = true; clearTimeout(timer);
        Promise.resolve(dispose?.()).then(() => error ? reject(error) : resolve(value), reject);
      };
      const observe = (change) => {
        if (settled || change.slot !== slot
          || (change.generation === after.generation && change.revision === after.revision)) return;
        pending = change;
        if (reading) return;
        reading = true;
        void (async () => {
          try {
            while (pending && !settled) {
              pending = undefined;
              const readable = await api.terminal.toText(slot, { lines });
              const cursor = readable.snapshot;
              if (cursor.generation === after.generation && cursor.revision === after.revision) continue;
              finish({ changed: true, readable });
            }
          } catch (error) { finish(undefined, error); }
          finally { reading = false; }
        })();
      };
      api.watchPaneChanges(observe).then((stop) => {
        dispose = stop;
        if (settled) void stop();
      }, (error) => finish(undefined, error));
      timer = setTimeout(() => finish({ changed: false, after }), timeoutMs);
    });
  };
  api.terminal.actAndWait = async (slot, action, { lines, timeoutMs = 30_000 } = {}) => {
    if (typeof slot !== 'string' || slot.length === 0) throw new TypeError('pane semantic action requires a nonempty slot');
    exactSemanticAction(action);
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
      throw new RangeError('pane semantic action wait timeout must be between 1 and 30000ms');
    }
    let changed;
    const observed = new Promise((resolve) => { changed = resolve; });
    const stop = await api.watchPaneChanges((change) => {
      if (change.slot === slot
        && (change.generation !== action.generation || change.revision !== action.revision)) changed(change);
    });
    let timer;
    try {
      await api.terminal.act(slot, action);
      const change = await Promise.race([
        observed,
        new Promise((resolve) => { timer = setTimeout(() => resolve(null), timeoutMs); }),
      ]);
      if (change === null) return { changed: false, after: { generation: action.generation, revision: action.revision } };
      const readable = await api.terminal.toText(slot, { lines });
      if (readable.snapshot.generation === action.generation && readable.snapshot.revision === action.revision) {
        throw new Error('pane change did not advance the readable snapshot cursor');
      }
      return { changed: true, readable };
    } finally {
      clearTimeout(timer);
      await stop();
    }
  };
  api.terminal.switchOccupantAndWait = async (slot, generation, revision, target, { timeoutMs = 30_000 } = {}) => {
    if (typeof slot !== 'string' || slot.length === 0) throw new TypeError('pane occupant switch requires a nonempty slot');
    if (!Number.isSafeInteger(generation) || generation < 0 || !Number.isSafeInteger(revision) || revision < 0) {
      throw new TypeError('pane occupant switch requires nonnegative safe integer generation and revision');
    }
    const wanted = exactOccupantTarget(target);
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
      throw new RangeError('pane occupant switch wait timeout must be between 1 and 30000ms');
    }
    let changed;
    const observed = new Promise((resolve) => { changed = resolve; });
    const stop = await api.watchPaneChanges((change) => {
      if (change.slot === slot && (change.generation !== generation || change.revision !== revision)) changed(change);
    });
    let timer;
    try {
      await api.terminal.switchOccupantObserved(slot, generation, revision, wanted);
      const change = await Promise.race([
        observed,
        new Promise((resolve) => { timer = setTimeout(() => resolve(null), timeoutMs); }),
      ]);
      if (change === null) return { changed: false, target: wanted, after: { generation, revision } };
      const inventory = await api.terminal.panes();
      const pane = inventory.panes.find((candidate) => candidate.slot === slot);
      if (!pane) throw new Error(inventory.truncated ? 'switched pane cannot be verified from a truncated inventory' : 'switched pane disappeared');
      const matches = wanted.kind === 'terminal'
        ? pane.kind === 'terminal'
        : pane.kind === 'surface' && pane.provider?.extension === wanted.extension && pane.provider?.provider === wanted.provider;
      if (!matches) throw new Error('pane changed without installing the requested occupant');
      return { changed: true, pane };
    } finally {
      clearTimeout(timer);
      await stop();
    }
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
  api.watchExecutions = (listener) => watch('executions', 'executions', listener, 'execution');
  api.watchImagePulls = (listener) => watch('image-pulls', 'image_pulls', listener, 'image pull');
  api.watchExtensions = (listener) => watch('extensions', 'extensions', listener, 'extension');
  api.extensions.enableAndWait = async (name, imageDigest, { timeoutMs = 30_000 } = {}) => {
    const digest = immutableDigest(imageDigest, 'extension image');
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
      throw new RangeError('extension enable wait timeout must be between 1 and 30000ms');
    }
    let observed;
    const inventory = new Promise((resolve, reject) => {
      observed = (extensions) => {
        const current = extensions.find((extension) => extension.name === name);
        if (current && current.image_digest !== digest) {
          reject(new Error(`extension ${name} was replaced while enabling`));
        } else if (current?.enabled) {
          resolve(current);
        }
      };
    });
    const stop = await api.watchExtensions(observed);
    let timer;
    try {
      await api.extensions.enable(name, digest);
      const extension = await Promise.race([
        inventory,
        new Promise((resolve) => { timer = setTimeout(() => resolve(null), timeoutMs); }),
      ]);
      return extension === null
        ? { changed: false, name, image_digest: digest }
        : { changed: true, extension };
    } finally {
      clearTimeout(timer);
      await stop();
    }
  };
  api.extensions.disableAndWait = async (name, imageDigest, { timeoutMs = 30_000 } = {}) => {
    const digest = immutableDigest(imageDigest, 'extension image');
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
      throw new RangeError('extension disable wait timeout must be between 1 and 30000ms');
    }
    let observed;
    const inventory = new Promise((resolve, reject) => {
      observed = (extensions) => {
        const current = extensions.find((extension) => extension.name === name);
        if (!current) reject(new Error(`extension ${name} disappeared while disabling`));
        else if (current.image_digest !== digest) reject(new Error(`extension ${name} was replaced while disabling`));
        else if (!current.enabled) resolve(current);
      };
    });
    const stop = await api.watchExtensions(observed);
    let timer;
    try {
      await api.extensions.disable(name, digest);
      const extension = await Promise.race([
        inventory,
        new Promise((resolve) => { timer = setTimeout(() => resolve(null), timeoutMs); }),
      ]);
      return extension === null
        ? { changed: false, name, image_digest: digest }
        : { changed: true, extension };
    } finally {
      clearTimeout(timer);
      await stop();
    }
  };
  api.extensions.retryAndWait = async (name, imageDigest, { timeoutMs = 30_000 } = {}) => {
    const digest = immutableDigest(imageDigest, 'extension image');
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
      throw new RangeError('extension retry wait timeout must be between 1 and 30000ms');
    }
    let observed;
    const inventory = new Promise((resolve, reject) => {
      observed = (extensions) => {
        const current = extensions.find((extension) => extension.name === name);
        if (!current) reject(new Error(`extension ${name} disappeared while retrying`));
        else if (current.image_digest !== digest) reject(new Error(`extension ${name} was replaced while retrying`));
        else if (current.enabled && current.status === 'duty') resolve(current);
      };
    });
    const stop = await api.watchExtensions(observed);
    let timer;
    try {
      await api.extensions.retry(name, digest);
      const extension = await Promise.race([
        inventory,
        new Promise((resolve) => { timer = setTimeout(() => resolve(null), timeoutMs); }),
      ]);
      return extension === null
        ? { changed: false, name, image_digest: digest }
        : { changed: true, extension };
    } finally {
      clearTimeout(timer);
      await stop();
    }
  };
  api.extensions.removeAndWait = async (name, imageDigest, { timeoutMs = 30_000 } = {}) => {
    const digest = immutableDigest(imageDigest, 'extension image');
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
      throw new RangeError('extension remove wait timeout must be between 1 and 30000ms');
    }
    let observed;
    const inventory = new Promise((resolve) => {
      observed = (extensions) => {
        const current = extensions.find((extension) => extension.name === name);
        if (!current || current.image_digest !== digest) resolve(current ?? null);
      };
    });
    const stop = await api.watchExtensions(observed);
    let timer;
    try {
      await api.extensions.remove(name, digest);
      const replacement = await Promise.race([
        inventory,
        new Promise((resolve) => { timer = setTimeout(() => resolve(undefined), timeoutMs); }),
      ]);
      return replacement === undefined
        ? { changed: false, name, image_digest: digest }
        : { changed: true, removed: { name, image_digest: digest }, replacement };
    } finally {
      clearTimeout(timer);
      await stop();
    }
  };
  api.watchExtensionAcquisitions = (listener) => watch('extension-acquisitions', 'extension_acquisitions', listener, 'extension acquisition');
  const commitAcquisitionAndWait = async (operation, job, revision, granted, { timeoutMs = 30_000 } = {}) => {
    if (!Number.isSafeInteger(revision) || revision < 0) {
      throw new TypeError(`extension ${operation} wait requires a nonnegative safe integer revision`);
    }
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
      throw new RangeError(`extension ${operation} wait timeout must be between 1 and 30000ms`);
    }
    const status = await api.extensions.acquisition(job);
    if (status.job !== job || status.revision !== revision || status.state !== 'ready' || !status.candidate) {
      throw new Error(`extension ${operation} requires the exact ready acquisition revision`);
    }
    const candidate = status.candidate;
    const digest = immutableDigest(candidate.image_digest, 'extension candidate image');
    if ((operation === 'install') !== (candidate.installed_image_digest == null)) {
      throw new Error(`extension candidate is not eligible for ${operation}`);
    }
    let observed;
    let authorityReturned = false;
    let latest;
    const inventory = new Promise((resolve, reject) => {
      observed = (extensions) => {
        const current = extensions.find((extension) => extension.name === candidate.name);
        if (!authorityReturned) { latest = current ?? null; return; }
        if (current?.image_digest === digest) resolve(current);
        else reject(new Error(`extension ${candidate.name} was replaced or disappeared after ${operation}`));
      };
    });
    const stop = await api.watchExtensions(observed);
    let timer;
    try {
      const committed = await api.extensions[operation](job, revision, granted);
      if (committed.name !== candidate.name || committed.image_digest !== digest) {
        throw new Error(`extension ${operation} returned a different candidate identity`);
      }
      authorityReturned = true;
      if (latest !== undefined) observed(latest === null ? [] : [latest]);
      const extension = await Promise.race([
        inventory,
        new Promise((resolve) => { timer = setTimeout(() => resolve(null), timeoutMs); }),
      ]);
      return extension === null
        ? { changed: false, name: candidate.name, image_digest: digest, revision }
        : { changed: true, extension };
    } finally {
      clearTimeout(timer);
      await stop();
    }
  };
  api.extensions.installAndWait = (job, revision, granted, options) => commitAcquisitionAndWait('install', job, revision, granted, options);
  api.extensions.updateAndWait = (job, revision, granted, options) => commitAcquisitionAndWait('update', job, revision, granted, options);
  api.extensions.waitForAcquisition = async (job, afterRevision, { timeoutMs = 30_000 } = {}) => {
    if (typeof job !== 'string' || job.length === 0 || new TextEncoder().encode(job).byteLength > 128) {
      throw new TypeError('extension acquisition wait requires a 1..128 byte job identity');
    }
    if (!Number.isSafeInteger(afterRevision) || afterRevision < 0) {
      throw new TypeError('extension acquisition wait requires a nonnegative safe integer revision');
    }
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30_000) {
      throw new RangeError('extension acquisition wait timeout must be between 1 and 30000ms');
    }
    let dispose; let timer; let settled = false; let reading = false; let latest;
    return new Promise((resolve, reject) => {
      const finish = (value, error) => {
        if (settled) return; settled = true; clearTimeout(timer);
        Promise.resolve(dispose?.()).then(() => error ? reject(error) : resolve(value), reject);
      };
      const observe = (change) => {
        if (settled || change.job !== job || change.revision <= afterRevision) return;
        latest = change;
        if (reading) return;
        reading = true;
        void (async () => {
          try {
            while (latest && !settled) {
              const expected = latest; latest = undefined;
              const status = await api.extensions.acquisition(job);
              if (status.job !== job || status.revision < expected.revision || status.revision <= afterRevision) continue;
              finish({ changed: true, status });
            }
          } catch (error) { finish(undefined, error); }
          finally { reading = false; }
        })();
      };
      api.watchExtensionAcquisitions(observe).then((stop) => {
        dispose = stop;
        if (settled) void stop();
      }, (error) => finish(undefined, error));
      timer = setTimeout(() => finish({ changed: false, job, revision: afterRevision }), timeoutMs);
    });
  };
  api.watchWorkspaceLifecycle = (listener) => watch('workspace-lifecycle', 'workspace_lifecycle', listener, 'workspace lifecycle');
  api.watchWorkspaceEvents = (listener) => watch('workspace-events', 'workspace_events', listener, 'workspace event');
  return api;
}

/** Mirrors Rust Request::capability for every fixed wire call used by this public facade. */
export function requestCapability(call) {
  const capability = PROTOCOL_REQUEST_CAPABILITIES[call];
  if (capability !== undefined && capability !== null) return capability;
  if (capability === null) throw new RangeError(`extension request ${call} has topic-selected capability`);
  throw new RangeError(`unclassified extension request ${call}`);
}

const camel = (value) => value.replace(/_([a-z])/g, (_, letter) => letter.toUpperCase());
const facadeOverrides = Object.freeze({
  extension_acquisition_start: 'extensions.startAcquisition',
  extension_acquisition_status: 'extensions.acquisition',
  extension_acquisition_cancel: 'extensions.cancelAcquisition',
  execution_inspect: 'containers.execution',
  execution_list: 'containers.executions',
  execution_logs: 'containers.executionLogs',
  execution_wait: 'containers.waitExecution',
  execution_kill: 'containers.signalExecution',
  execution_remove: 'containers.removeExecution',
  container_attach_terminal: 'containers.attachTerminal',
  image_pull_start: 'images.startPull',
  image_pull_status: 'images.pullStatus',
  image_pull_cancel: 'images.cancelPull',
  pane_list: 'terminal.panes',
  pane_semantic_read: 'terminal.semantics',
  pane_semantic_action: 'terminal.act',
  terminal_read_pane: 'terminal.read',
  terminal_write_pane: 'terminal.writeInput',
  terminal_close_pane: 'terminal.close',
  terminal_close_pane_observed: 'terminal.closeObserved',
  terminal_focus_pane: 'terminal.focus',
  terminal_focus_pane_observed: 'terminal.focusObserved',
  terminal_retitle_pane: 'terminal.retitle',
  terminal_retitle_pane_observed: 'terminal.retitleObserved',
});
const internalRequests = Object.freeze({
  interface_open_tab: 'owned by the React/native renderer root lifecycle',
  interface_split: 'owned by the React/native renderer root lifecycle',
  interface_withdraw: 'owned by the React/native renderer root lifecycle',
  interface_render: 'owned by the React/native renderer commit transport',
  interface_render_at: 'owned by the React/native renderer commit transport',
  source_resize: 'owned by the React/native renderer virtual source transport',
  source_resize_at: 'owned by the React/native renderer virtual source transport',
});
function facadePath(call) {
  if (facadeOverrides[call]) return facadeOverrides[call];
  for (const [prefix, group] of [
    ['workspace_', ''], ['extension_', 'extensions.'], ['container_', 'containers.'],
    ['image_', 'images.'], ['volume_', 'volumes.'], ['network_', 'networks.'],
    ['terminal_', 'terminal.'], ['filesystem_', 'files.'],
  ]) if (call.startsWith(prefix)) return group + camel(call.slice(prefix.length));
  return null;
}

/** Schema-derived inventory connecting every Rust request/topic to its supported public route. */
export const protocolSurface = Object.freeze({
  requests: Object.freeze(Object.fromEntries(Object.keys(PROTOCOL_REPLIES).map((call) => {
    if (internalRequests[call]) return [call, Object.freeze({ kind: 'internal', rationale: internalRequests[call] })];
    if (call === 'event_subscribe') return [call, Object.freeze({ kind: 'subscription', api: 'subscribe' })];
    if (call === 'event_unsubscribe') return [call, Object.freeze({ kind: 'subscription', api: 'unsubscribe' })];
    return [call, Object.freeze({ kind: 'facade', api: facadePath(call) })];
  }))),
  topics: Object.freeze(Object.fromEntries(PROTOCOL_TOPICS.map(({ wire }) => [wire,
    Object.freeze({ subscribe: 'subscribe', unsubscribe: 'unsubscribe' })]))),
});

/** Honest inventory of the current host contract; gaps are not callable APIs. */
export const protocolCoverage = Object.freeze({
  available: Object.freeze({
    workspace: ['info', 'list', 'inspect', 'create', 'adopt', 'update', 'delete', 'start', 'stop', 'restart'],
    containers: ['list', 'inspect', 'processes', 'logs', 'execution', 'executions', 'executionLogs', 'waitExecution', 'signalExecution', 'removeExecution', 'create', 'start', 'stop', 'remove', 'pause', 'unpause', 'restart', 'rename', 'kill', 'exec', 'attachTerminal'],
    images: ['list', 'inspect', 'pull', 'startPull', 'pullStatus', 'cancelPull', 'remove', 'prune'],
    volumes: ['list', 'inspect', 'create', 'remove'],
    networks: ['list', 'inspect', 'create', 'remove', 'connect', 'disconnect'],
    terminal: ['panes', 'tabs', 'topology', 'openTab', 'split', 'splitObserved', 'spawn', 'spawnObserved', 'read', 'semantics', 'act', 'writeInput', 'resizeGrid', 'resizeGridObserved', 'close', 'closeObserved', 'focus', 'focusObserved', 'retitle', 'retitleObserved', 'ratio', 'ratioObserved', 'switchOccupant', 'switchOccupantObserved'],
    files: ['list', 'read', 'readRange', 'stat', 'write', 'createObserved', 'mkdir', 'rename', 'renameObserved', 'remove', 'removeObserved'],
    extensions: ['list', 'inspect', 'enable', 'disable', 'retry', 'remove', 'startAcquisition', 'acquisition', 'cancelAcquisition', 'install', 'update'],
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
