import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';

import {
  ExtensionError,
  ExecutionOperationError,
  IncompleteCatalogueError,
  PROTOCOL_CAPABILITIES,
  Session,
  protocolCoverage,
  protocolSurface,
  requestCapability,
  validateRowRequest,
  validateUiEvent,
  workspace,
} from '../dist/index.js';
import { KIND, Reader, encode } from '../dist/wire.js';
import { PROTOCOL } from '../dist/session.js';

const FILE_JOURNAL = '0123456789abcdef0123456789abcdef';
const NEXT_FILE_JOURNAL = 'fedcba9876543210fedcba9876543210';

test('row requests reject unsafe or unbounded database windows', () => {
  const request = {
    id: 4,
    source: 7,
    version: 2,
    range: { start: 999_936, count: 128 },
    sort: { column: 'name', descending: true },
    filter: 'active',
    slot: 'surface-2',
  };
  assert.equal(validateRowRequest(request), request);
  assert.throws(
    () => validateRowRequest({ ...request, range: { start: 0, count: 129 } }),
    /between 1 and 128/,
  );
  assert.throws(
    () => validateRowRequest({ ...request, sort: { column: 'name', descending: 'yes' } }),
    /boolean direction/,
  );
  assert.throws(() => validateRowRequest({ ...request, slot: '' }), /nonempty NUL-free string/);
  assert.throws(
    () => validateRowRequest({ ...request, slot: 'x'.repeat(257) }),
    /at most 256 bytes/,
  );
  assert.throws(
    () => validateRowRequest({ ...request, filter: 'x'.repeat(4097) }),
    /at most 4096 bytes/,
  );
  assert.throws(
    () => validateRowRequest({ ...request, sort: { column: 'x'.repeat(129), descending: false } }),
    /at most 128 bytes/,
  );
  assert.throws(() => validateRowRequest({ ...request, filter: 'active\0drop' }), /NUL-free/);
});

test('JSON state codecs migrate, retry CAS conflicts, and remain bounded before framing', async () => {
  const calls = [];
  let reads = 0;
  let writes = 0;
  const api = workspace({
    granted: ['state:read', 'state:write'],
    async call(name, payload) {
      calls.push([name, payload]);
      if (name === 'state_read') {
        reads += 1;
        const value = reads === 1 ? undefined : { version: 1, schemas: ['public'] };
        return {
          reply: 'state',
          with: {
            identity: reads === 1 ? 'absent' : `sha256:${'b'.repeat(64)}`,
            contents:
              value === undefined ? [] : [...new TextEncoder().encode(JSON.stringify(value))],
          },
        };
      }
      writes += 1;
      if (writes === 1)
        throw new ExtensionError({
          error: 'conflict',
          detail: 'extension state changed after it was read',
        });
      return { reply: 'identity', with: `sha256:${'c'.repeat(64)}` };
    },
    onEvent() {
      return () => {};
    },
  });
  const codec = {
    decode(value) {
      if (value === undefined) return { version: 2, schemas: [], refreshes: 0 };
      if (value?.version === 1) return { version: 2, schemas: value.schemas, refreshes: 0 };
      if (value?.version === 2) return value;
      throw new TypeError('unsupported state schema');
    },
    encode(value) {
      return value;
    },
  };
  let updates = 0;
  const result = await api.state.updateJson(codec, (current) => {
    updates += 1;
    return { ...current, refreshes: current.refreshes + 1 };
  });
  assert.deepEqual(result, {
    identity: `sha256:${'c'.repeat(64)}`,
    value: { version: 2, schemas: ['public'], refreshes: 1 },
  });
  assert.equal(updates, 2, 'the pure update is rerun against fresh state after a CAS conflict');
  const writesOnWire = calls.filter(([name]) => name === 'state_write');
  assert.equal(writesOnWire[0][1].observed, 'absent');
  assert.deepEqual(
    JSON.parse(new TextDecoder().decode(Uint8Array.from(writesOnWire[1][1].contents))),
    result.value,
  );

  const beforeInvalid = calls.length;
  assert.throws(
    () => api.state.writeJson('absent', { data: 'x'.repeat(1024 * 1024) }, codec),
    /1 MiB/,
  );
  assert.equal(calls.length, beforeInvalid, 'oversized JSON is rejected before framing');
  await assert.rejects(
    api.state.updateJson(codec, (value) => value, { attempts: 0 }),
    /1 through 16/,
  );
});

test('resizeGridAndWait verifies the requested grid after an observed cursor advance', async () => {
  const calls = [];
  let publish;
  const api = workspace({
    granted: [],
    call() {
      throw new Error('raw call was not stubbed');
    },
    onEvent() {
      return () => {};
    },
  });
  api.watchPaneChanges = async (listener) => {
    publish = listener;
    calls.push('subscribe');
    return async () => calls.push('unsubscribe');
  };
  let reads = 0;
  api.terminal.read = async (...args) => {
    calls.push(['read', ...args]);
    reads += 1;
    return reads === 1
      ? {
          slot: 'pane-1',
          generation: 4,
          revision: 9,
          columns: 80,
          rows: 24,
          lines: [],
          truncated: false,
        }
      : {
          slot: 'pane-1',
          generation: 4,
          revision: 10,
          columns: 120,
          rows: 40,
          lines: [],
          truncated: false,
        };
  };
  api.terminal.resizeGridObserved = async (...args) => {
    calls.push(['resize', ...args]);
    publish({ slot: 'pane-1', kind: 'terminal', generation: 4, revision: 10, coalesced: 0 });
  };
  const result = await api.terminal.resizeGridAndWait('pane-1', 4, 9, 120, 40, { lines: 5 });
  assert.equal(result.changed, true);
  assert.deepEqual([result.after.columns, result.after.rows], [120, 40]);
  assert.deepEqual(calls, [
    'subscribe',
    ['read', 'pane-1', 5],
    ['resize', 'pane-1', 4, 9, 120, 40],
    ['read', 'pane-1', 5],
    'unsubscribe',
  ]);
});

test('ratioAndWait verifies a second-child share through authoritative topology', async () => {
  const calls = [];
  let publish;
  const api = workspace({
    granted: [],
    call() {
      throw new Error('raw call was not stubbed');
    },
    onEvent() {
      return () => {};
    },
  });
  api.watchPaneChanges = async (listener) => {
    publish = listener;
    calls.push('subscribe');
    return async () => calls.push('unsubscribe');
  };
  api.terminal.ratioObserved = async (...args) => {
    calls.push(['ratio', ...args]);
    publish({ slot: 'pane-2', kind: 'terminal', generation: 3, revision: 8, coalesced: 0 });
  };
  const pane = {
    slot: 'pane-2',
    generation: 3,
    revision: 8,
    kind: 'terminal',
    provider: null,
    tab: 'tab-1',
    title: 'Shell',
    focused: false,
  };
  api.terminal.panes = async () => ({ panes: [pane], truncated: false });
  api.terminal.topology = async () => ({
    active_tab: 'tab-1',
    tabs: [
      {
        id: 'tab-1',
        title: 'Shell',
        root: {
          kind: 'split',
          division: 'beside',
          ratio_per_mille: 400,
          first: { kind: 'pane', pane: { slot: 'pane-1' }, grid: null, focused: true },
          second: { kind: 'pane', pane: { slot: 'pane-2' }, grid: null, focused: false },
        },
      },
    ],
  });
  assert.deepEqual(await api.terminal.ratioAndWait('pane-2', 3, 7, 0.6), {
    changed: true,
    ratio: 0.6,
    actual: 0.6,
    pane,
  });
  assert.deepEqual(calls, ['subscribe', ['ratio', 'pane-2', 3, 7, 0.6], 'unsubscribe']);
  await assert.rejects(api.terminal.ratioAndWait('pane-2', 3, 7, 0.01), /0.05/);
});

async function pair(options) {
  const server = net.createServer();
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  const accepted = new Promise((resolve) => server.once('connection', resolve));
  const connecting = new Promise((resolve, reject) => {
    const socket = net.createConnection(address.port, '127.0.0.1');
    socket.once('error', reject);
    socket.once('connect', () => resolve(socket));
  });
  const [host, extension] = await Promise.all([accepted, connecting]);
  host.write(
    encode({
      channel: 0,
      kind: KIND.open,
      payload: {
        protocol: PROTOCOL,
        extension: 'test',
        granted: PROTOCOL_CAPABILITIES.map(({ wire }) => wire),
      },
    }),
  );
  const session = new Session(extension, options);
  await session.ready;
  return { host, session, server };
}

function frames(stream) {
  const reader = new Reader();
  const queued = [];
  const waiters = [];
  stream.on('data', (chunk) => {
    for (const frame of reader.take(chunk)) {
      const waiter = waiters.shift();
      if (waiter) waiter(frame);
      else queued.push(frame);
    }
  });
  return () =>
    new Promise((resolve) => {
      const frame = queued.shift();
      if (frame) resolve(frame);
      else waiters.push(resolve);
    });
}

test('ordered replies correlate concurrent typed calls and failures reject', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next(); // hello
  const api = workspace(stage.session);
  const info = api.info();
  const list = api.containers.list();
  const networks = api.networks.list();
  assert.equal((await next()).payload.call, 'workspace_info');
  assert.equal((await next()).payload.call, 'container_list');
  assert.equal((await next()).payload.call, 'network_list');
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'workspace',
        with: { name: 'dev', architecture: 'arm64', image: 'alpine' },
      },
    }),
  );
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      flags: 3,
      payload: { error: 'denied', capability: 'containers:read', detail: 'not granted' },
    }),
  );
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      flags: 3,
      payload: { error: 'unavailable', detail: 'socket refused' },
    }),
  );
  assert.equal((await info).name, 'dev');
  await assert.rejects(list, (error) => error instanceof ExtensionError && error.kind === 'denied');
  await assert.rejects(
    networks,
    (error) =>
      error instanceof ExtensionError &&
      error.kind === 'unavailable' &&
      error.message === 'socket refused',
  );
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('background notification validates before framing and uses its exact Unix call', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  assert.throws(
    () => api.notifications.publish({ id: 'bad\n', title: 'Build', body: 'done' }),
    /control characters/,
  );
  const notification = { id: 'index-build', title: 'Index ready', body: '1,000,000 rows indexed' };
  const publishing = api.notifications.publish(notification);
  assert.deepEqual((await next()).payload, {
    call: 'notification_publish',
    with: { notification },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.equal(await publishing, undefined);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('the complete typed facade binds cancellation without changing method arguments', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const controller = new AbortController();
  const api = workspace(stage.session).withSignal(controller.signal);
  const pending = api.containers.inspect('a'.repeat(32));
  assert.deepEqual((await next()).payload, {
    call: 'container_inspect',
    with: { id: 'a'.repeat(32) },
  });
  controller.abort();
  await assert.rejects(pending, { name: 'AbortError' });
  await assert.rejects(api.info(), /closed/);
  stage.host.destroy();
  stage.server.close();
});

test('an already-aborted typed facade emits no Unix request frame', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(workspace(stage.session, { signal: controller.signal }).info(), {
    name: 'AbortError',
  });
  const wrote = await Promise.race([
    next().then(() => true),
    new Promise((resolve) => setTimeout(() => resolve(false), 20)),
  ]);
  assert.equal(wrote, false);
  await stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('aborting a bound watcher releases its shared subscription without closing the session', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const controller = new AbortController();
  const api = workspace(stage.session).withSignal(controller.signal);
  const watching = api.watchContainers(() => {});
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'containers' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const stop = await watching;
  controller.abort();
  assert.deepEqual((await next()).payload, {
    call: 'event_unsubscribe',
    with: { topic: 'containers' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await stop();
  const info = workspace(stage.session).info();
  assert.equal((await next()).payload.call, 'workspace_info');
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'workspace',
        with: { name: 'dev', architecture: 'arm64', image: 'alpine' },
      },
    }),
  );
  assert.equal((await info).name, 'dev');
  await stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('pending calls are bounded and a timeout closes the ambiguous ordered stream', async () => {
  const stage = await pair({ pendingLimit: 2, timeout: 100 });
  const next = frames(stage.host);
  await next();
  const first = stage.session.call('workspace_info');
  await new Promise((resolve) => setTimeout(resolve, 60));
  const second = stage.session.call('workspace_list');
  await assert.rejects(stage.session.call('container_list'), /limit/);
  await assert.rejects(first, /timed out/);
  await assert.rejects(second, /timed out/);
  await assert.rejects(stage.session.call('image_list'), /closed/);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('an event returns credit only after delivery', async () => {
  const seen = [];
  const stage = await pair({ onEvent: (event) => seen.push(event) });
  const next = frames(stage.host);
  await next();
  const subscribed = stage.session.call('event_subscribe', { topic: 'containers' });
  await next();
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await subscribed;
  stage.host.write(
    encode({ channel: 4, kind: KIND.event, payload: { snapshot: 'containers', of: [] } }),
  );
  const credit = await next();
  assert.deepEqual(seen, [{ snapshot: 'containers', of: [] }]);
  assert.equal(credit.channel, 4);
  assert.equal(credit.kind, KIND.credit);
  assert.equal(credit.payload, 1);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('a throwing event listener cannot starve healthy listeners or event credit', async () => {
  const seen = [];
  const errors = [];
  const stage = await pair({ onEventError: (error) => errors.push(error.message) });
  const next = frames(stage.host);
  await next();
  const subscribed = stage.session.call('event_subscribe', { topic: 'images' });
  await next();
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await subscribed;
  stage.session.onEvent(() => {
    throw new Error('broken observer');
  });
  stage.session.onEvent((event) => seen.push(event));
  stage.host.write(
    encode({
      channel: 9,
      kind: KIND.event,
      payload: { snapshot: 'images', of: { images: [], truncated: false } },
    }),
  );
  const credit = await next();
  assert.deepEqual(errors, ['broken observer']);
  assert.deepEqual(seen, [{ snapshot: 'images', of: { images: [], truncated: false } }]);
  assert.deepEqual(
    { channel: credit.channel, kind: credit.kind, payload: credit.payload },
    {
      channel: 9,
      kind: KIND.credit,
      payload: 1,
    },
  );
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('a reply for the wrong operation fails closed and rejects every correlated caller', async () => {
  const closed = [];
  const stage = await pair({ onClose: (error) => closed.push(error.message) });
  const next = frames(stage.host);
  await next();
  const first = stage.session.call('workspace_info');
  const second = stage.session.call('workspace_list');
  await next();
  await next();
  const disconnected = new Promise((resolve) => stage.host.once('close', resolve));
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'workspaces', with: [] } }),
  );
  await assert.rejects(first, /reply\.reply must be workspace/);
  await assert.rejects(second, /reply\.reply must be workspace/);
  await disconnected;
  assert.deepEqual(closed, ['reply.reply must be workspace']);
  stage.server.close();
});

test('a malformed failure rejects the call and closes the ordered stream', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const pending = stage.session.call('workspace_info');
  await next();
  const disconnected = new Promise((resolve) => stage.host.once('close', resolve));
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, flags: 3, payload: { error: 'denied' } }),
  );
  await assert.rejects(pending, /failure\.capability must be present/);
  await disconnected;
  await assert.rejects(stage.session.call('workspace_info'), /closed/);
  stage.server.close();
});

test('a malformed subscribed snapshot closes without delivery or returned credit', async () => {
  const seen = [];
  const stage = await pair({ onEvent: (event) => seen.push(event) });
  const next = frames(stage.host);
  await next();
  const subscribed = stage.session.call('event_subscribe', { topic: 'containers' });
  await next();
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await subscribed;
  const disconnected = new Promise((resolve) => stage.host.once('close', resolve));
  stage.host.write(
    encode({ channel: 8, kind: KIND.event, payload: { snapshot: 'containers', of: [{}] } }),
  );
  await disconnected;
  assert.deepEqual(seen, []);
  assert.equal(stage.host.readableLength, 0, 'invalid events return no credit');
  stage.server.close();
});

test('separately typed GUI interaction events remain deliverable and return credit', async () => {
  const seen = [];
  const stage = await pair({ onEvent: (event) => seen.push(event) });
  const next = frames(stage.host);
  await next();
  const event = {
    interaction: 'key',
    trigger: 'Key',
    node: 7,
    id: '7:Key',
    slot: 'pane-1',
    key: 'a',
    keycode: 38,
    modifiers: 4,
    pressed: true,
  };
  stage.host.write(encode({ channel: 9, kind: KIND.event, payload: event }));
  const credit = await next();
  assert.deepEqual(seen, [event]);
  assert.deepEqual(
    { channel: credit.channel, kind: credit.kind, payload: credit.payload },
    { channel: 9, kind: KIND.credit, payload: 1 },
  );
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('generated UI event validation accepts only the canonical payload', () => {
  const current = {
    interaction: 'drop',
    trigger: 'Drop',
    node: 7,
    id: '7:Drop',
    slot: 'pane-1',
    source: 4,
    x: 2.5,
    y: 8,
  };
  assert.equal(validateUiEvent(current), current);
  const omitted = {
    interaction: 'drop',
    trigger: 'Drop',
    node: 7,
    id: '7:Drop',
    source: 4,
    x: 2.5,
    y: 8,
  };
  const nullable = { ...omitted, slot: null };
  assert.equal(validateUiEvent(omitted), omitted);
  assert.equal(validateUiEvent(nullable), nullable);
  assert.throws(
    () =>
      validateUiEvent({
        interaction: 'drop',
        trigger: 'Drop',
        node: 7,
        id: '7:Drop',
        x: 2.5,
        y: 8,
      }),
    /ui event/,
  );
  assert.throws(
    () =>
      validateUiEvent({
        slot: 'pane-1',
        event: { Drop: { node: 7, id: '7:Drop', source: 4, x: 2.5, y: 8 } },
      }),
    /interaction/,
  );
});

test('concurrent pane-change waits share their host subscription until the last disposer', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const first = api.watchPaneChanges(() => {});
  const second = api.watchPaneChanges(() => {});
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'pane-changes' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const [stopFirst, stopSecond] = await Promise.all([first, second]);

  await stopFirst();
  const probe = stage.session.call('workspace_info');
  assert.equal(
    (await next()).payload.call,
    'workspace_info',
    'the first disposer must not unsubscribe the second wait',
  );
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'workspace',
        with: { name: 'dev', architecture: 'arm64', image: 'alpine:3.20' },
      },
    }),
  );
  await probe;

  const stopped = stopSecond();
  assert.deepEqual((await next()).payload, {
    call: 'event_unsubscribe',
    with: { topic: 'pane-changes' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await stopped;
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('workspace lifecycle methods use the typed control calls', async (context) => {
  const stage = await pair();
  context.after(async () => {
    await stage.session.close();
    stage.host.destroy();
    await new Promise((resolve) => stage.server.close(resolve));
  });
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const configuration = {
    configuration_revision: 'fedcba9876543210fedcba9876543210',
    name: 'other',
    image: 'alpine:3.20',
    architecture: 'arm64',
    storage: null,
    shell: null,
    cpus: null,
    memory_mb: null,
    environment: [],
    mounts: [],
    docker_socket: true,
    scrollback: 100000,
    vpn: null,
    execution_lifetime: 'persisted',
    terminal: {
      font_family: null,
      font_size: null,
      foreground: null,
      background: null,
      cursor_shape: null,
      cursor_blink: null,
    },
  };
  const operations = [
    api.inspect('other'),
    api.create(configuration),
    api.update(
      'other',
      '0123456789abcdef0123456789abcdef',
      configuration.configuration_revision,
      configuration,
    ),
    api.delete('other', '0123456789abcdef0123456789abcdef'),
    api.start('other'),
    api.stop('other'),
    api.restart('other'),
  ];
  assert.equal('adopt' in api, false, 'the removed adoption facade is not advertised');
  assert.equal(
    'workspace_adopt' in protocolSurface.requests,
    false,
    'the generated protocol surface does not advertise the removed call',
  );
  const calls = [];
  for (let index = 0; index < operations.length; index += 1) calls.push((await next()).payload);
  assert.deepEqual(
    calls.map((call) => call.call),
    [
      'workspace_inspect',
      'workspace_create',
      'workspace_update',
      'workspace_delete',
      'workspace_start',
      'workspace_stop',
      'workspace_restart',
    ],
  );
  for (let index = 0; index < operations.length; index += 1) {
    const payload =
      index < 3 ? { reply: 'workspace_configuration', with: configuration } : { reply: 'done' };
    stage.host.write(encode({ channel: 2, kind: KIND.response, payload }));
  }
  const results = await Promise.all(operations);
  assert.deepEqual(results.slice(0, 3), [configuration, configuration, configuration]);
  assert.deepEqual(results.slice(3), [undefined, undefined, undefined, undefined]);
});

test('workspace environment patch preserves exact CAS framing without returning values', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const generation = '0123456789abcdef0123456789abcdef';
  const revision = 'fedcba9876543210fedcba9876543210';
  const pending = api.patchEnvironment('dev', generation, revision, {
    set: [['PGPASSWORD', 'rotated']],
    remove: ['OLD_PASSWORD'],
  });
  assert.deepEqual((await next()).payload, {
    call: 'workspace_environment_patch',
    with: {
      name: 'dev',
      generation,
      configuration_revision: revision,
      patch: { set: [['PGPASSWORD', 'rotated']], remove: ['OLD_PASSWORD'] },
    },
  });
  const nextRevision = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'workspace_environment_patch',
        with: { generation, configuration_revision: nextRevision, changed: true },
      },
    }),
  );
  assert.deepEqual(await pending, {
    generation,
    configuration_revision: nextRevision,
    changed: true,
  });
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('coverage names delivered snapshots and leaves unsupported topics unavailable', () => {
  assert.deepEqual(protocolCoverage.available.workspace, [
    'info',
    'list',
    'inspect',
    'create',
    'adopt',
    'update',
    'patchEnvironment',
    'delete',
    'start',
    'stop',
    'restart',
  ]);
  assert.ok(protocolCoverage.available.containers.includes('create'));
  assert.ok(protocolCoverage.available.containers.includes('remove'));
  assert.ok(protocolCoverage.available.terminal.includes('read'));
  assert.ok(protocolCoverage.available.terminal.includes('split'));
  assert.ok(protocolCoverage.unavailable.workspace.includes('mutateWhileRunning'));
  assert.ok(protocolCoverage.available.containers.includes('processes'));
  assert.deepEqual(protocolCoverage.available.images, [
    'inventory',
    'list',
    'inspect',
    'pull',
    'startPull',
    'pullStatus',
    'cancelPull',
    'remove',
    'prune',
    'removeAndWait',
  ]);
  assert.deepEqual(protocolCoverage.unavailable.images, []);
  assert.deepEqual(protocolCoverage.available.snapshotTopics, [
    'containers',
    'container-inventory',
    'executions',
    'images',
    'image-pulls',
    'volumes',
    'networks',
    'terminal',
    'pane-changes',
    'extensions',
    'extension-acquisitions',
    'workspace-lifecycle',
    'workspace-events',
    'filesystem',
  ]);
  assert.ok(protocolCoverage.available.terminal.includes('switchOccupant'));
  assert.ok(!protocolCoverage.unavailable.events.includes('extensions'));
  assert.deepEqual(protocolCoverage.available.extensions, [
    'list',
    'catalogue',
    'requireCompleteCatalogue',
    'inspect',
    'enable',
    'disable',
    'retry',
    'remove',
    'startAcquisition',
    'acquisition',
    'cancelAcquisition',
    'install',
    'update',
  ]);
  assert.deepEqual(protocolCoverage.unavailable.extensions, []);
  assert.ok(protocolCoverage.available.workspaceEvents.includes('key'));
  assert.ok(
    !protocolCoverage.unavailable.events.some((name) => name.startsWith('global')),
    'window-level workspace events must not also be advertised as unavailable under stale names',
  );
  assert.ok(protocolCoverage.available.interfaceEvents.includes('drag'));
  assert.ok(protocolCoverage.available.interfaceEvents.includes('drop'));
  assert.ok(!protocolCoverage.unavailable.events.includes('drag'));
  const api = workspace({
    granted: ['workspaces:read'],
    call() {
      throw new Error('not called');
    },
  });
  assert.deepEqual(api.granted, ['workspaces:read']);
  assert.equal(api.renameWorkspace, undefined);
  assert.equal(typeof api.containers.processes, 'function');
  assert.equal(typeof api.volumes.create, 'function');
  assert.equal(typeof api.networks.connect, 'function');
  assert.deepEqual(
    Object.keys(api.images),
    protocolCoverage.available.images,
    'coverage must enumerate every callable typed image authority in API order',
  );
  assert.equal(typeof api.terminal.writeInput, 'function');
  assert.equal(typeof api.terminal.switchOccupant, 'function');
  assert.deepEqual(
    Object.keys(api.files),
    protocolCoverage.available.files,
    'coverage must enumerate observed filesystem authorities as well as compatibility calls',
  );
});

test('the schema-derived public surface covers every Rust request and topic', () => {
  const schema = JSON.parse(
    fs.readFileSync(
      new URL('../../../../src/workspaces/hl-extension/protocol/v1.json', import.meta.url),
    ),
  );
  const requests = schema.roots.request.variants.map(({ name }) => name);
  const topics = schema.topics.map(({ wire }) => wire);
  assert.deepEqual(Object.keys(protocolSurface.requests), requests);
  assert.deepEqual(Object.keys(protocolSurface.topics), topics);
  assert.deepEqual(
    Object.entries(protocolSurface.requests)
      .filter(([, route]) => route.kind === 'internal')
      .map(([call]) => call),
    [
      'interface_open_tab',
      'interface_split',
      'interface_withdraw',
      'interface_render',
      'interface_render_at',
      'source_resize',
      'source_resize_at',
    ],
    'only renderer-owned lifecycle, commit, and virtual-source transport may lack facade methods',
  );

  const api = workspace({
    granted: [],
    call() {
      throw new Error('not called');
    },
    onEvent() {
      return () => {};
    },
  });
  for (const [call, route] of Object.entries(protocolSurface.requests)) {
    if (route.kind === 'internal') {
      assert.match(route.rationale, /renderer/, `${call} lacks a concrete internal rationale`);
      continue;
    }
    let value = api;
    for (const part of route.api.split('.')) value = value?.[part];
    assert.equal(typeof value, 'function', `${call} has no public ${route.api} method`);
  }
  for (const [topic, routes] of Object.entries(protocolSurface.topics)) {
    assert.equal(
      typeof api[routes.subscribe],
      'function',
      `${topic} has no typed subscription route`,
    );
    assert.equal(
      typeof api[routes.unsubscribe],
      'function',
      `${topic} has no typed unsubscription route`,
    );
  }
});

test('every fixed public facade request is classified with its Rust host capability', () => {
  const source = fs.readFileSync(new URL('../dist/index.js', import.meta.url), 'utf8');
  const calls = new Set(
    [...source.matchAll(/(?:session\.call|done)\('([a-z_]+)'/g)].map((match) => match[1]),
  );
  calls.delete('event_subscribe');
  calls.delete('event_unsubscribe');
  for (const call of calls)
    assert.doesNotThrow(() => requestCapability(call), `${call} is unclassified`);
  assert.equal(requestCapability('container_attach_terminal'), 'containers:attach');
  assert.equal(requestCapability('terminal_read_pane'), 'terminals:output');
  assert.equal(requestCapability('pane_semantic_action'), 'panes:semantic-control');
  assert.equal(requestCapability('filesystem_remove_observed'), 'filesystem:write');
  assert.throws(() => requestCapability('future_unclassified_call'), /unclassified/);
});

test('terminal occupant switching validates and preserves the exact CAS wire shape', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  for (const [generation, target] of [
    [-1, { kind: 'terminal' }],
    [Number.MAX_SAFE_INTEGER + 1, { kind: 'terminal' }],
    [0, { kind: 'terminal', extra: true }],
    [0, { kind: 'surface', extension: '', provider: 'main' }],
    [0, { kind: 'surface', extension: 'demo', provider: '' }],
    [0, { kind: 'unknown' }],
  ])
    assert.throws(() => api.terminal.switchOccupant('pane-1', generation, target));
  const surface = { kind: 'surface', extension: 'demo', provider: 'main' };
  const first = api.terminal.switchOccupant('pane-1', 7, surface);
  assert.deepEqual((await next()).payload, {
    call: 'terminal_switch_occupant',
    with: { slot: 'pane-1', generation: 7, target: surface },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await first;
  const second = api.terminal.switchOccupant('pane-1', 8, { kind: 'terminal' });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_switch_occupant',
    with: { slot: 'pane-1', generation: 8, target: { kind: 'terminal' } },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await second;
  const observed = api.terminal.switchOccupantObserved('pane-1', 9, 12, surface);
  assert.deepEqual((await next()).payload, {
    call: 'terminal_switch_occupant_observed',
    with: { slot: 'pane-1', generation: 9, revision: 12, target: surface },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await observed;
  assert.throws(
    () => api.terminal.switchOccupantObserved('pane-1', 9, -1, surface),
    /generation and revision/,
  );
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('occupant switch wait arms before authority and verifies exact provider identity', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const target = { kind: 'surface', extension: 'manager', provider: 'main' };
  const pending = api.terminal.switchOccupantAndWait('pane-1', 7, 11, target, { timeoutMs: 1_000 });
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'pane-changes' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual((await next()).payload, {
    call: 'terminal_switch_occupant_observed',
    with: {
      slot: 'pane-1',
      generation: 7,
      revision: 11,
      target,
    },
  });
  stage.host.write(
    encode({
      channel: 22,
      kind: KIND.event,
      payload: {
        snapshot: 'pane_changes',
        of: {
          slot: 'pane-1',
          kind: 'surface',
          generation: 8,
          revision: 12,
          coalesced: 0,
        },
      },
    }),
  );
  assert.equal((await next()).kind, KIND.credit);
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual((await next()).payload, { call: 'pane_list' });
  const pane = {
    slot: 'pane-1',
    generation: 8,
    revision: 12,
    kind: 'surface',
    provider: {
      extension: 'manager',
      provider: 'main',
    },
    tab: 'tab-1',
    title: 'Manager',
    focused: true,
  };
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: { reply: 'panes', with: { panes: [pane], truncated: false } },
    }),
  );
  assert.deepEqual((await next()).payload, {
    call: 'event_unsubscribe',
    with: { topic: 'pane-changes' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual(await pending, { changed: true, pane });
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('occupant switch wait rejects a mismatched resulting provider and still disposes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const pending = api.terminal.switchOccupantAndWait('pane-1', 7, 11, {
    kind: 'surface',
    extension: 'manager',
    provider: 'main',
  });
  await next();
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await next();
  stage.host.write(
    encode({
      channel: 22,
      kind: KIND.event,
      payload: {
        snapshot: 'pane_changes',
        of: {
          slot: 'pane-1',
          kind: 'surface',
          generation: 8,
          revision: 12,
          coalesced: 0,
        },
      },
    }),
  );
  await next();
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await next();
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'panes',
        with: {
          panes: [
            {
              slot: 'pane-1',
              generation: 8,
              revision: 12,
              kind: 'surface',
              provider: { extension: 'other', provider: 'main' },
              tab: 'tab-1',
              title: 'Other',
              focused: true,
            },
          ],
          truncated: false,
        },
      },
    }),
  );
  assert.equal((await next()).payload.call, 'event_unsubscribe');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await assert.rejects(pending, /without installing the requested occupant/);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('extension inventory and acquisition watchers use separate exact topics', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const inventory = [];
  const acquisitions = [];
  const openingInventory = api.watchExtensions((value) => inventory.push(value));
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'extensions' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const stopInventory = await openingInventory;
  const openingAcquisitions = api.watchExtensionAcquisitions((value) => acquisitions.push(value));
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'extension-acquisitions' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const stopAcquisitions = await openingAcquisitions;
  stage.host.write(
    encode({
      channel: 6,
      kind: KIND.event,
      payload: {
        snapshot: 'extensions',
        of: [{ name: 'manager', image_digest: 'sha256:a', status: 'duty' }],
      },
    }),
  );
  assert.equal((await next()).kind, KIND.credit);
  stage.host.write(
    encode({
      channel: 7,
      kind: KIND.event,
      payload: {
        snapshot: 'extension_acquisitions',
        of: { job: 'j', revision: 2, state: 'ready', coalesced: 4 },
      },
    }),
  );
  assert.equal((await next()).kind, KIND.credit);
  assert.equal(inventory[0][0].name, 'manager');
  assert.deepEqual(acquisitions[0], { job: 'j', revision: 2, state: 'ready', coalesced: 4 });
  const stoppingInventory = stopInventory();
  assert.deepEqual((await next()).payload.with, { topic: 'extensions' });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await stoppingInventory;
  const stoppingAcquisitions = stopAcquisitions();
  assert.deepEqual((await next()).payload.with, { topic: 'extension-acquisitions' });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await stoppingAcquisitions;
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('workspace lifecycle watcher uses its WorkspaceRead-gated exact topic', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const changes = [];
  const opening = api.watchWorkspaceLifecycle((value) => changes.push(value));
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'workspace-lifecycle' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const stop = await opening;
  stage.host.write(
    encode({
      channel: 8,
      kind: KIND.event,
      payload: {
        snapshot: 'workspace_lifecycle',
        of: {
          workspace: 'target',
          action: 'update',
          revision: 9,
          coalesced: 2,
        },
      },
    }),
  );
  assert.equal((await next()).kind, KIND.credit);
  assert.deepEqual(changes, [{ workspace: 'target', action: 'update', revision: 9, coalesced: 2 }]);
  const stopping = stop();
  assert.deepEqual((await next()).payload, {
    call: 'event_unsubscribe',
    with: { topic: 'workspace-lifecycle' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await stopping;
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('workspace input watcher uses its separate grant topic, returns credit, and disposes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const batches = [];
  const opening = api.watchWorkspaceEvents((value) => batches.push(value));
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'workspace-events' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const stop = await opening;
  stage.host.write(
    encode({
      channel: 9,
      kind: KIND.event,
      payload: {
        snapshot: 'workspace_events',
        of: {
          events: [
            { event: 'key', key: 'a', modifiers: [], pressed: true, slot: 'pane-2', generation: 7 },
            { event: 'focus', active: true, slot: 'pane-2', generation: 7 },
            {
              event: 'pointer',
              phase: 'press',
              slot: 'pane-2',
              generation: 7,
              x: 12.5,
              y: 8,
              button: 1,
              modifiers: ['shift'],
              delta_x: null,
              delta_y: null,
            },
          ],
          dropped: 3,
        },
      },
    }),
  );
  assert.equal((await next()).kind, KIND.credit);
  assert.equal(batches[0].dropped, 3);
  assert.equal(batches[0].events[0].slot, 'pane-2');
  assert.equal(batches[0].events[0].generation, 7);
  assert.equal(batches[0].events[1].slot, 'pane-2');
  const stopping = stop();
  assert.deepEqual((await next()).payload, {
    call: 'event_unsubscribe',
    with: { topic: 'workspace-events' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await stopping;
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('filesystem watcher preserves bounded completeness and coalescing metadata', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const seen = [];
  const opening = api.watchFilesystem((value) => seen.push(value));
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'filesystem' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const stop = await opening;
  const inventory = {
    journal: FILE_JOURNAL,
    entries: [{ path: 'src/main.ts', directory: false, size: 12, identity: 'v1:1:2:3:4:5:6:7' }],
    complete: false,
    coalesced: 4,
    revision: 19,
  };
  stage.host.write(
    encode({ channel: 17, kind: KIND.event, payload: { snapshot: 'filesystem', of: inventory } }),
  );
  assert.equal((await next()).kind, KIND.credit);
  assert.deepEqual(seen, [inventory]);
  const stopping = stop();
  assert.deepEqual((await next()).payload, {
    call: 'event_unsubscribe',
    with: { topic: 'filesystem' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await stopping;
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('real Unix filesystem snapshots reject journal replacement before delivery', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const seen = [];
  const opening = workspace(stage.session).watchFilesystem((inventory) => seen.push(inventory));
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'filesystem' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await opening;
  const inventory = (journal, revision) => ({
    journal,
    entries: [],
    complete: true,
    coalesced: 0,
    revision,
  });
  stage.host.write(
    encode({
      channel: 17,
      kind: KIND.event,
      payload: { snapshot: 'filesystem', of: inventory(FILE_JOURNAL, 9) },
    }),
  );
  assert.equal((await next()).kind, KIND.credit);
  assert.equal(seen.length, 1);

  stage.host.write(
    encode({
      channel: 17,
      kind: KIND.event,
      payload: { snapshot: 'filesystem', of: inventory(NEXT_FILE_JOURNAL, 0) },
    }),
  );
  const closed = await stage.session.closed;
  assert.match(closed.message, /journal changed within one host session/);
  assert.equal(seen.length, 1, 'the invalid replacement is never delivered');
  await assert.rejects(workspace(stage.session).info(), /closed|journal changed/);
  stage.host.destroy();
  stage.server.close();
});

test('real Unix filesystem snapshots reject a regressing revision', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const opening = workspace(stage.session).watchFilesystem(() => {});
  await next();
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await opening;
  for (const revision of [9, 8]) {
    stage.host.write(
      encode({
        channel: 18,
        kind: KIND.event,
        payload: {
          snapshot: 'filesystem',
          of: {
            journal: FILE_JOURNAL,
            entries: [],
            complete: true,
            coalesced: 0,
            revision,
          },
        },
      }),
    );
    if (revision === 9) assert.equal((await next()).kind, KIND.credit);
  }
  assert.match((await stage.session.closed).message, /revision moved backwards/);
  stage.host.destroy();
  stage.server.close();
});

test('real Unix filesystem snapshots reject malformed journal identity without credit', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const opening = workspace(stage.session).watchFilesystem(() => {
    assert.fail('malformed snapshot must not be delivered');
  });
  await next();
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await opening;
  stage.host.write(
    encode({
      channel: 19,
      kind: KIND.event,
      payload: {
        snapshot: 'filesystem',
        of: {
          journal: 'not-a-journal',
          entries: [],
          complete: true,
          coalesced: 0,
          revision: 0,
        },
      },
    }),
  );
  assert.match((await stage.session.closed).message, /32 hexadecimal characters/);
  stage.host.destroy();
  stage.server.close();
});

test('filesystem change watcher advances opaque pages and exposes truncation', async () => {
  const calls = [];
  const pages = [
    {
      journal: FILE_JOURNAL,
      changes: [
        {
          revision: 11,
          kind: 'modify',
          path: 'src/late.ts',
          entry: { path: 'src/late.ts', directory: false, size: 4, identity: 'v2' },
        },
      ],
      next: 11,
      current: 12,
      more: true,
      truncated: false,
    },
    {
      journal: FILE_JOURNAL,
      changes: [{ revision: 12, kind: 'remove', path: 'src/gone.ts', entry: null }],
      next: 12,
      current: 12,
      more: false,
      truncated: false,
    },
  ];
  const api = workspace({
    granted: ['filesystem:read'],
    async call(name, payload) {
      calls.push([name, payload]);
      return {
        reply: 'file_changes',
        with: pages.shift() ?? {
          journal: FILE_JOURNAL,
          changes: [],
          next: 12,
          current: 12,
          more: false,
          truncated: false,
        },
      };
    },
    onEvent() {
      return () => {};
    },
  });
  const seen = [];
  const controller = new AbortController();
  const delivered = new Promise((resolve) => {
    void api.files.watchChanges(
      (page) => {
        seen.push(...page.changes.map((change) => change.path));
        if (seen.length === 2) {
          controller.abort();
          resolve();
        }
      },
      {
        cursor: { journal: FILE_JOURNAL, revision: 10 },
        pageSize: 1,
        pollMs: 1000,
        signal: controller.signal,
      },
    );
  });
  await delivered;
  assert.deepEqual(seen, ['src/late.ts', 'src/gone.ts']);
  assert.deepEqual(calls.slice(0, 2), [
    ['filesystem_changes', { observed: FILE_JOURNAL, after: 10, limit: 1 }],
    ['filesystem_changes', { observed: FILE_JOURNAL, after: 11, limit: 1 }],
  ]);
});

test('filesystem change watcher exposes a cursor advance with no visible paths', async () => {
  const calls = [];
  const page = {
    journal: FILE_JOURNAL,
    changes: [],
    next: 19,
    current: 19,
    more: false,
    truncated: false,
  };
  const api = workspace({
    granted: ['filesystem:read'],
    async call(name, payload) {
      calls.push([name, payload]);
      return { reply: 'file_changes', with: page };
    },
    onEvent() {
      return () => {};
    },
  });
  const controller = new AbortController();
  let delivered;
  const seen = new Promise((resolve) => {
    delivered = resolve;
  });
  const stop = await api.files.watchChanges(
    (value) => {
      controller.abort();
      delivered(value);
    },
    { cursor: { journal: FILE_JOURNAL, revision: 7 }, pollMs: 1_000, signal: controller.signal },
  );
  assert.deepEqual(await seen, page);
  await stop();
  assert.deepEqual(calls, [
    ['filesystem_changes', { observed: FILE_JOURNAL, after: 7, limit: 256 }],
  ]);
});

test('latest filesystem work bounds the uncommitted changes retained across supersession', async () => {
  let revision = 0;
  const calls = [];
  const api = workspace({
    granted: ['filesystem:read'],
    async call(name, payload) {
      calls.push([name, payload]);
      revision += 1;
      return {
        reply: 'file_changes',
        with: {
          journal: FILE_JOURNAL,
          changes: [{ revision, kind: 'modify', path: `src/${revision}.ts`, entry: null }],
          next: revision,
          current: revision,
          more: false,
          truncated: false,
        },
      };
    },
    onEvent() {
      return () => {};
    },
  });
  const stop = await api.files.watchLatestChanges(
    (_page, signal) =>
      new Promise((resolve) => signal.addEventListener('abort', resolve, { once: true })),
    {
      cursor: { journal: FILE_JOURNAL, revision: 0 },
      pollMs: 1,
      maxBufferedChanges: 1,
    },
  );
  await assert.rejects(stop.done, /latest-change buffer exceeded 1 changes/);
  assert.deepEqual(
    calls.slice(0, 2).map(([, { after }]) => after),
    [0, 1],
  );
  await assert.rejects(
    api.files.watchLatestChanges(() => {}, {
      cursor: { journal: FILE_JOURNAL, revision: 0 },
      maxBufferedChanges: 0,
    }),
    /between 1 and 65536 changes/,
  );
  assert.equal(calls.length, 2, 'an invalid local bound must not be framed');
});

test('real Unix change-page iteration applies backpressure and surfaces overflow, malformed cursors, and abort', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const files = workspace(stage.session).files;
  const stream = files.changePages({
    cursor: { journal: FILE_JOURNAL, revision: 10 },
    pageSize: 1,
    pollMs: 10,
  });

  const first = stream.next();
  assert.deepEqual((await next()).payload, {
    call: 'filesystem_changes',
    with: { observed: FILE_JOURNAL, after: 10, limit: 1 },
  });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'file_changes',
        with: {
          journal: FILE_JOURNAL,
          changes: [
            {
              revision: 11,
              kind: 'modify',
              path: 'src/test.ts',
              entry: { path: 'src/test.ts', directory: false, size: 4, identity: 'file-v2' },
            },
          ],
          next: 11,
          current: 12,
          more: true,
          truncated: false,
        },
      },
    }),
  );
  assert.equal((await first).value.next, 11);
  await new Promise((resolve) => setTimeout(resolve, 20));

  const overflow = stream.next();
  assert.deepEqual((await next()).payload.with, { observed: FILE_JOURNAL, after: 11, limit: 1 });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'file_changes',
        with: {
          journal: NEXT_FILE_JOURNAL,
          changes: [],
          next: 20,
          current: 20,
          more: false,
          truncated: true,
        },
      },
    }),
  );
  assert.equal((await overflow).value.truncated, true);

  const malformed = stream.next();
  assert.deepEqual((await next()).payload.with, {
    observed: NEXT_FILE_JOURNAL,
    after: 20,
    limit: 1,
  });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'file_changes',
        with: {
          journal: FILE_JOURNAL,
          changes: [{ revision: 20, kind: 'remove', path: 'src/stale.ts', entry: null }],
          next: 20,
          current: 20,
          more: false,
          truncated: false,
        },
      },
    }),
  );
  await assert.rejects(malformed, /inconsistent filesystem change page/);

  const controller = new AbortController();
  const idle = files.changePages({
    cursor: { journal: NEXT_FILE_JOURNAL, revision: 20 },
    pollMs: 10_000,
    signal: controller.signal,
  });
  const waiting = idle.next();
  assert.deepEqual((await next()).payload.with, {
    observed: NEXT_FILE_JOURNAL,
    after: 20,
    limit: 256,
  });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'file_changes',
        with: {
          journal: NEXT_FILE_JOURNAL,
          changes: [],
          next: 20,
          current: 20,
          more: false,
          truncated: false,
        },
      },
    }),
  );
  await new Promise((resolve) => setTimeout(resolve, 20));
  controller.abort('diagnostics stopped');
  await assert.rejects(waiting, (error) => error.name === 'AbortError');

  const disconnected = files
    .changePages({ cursor: { journal: FILE_JOURNAL, revision: 20 }, pollMs: 1 })
    .next();
  assert.deepEqual((await next()).payload.with, { observed: FILE_JOURNAL, after: 20, limit: 256 });
  stage.host.destroy();
  await assert.rejects(disconnected);
  stage.session.close();
  stage.server.close();
});

test('callback filesystem watcher reports listener failure through its stop handle', async () => {
  const api = workspace({
    granted: ['filesystem:read'],
    async call() {
      return {
        reply: 'file_changes',
        with: {
          journal: FILE_JOURNAL,
          changes: [{ revision: 1, kind: 'remove', path: 'stale.ts', entry: null }],
          next: 1,
          current: 1,
          more: false,
          truncated: false,
        },
      };
    },
    onEvent() {
      return () => {};
    },
  });
  const failure = new Error('test scheduler failed');
  let reached;
  const delivered = new Promise((resolve) => {
    reached = resolve;
  });
  const stop = await api.files.watchChanges(
    () => {
      reached();
      throw failure;
    },
    { cursor: { journal: FILE_JOURNAL, revision: 0 }, pollMs: 10_000 },
  );
  await delivered;
  await assert.rejects(stop.done, (error) => error === failure);
  await assert.rejects(stop(), (error) => error === failure);
});

test('execution watcher uses exact topic and returns credit after delivery', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const seen = [];
  const opening = api.watchExecutions((value) => seen.push(value));
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'executions' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const stop = await opening;
  const catalogue = {
    executions: [
      {
        id: 'e1',
        container_id: 'c1',
        running: false,
        exit_code: 0,
        pid: 1,
        command: ['true'],
        user: 'root',
      },
    ],
    truncated: false,
  };
  stage.host.write(
    encode({ channel: 9, kind: KIND.event, payload: { snapshot: 'executions', of: catalogue } }),
  );
  assert.equal((await next()).kind, KIND.credit);
  assert.deepEqual(seen, [catalogue]);
  const stopping = stop();
  assert.deepEqual((await next()).payload.with, { topic: 'executions' });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await stopping;
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('container watcher uses existing snapshot topic and returns credit after delivery', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const seen = [];
  const opening = api.watchContainers((value) => seen.push(value));
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'containers' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const stop = await opening;
  const containers = [
    {
      id: 'a'.repeat(64),
      name: 'worker_2.prod',
      image: 'alpine:3.20',
      state: 'running',
      created: 1,
    },
  ];
  stage.host.write(
    encode({ channel: 10, kind: KIND.event, payload: { snapshot: 'containers', of: containers } }),
  );
  assert.equal((await next()).kind, KIND.credit);
  assert.deepEqual(seen, [containers]);
  assert.equal(seen[0][0].id, 'a'.repeat(64), 'rename observation preserves immutable identity');
  const stopping = stop();
  assert.deepEqual((await next()).payload.with, { topic: 'containers' });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await stopping;
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('all bounded inventory snapshots have typed watchers with independent disposal and credit', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const seen = { images: [], volumes: [], networks: [], terminal: [] };
  const definitions = [
    [
      'watchImages',
      'images',
      'images',
      [{ id: 'sha256:a', reference: 'alpine:3.20', size: 42, created: 7 }],
    ],
    [
      'watchVolumes',
      'volumes',
      'volumes',
      [{ name: 'cache', driver: 'local', generation: 'a'.repeat(32) }],
    ],
    [
      'watchNetworks',
      'networks',
      'networks',
      [{ id: 'b'.repeat(32), name: 'dev', driver: 'bridge', scope: 'local', kind: 'custom' }],
    ],
    [
      'watchTerminal',
      'terminal',
      'terminal',
      [{ id: 'tab-1', title: 'Shell', pinned: false, panes: [] }],
    ],
  ];
  for (const [method, topic, snapshot, value] of definitions) {
    const opening = api[method]((payload) => seen[topic].push(payload));
    assert.deepEqual((await next()).payload, { call: 'event_subscribe', with: { topic } });
    stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
    const stop = await opening;
    const inventory =
      snapshot === 'images'
        ? { images: value, truncated: false }
        : snapshot === 'volumes'
          ? { volumes: value, truncated: false }
          : snapshot === 'networks'
            ? { networks: value, truncated: false }
            : value;
    stage.host.write(
      encode({ channel: 20, kind: KIND.event, payload: { snapshot, of: inventory } }),
    );
    const credit = await next();
    assert.equal(credit.kind, KIND.credit);
    assert.equal(credit.channel, 20);
    assert.deepEqual(seen[topic], [value]);
    const stopping = stop();
    assert.deepEqual((await next()).payload, { call: 'event_unsubscribe', with: { topic } });
    stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
    await stopping;
  }
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('image pull jobs and progress watcher preserve exact typed wire shapes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const opening = api.watchImagePulls(() => {});
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'image-pulls' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const stop = await opening;
  const operations = [
    api.images.startPull('alpine:3.20'),
    api.images.pullStatus('7'),
    api.images.cancelPull('7'),
  ];
  assert.deepEqual((await next()).payload, {
    call: 'image_pull_start',
    with: { reference: 'alpine:3.20' },
  });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: { reply: 'image_pull_job', with: { job: '7' } },
    }),
  );
  assert.deepEqual((await next()).payload, { call: 'image_pull_status', with: { job: '7' } });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'image_pull',
        with: { job: '7', reference: 'alpine:3.20', revision: 1, state: 'pulling' },
      },
    }),
  );
  assert.deepEqual((await next()).payload, { call: 'image_pull_cancel', with: { job: '7' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await Promise.all(operations);
  const stopping = stop();
  assert.deepEqual((await next()).payload.with, { topic: 'image-pulls' });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await stopping;
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('image pull rejects invalid bounds and pre-aborted signals before host access', async () => {
  let calls = 0;
  const api = workspace({
    async call() {
      calls += 1;
      throw new Error('unexpected host access');
    },
    onEvent() {
      return () => {};
    },
  });
  for (const timeoutMs of [0, -1, NaN, Infinity, 1.5, 86_400_001]) {
    await assert.rejects(api.images.pull('alpine:3.20', { timeoutMs }), RangeError);
  }
  const controller = new AbortController();
  controller.abort(new Error('stop'));
  await assert.rejects(
    api.images.pull('alpine:3.20', { signal: controller.signal }),
    /stop|abort/i,
  );
  assert.equal(calls, 0);
});

test('image pull completes through start and status without cancelling', async () => {
  const calls = [];
  const image = {
    id: `sha256:${'a'.repeat(64)}`,
    reference: 'docker.io/library/alpine:3.20',
    references: ['docker.io/library/alpine:3.20'],
    size: 1,
    created: 1,
  };
  const api = workspace({
    async call(name) {
      calls.push(name);
      if (name === 'image_pull_start') return { reply: 'image_pull_job', with: { job: 'job' } };
      return {
        reply: 'image_pull',
        with: { job: 'job', reference: image.reference, revision: 2, state: 'complete', image },
      };
    },
    onEvent() {
      return () => {};
    },
  });
  assert.deepEqual(await api.images.pull('alpine:3.20'), image);
  assert.deepEqual(calls, ['image_pull_start', 'image_pull_status']);
});

test('streaming execution applies callback backpressure and cancels callback failure', async () => {
  const stage = await pair();
  await frames(stage.host)();
  const api = workspace(stage.session);
  const pages = [
    {
      entries: [{ sequence: 1, timestamp_ms: 1, stream: 'stdout', bytes: [97] }],
      next: 1,
      more: true,
      eof: false,
      gap: false,
    },
    {
      entries: [{ sequence: 2, timestamp_ms: 2, stream: 'stderr', bytes: [98] }],
      next: 2,
      more: false,
      eof: true,
      gap: false,
    },
  ];
  const calls = [];
  api.containers.exec = async () => 'e'.repeat(32);
  api.containers.executionOutputPages = async function* () {
    yield* pages;
  };
  api.containers.execution = async (id) => ({ id, running: false, exit_code: 0, pid: null });
  api.containers.cancelExecution = async (...arguments_) => {
    calls.push(['cancel', ...arguments_]);
  };
  await assert.rejects(
    api.containers.execStreaming('c'.repeat(64), 7, { command: ['psql'] }, async (page) => {
      calls.push(['page', page.next]);
      await Promise.resolve();
      if (page.next === 2) throw new Error('consumer refused output');
    }),
    (error) =>
      error.name === 'ExecutionOperationError' &&
      error.executionId === 'e'.repeat(32) &&
      error.phase === 'output',
  );
  assert.deepEqual(calls, [
    ['page', 1],
    ['page', 2],
    ['cancel', 'e'.repeat(32), { signal: 'SIGTERM', timeoutMs: 1_000 }],
  ]);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('mutable yielded pages cannot corrupt process or output continuation', async () => {
  const api = workspace({
    granted: ['containers:read'],
    call() {
      throw new Error('test replaces the paged methods');
    },
    onEvent() {
      return () => {};
    },
  });
  let processCalls = 0;
  api.containers.processes = async () => {
    processCalls += 1;
    return processCalls === 1
      ? { processes: [], snapshot: 'snapshot-1', next: 1, more: true }
      : { processes: [], snapshot: 'snapshot-1', next: null, more: false };
  };
  const processes = api.containers.processPages('c'.repeat(64), { limit: 1 });
  const processPage = (await processes.next()).value;
  processPage.more = false;
  processPage.next = null;
  assert.equal((await processes.next()).done, false);
  assert.equal(processCalls, 2);

  let outputCalls = 0;
  api.containers.executionOutput = async () => {
    outputCalls += 1;
    return outputCalls === 1
      ? {
          entries: [{ sequence: 1, timestamp_ms: 1, stream: 'stdout', bytes: [1] }],
          next: 1,
          more: true,
          eof: false,
          gap: false,
        }
      : { entries: [], next: 1, more: false, eof: true, gap: false };
  };
  const output = api.containers.executionOutputPages('e'.repeat(32), {
    limit: 1,
    pollIntervalMs: 10,
  });
  const outputPage = (await output.next()).value;
  outputPage.next = 0;
  outputPage.more = false;
  outputPage.eof = true;
  assert.equal((await output.next()).done, false);
  assert.equal(outputCalls, 2);
});

test('pre-aborted streaming execution never starts a container command', async () => {
  const stage = await pair();
  await frames(stage.host)();
  const api = workspace(stage.session);
  const controller = new AbortController();
  controller.abort(new Error('caller stopped'));
  let starts = 0;
  api.containers.exec = async () => {
    starts += 1;
    return 'e'.repeat(32);
  };
  await assert.rejects(
    api.containers.execStreaming(
      'c'.repeat(64),
      7,
      { command: ['psql'], signal: controller.signal },
      () => {},
    ),
    (error) => error.name === 'AbortError' && error.cause === controller.signal.reason,
  );
  assert.equal(starts, 0);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('real Unix streaming execution rejects unusable cleanup configuration before starting', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const container = 'c'.repeat(64);
  for (const options of [
    { command: ['psql'], pageLimit: 17 },
    { command: ['psql'], pollIntervalMs: 9 },
    { command: ['psql'], cancelSignal: 'SIG TERM' },
    { command: ['psql'], cancelTimeoutMs: 0 },
  ]) {
    await assert.rejects(api.containers.execStreaming(container, 7, options, () => {}));
  }

  const listing = api.containers.list();
  const frame = await next();
  assert.equal(
    frame.payload.call,
    'container_list',
    'invalid static options must emit no execution mutation frame',
  );
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'containers', with: [] } }),
  );
  assert.deepEqual(await listing, []);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('abort while streaming execution starts cancels its returned identity before I/O', async () => {
  const stage = await pair();
  await frames(stage.host)();
  const api = workspace(stage.session);
  const controller = new AbortController();
  const calls = [];
  api.containers.exec = async () => {
    controller.abort(new Error('caller stopped during start'));
    return 'e'.repeat(32);
  };
  api.containers.executionOutputPages = async function* () {
    calls.push('output');
  };
  api.containers.cancelExecution = async (...arguments_) => {
    calls.push(['cancel', ...arguments_]);
  };
  await assert.rejects(
    api.containers.execStreaming(
      'c'.repeat(64),
      7,
      { command: ['psql'], signal: controller.signal },
      () => {},
    ),
    (error) =>
      error instanceof ExecutionOperationError &&
      error.phase === 'output' &&
      error.cause?.cause === controller.signal.reason,
  );
  assert.deepEqual(calls, [['cancel', 'e'.repeat(32), { signal: 'SIGTERM', timeoutMs: 1_000 }]]);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('streaming execution owns stdin failure while concurrently draining output', async () => {
  const stage = await pair();
  await frames(stage.host)();
  const api = workspace(stage.session);
  const calls = [];
  api.containers.exec = async (_id, _generation, options) => {
    calls.push(['exec', options.stdin]);
    return 'e'.repeat(32);
  };
  api.containers.pipeExecutionStdin = async (_id, source, options) => {
    calls.push(['input', [...source], options.close]);
    throw new Error('stdin producer failed');
  };
  api.containers.executionOutputPages = async function* () {
    calls.push(['output']);
  };
  api.containers.cancelExecution = async (...arguments_) => {
    calls.push(['cancel', ...arguments_]);
  };
  await assert.rejects(
    api.containers.execStreaming(
      'c'.repeat(64),
      7,
      { command: ['psql'], input: ['select 1;\n'] },
      () => {},
    ),
    (error) =>
      error instanceof ExecutionOperationError &&
      error.phase === 'input' &&
      error.executionId === 'e'.repeat(32),
  );
  assert.deepEqual(calls, [
    ['exec', true],
    ['output'],
    ['input', ['select 1;\n'], true],
    ['cancel', 'e'.repeat(32), { signal: 'SIGTERM', timeoutMs: 1_000 }],
  ]);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('text execution preserves split UTF-8 and cancels aggregate overflow', async () => {
  const stage = await pair();
  await frames(stage.host)();
  const api = workspace(stage.session);
  let cancelled = 0;
  api.containers.execStreaming = async (_id, _generation, _options, onPage) => {
    try {
      await onPage({
        entries: [
          { sequence: 1, timestamp_ms: 1, stream: 'stdout', bytes: [0xe2, 0x82] },
          { sequence: 2, timestamp_ms: 2, stream: 'stdout', bytes: [0xac] },
          { sequence: 3, timestamp_ms: 3, stream: 'stderr', bytes: [101, 114, 114] },
        ],
        next: 3,
        more: false,
        eof: true,
        gap: false,
      });
    } catch (error) {
      cancelled += 1;
      throw new ExecutionOperationError('e'.repeat(32), 'output', error);
    }
    return { executionId: 'e'.repeat(32), execution: { running: false, exit_code: 0 } };
  };
  assert.deepEqual(
    await api.containers.execText('c'.repeat(64), 1, { command: ['query'], maxBytes: 6 }),
    {
      executionId: 'e'.repeat(32),
      execution: { running: false, exit_code: 0 },
      stdout: '€',
      stderr: 'err',
    },
  );
  await assert.rejects(
    api.containers.execText('c'.repeat(64), 1, { command: ['query'], maxBytes: 5 }),
    (error) => error instanceof ExecutionOperationError && /5 byte limit/.test(error.cause.message),
  );
  assert.equal(cancelled, 1);
  api.containers.execStreaming = async (_id, _generation, _options, onPage) => {
    try {
      await onPage({
        entries: [{ sequence: 1, timestamp_ms: 1, stream: 'stdout', bytes: [0xff] }],
        next: 1,
        more: false,
        eof: true,
        gap: false,
      });
    } catch (error) {
      cancelled += 1;
      throw new ExecutionOperationError('e'.repeat(32), 'output', error);
    }
  };
  await assert.rejects(
    api.containers.execText('c'.repeat(64), 1, { command: ['query'], maxBytes: 6 }),
    (error) => error instanceof ExecutionOperationError && error.cause instanceof TypeError,
  );
  assert.equal(cancelled, 2, 'malformed text cancels instead of corrupting a query result');
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('line execution frames split UTF-8 records with bounded backpressure', async () => {
  const stage = await pair();
  await frames(stage.host)();
  const api = workspace(stage.session);
  const values = [];
  const stderr = [];
  let callbacks = 0;
  api.containers.execStreaming = async (_id, _generation, options, onPage) => {
    assert.equal(options.maxLineBytes, undefined, 'client-only options do not reach execution');
    await onPage({
      entries: [
        { sequence: 1, timestamp_ms: 1, stream: 'stdout', bytes: [...Buffer.from('{"name":"caf')] },
        { sequence: 2, timestamp_ms: 2, stream: 'stderr', bytes: [...Buffer.from('notice')] },
      ],
      next: 2,
      more: true,
      eof: false,
      gap: false,
    });
    assert.equal(callbacks, 0);
    await onPage({
      entries: [
        {
          sequence: 3,
          timestamp_ms: 3,
          stream: 'stdout',
          bytes: [...Buffer.from('é"}\r\n{"id":2}')],
        },
      ],
      next: 3,
      more: false,
      eof: true,
      gap: false,
    });
    return { executionId: 'e'.repeat(32), execution: { running: false, exit_code: 0 } };
  };
  const result = await api.containers.execLines(
    'c'.repeat(64),
    1,
    { command: ['psql'], maxLineBytes: 32, onStderr: (text) => stderr.push(text) },
    async (value, line) => {
      await Promise.resolve();
      callbacks += 1;
      values.push([line, value]);
    },
  );
  assert.deepEqual(values, [
    [1, '{"name":"café"}'],
    [2, '{"id":2}'],
  ]);
  assert.deepEqual(stderr, ['notice']);
  assert.equal(result.lines, 2);
  api.containers.execStreaming = async (_id, _generation, _options, onPage) => {
    try {
      await onPage({
        entries: [
          { sequence: 1, timestamp_ms: 1, stream: 'stdout', bytes: [...Buffer.from('12345')] },
        ],
        next: 1,
        more: false,
        eof: true,
        gap: false,
      });
    } catch (error) {
      throw new ExecutionOperationError('e'.repeat(32), 'output', error);
    }
  };
  await assert.rejects(
    api.containers.execLines('c'.repeat(64), 1, { command: ['query'], maxLineBytes: 4 }, () => {}),
    (error) => error instanceof ExecutionOperationError && /4 byte limit/.test(error.cause.message),
  );
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('JSON lines execution decodes values through the generic line stream', async () => {
  const stage = await pair();
  await frames(stage.host)();
  const api = workspace(stage.session);
  const values = [];
  api.containers.execLines = async (_id, _generation, options, onLine) => {
    assert.equal(options.maxLineBytes, 32);
    await onLine('{"ready":true}', 1);
    return { executionId: 'e'.repeat(32), execution: { running: false, exit_code: 0 }, lines: 1 };
  };
  const result = await api.containers.execJsonLines(
    'c'.repeat(64),
    1,
    { command: ['query'], maxLineBytes: 32 },
    (value, line) => values.push([line, value]),
  );
  assert.deepEqual(values, [[1, { ready: true }]]);
  assert.equal(result.lines, 1);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('image pull preserves host failure when best-effort cancellation also fails', async () => {
  const calls = [];
  const api = workspace({
    async call(name) {
      calls.push(name);
      if (name === 'image_pull_start') return { reply: 'image_pull_job', with: { job: 'job' } };
      if (name === 'image_pull_status')
        return {
          reply: 'image_pull',
          with: {
            job: 'job',
            reference: 'alpine:3.20',
            revision: 2,
            state: 'failed',
            error: 'registry refused credentials',
          },
        };
      throw new Error('cancel transport failed');
    },
    onEvent() {
      return () => {};
    },
  });
  await assert.rejects(api.images.pull('alpine:3.20'), /registry refused credentials/);
  assert.deepEqual(calls, ['image_pull_start', 'image_pull_status', 'image_pull_cancel']);
});

test('image pull timeout and AbortSignal cancel the owned host job and preserve the primary error', async () => {
  for (const mode of ['timeout', 'abort']) {
    const calls = [];
    const controller = new AbortController();
    const api = workspace({
      async call(name) {
        calls.push(name);
        if (name === 'image_pull_start') return { reply: 'image_pull_job', with: { job: 'job' } };
        if (name === 'image_pull_status') {
          if (mode === 'abort') controller.abort(new Error('caller stopped pull'));
          return {
            reply: 'image_pull',
            with: { job: 'job', reference: 'alpine:3.20', revision: 1, state: 'pulling' },
          };
        }
        throw new Error('cancel failed too');
      },
      onEvent() {
        return () => {};
      },
    });
    const operation = api.images.pull(
      'alpine:3.20',
      mode === 'timeout' ? { timeoutMs: 1 } : { timeoutMs: 1_000, signal: controller.signal },
    );
    await assert.rejects(
      operation,
      mode === 'timeout' ? /timed out/ : /caller stopped pull|abort/i,
    );
    assert.equal(calls.at(-1), 'image_pull_cancel');
  }
});

test('extension acquisition preserves job revision and explicit grant identity', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const digest = `sha256:${'a'.repeat(64)}`;
  const imageGrant = { read: [], use: [], pull: [], remove: [], prune_all_unused: false };
  const operations = [
    api.extensions.startAcquisition('registry/example:1'),
    api.extensions.acquisition('job-1'),
    api.extensions.cancelAcquisition('job-1', 7),
    api.extensions.install(
      'job-1',
      7,
      digest,
      ['interface:render', 'containers:attach'],
      { selectors: [{ name: 'database' }], create: false },
      imageGrant,
      { selectors: [{ name: 'database' }], create: false },
      { selectors: [{ name: 'data' }], create: false },
      {
        read: [{ subtree: 'src' }],
        write: [{ exact: 'settings.json' }],
        create: [],
        delete: [],
        rename: [],
      },
      { read: [{ workspace: 'dev', name: 'PGPASSWORD' }], write: [] },
    ),
    api.extensions.update('job-2', 8, digest, ['containers:read'], {
      selectors: [{ all: true }],
      create: true,
    }),
  ];
  const calls = [];
  for (let index = 0; index < operations.length; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls, [
    { call: 'extension_acquisition_start', with: { reference: 'registry/example:1' } },
    { call: 'extension_acquisition_status', with: { job: 'job-1' } },
    { call: 'extension_acquisition_cancel', with: { job: 'job-1', revision: 7 } },
    {
      call: 'extension_install',
      with: {
        job: 'job-1',
        revision: 7,
        image_digest: digest,
        granted: ['interface:render', 'containers:attach'],
        containers: { selectors: [{ name: 'database' }], create: false },
        images: imageGrant,
        networks: { selectors: [{ name: 'database' }], create: false },
        volumes: { selectors: [{ name: 'data' }], create: false },
        filesystem: {
          read: [{ subtree: 'src' }],
          write: [{ exact: 'settings.json' }],
          create: [],
          delete: [],
          rename: [],
        },
        workspace_environment: { read: [{ workspace: 'dev', name: 'PGPASSWORD' }], write: [] },
      },
    },
    {
      call: 'extension_update',
      with: {
        job: 'job-2',
        revision: 8,
        image_digest: digest,
        granted: ['containers:read'],
        containers: { selectors: [{ all: true }], create: true },
        images: imageGrant,
        networks: { selectors: [], create: false },
        volumes: { selectors: [], create: false },
        filesystem: { read: [], write: [], create: [], delete: [], rename: [] },
        workspace_environment: { read: [], write: [] },
      },
    },
  ]);
  const summary = { name: 'example', image_digest: 'sha256:abc', status: 'standby' };
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: { reply: 'extension_acquisition_job', with: { job: 'job-1' } },
    }),
  );
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'extension_acquisition',
        with: {
          job: 'job-1',
          reference: 'registry/example:1',
          revision: 7,
          state: 'inspecting',
          candidate: null,
          error: null,
        },
      },
    }),
  );
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  for (let index = 0; index < 2; index += 1)
    stage.host.write(
      encode({ channel: 2, kind: KIND.response, payload: { reply: 'extension', with: summary } }),
    );
  const results = await Promise.all(operations);
  assert.equal(results[0].job, 'job-1');
  assert.equal(results[1].revision, 7);
  assert.equal(results[2], undefined);
  assert.deepEqual(results.slice(3), [summary, summary]);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('extension acquisition wait ignores unchanged and other jobs, then reads authoritative advanced status', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const pending = api.extensions.waitForAcquisition('job-1', 7, { timeoutMs: 1_000 });
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'extension-acquisitions' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual((await next()).payload, {
    call: 'extension_acquisition_status',
    with: { job: 'job-1' },
  });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'extension_acquisition',
        with: {
          job: 'job-1',
          reference: 'registry/demo:1',
          revision: 7,
          state: 'inspecting',
          progress: null,
          candidate: null,
          error: null,
        },
      },
    }),
  );
  for (const change of [
    { job: 'job-1', revision: 7, state: 'pulling', coalesced: 0 },
    { job: 'job-2', revision: 8, state: 'ready', coalesced: 0 },
  ]) {
    stage.host.write(
      encode({
        channel: 21,
        kind: KIND.event,
        payload: { snapshot: 'extension_acquisitions', of: change },
      }),
    );
    assert.equal((await next()).kind, KIND.credit);
  }
  stage.host.write(
    encode({
      channel: 21,
      kind: KIND.event,
      payload: {
        snapshot: 'extension_acquisitions',
        of: {
          job: 'job-1',
          revision: 9,
          state: 'ready',
          coalesced: 2,
        },
      },
    }),
  );
  assert.deepEqual((await next()).payload, {
    call: 'extension_acquisition_status',
    with: { job: 'job-1' },
  });
  assert.equal((await next()).kind, KIND.credit);
  const status = {
    job: 'job-1',
    reference: 'registry/demo:1',
    revision: 9,
    state: 'reading-manifest',
    progress: null,
    candidate: null,
    error: null,
  };
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: { reply: 'extension_acquisition', with: status },
    }),
  );
  assert.deepEqual((await next()).payload, {
    call: 'event_unsubscribe',
    with: { topic: 'extension-acquisitions' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual(await pending, { changed: true, status });
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('extension acquisition wait times out with its exact cursor and disposes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  await assert.rejects(api.extensions.waitForAcquisition('', 1), /1..128 byte job identity/);
  const pending = api.extensions.waitForAcquisition('job-1', 7, { timeoutMs: 5 });
  assert.equal((await next()).payload.call, 'event_subscribe');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.equal((await next()).payload.call, 'extension_acquisition_status');
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'extension_acquisition',
        with: {
          job: 'job-1',
          reference: 'registry/demo:1',
          revision: 7,
          state: 'inspecting',
          progress: null,
          candidate: null,
          error: null,
        },
      },
    }),
  );
  assert.equal((await next()).payload.call, 'event_unsubscribe');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual(await pending, { changed: false, job: 'job-1', revision: 7 });
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('extension acquisition wait refuses a status overtaken by an in-flight invalidation', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const pending = api.extensions.waitForAcquisition('job-1', 4, { timeoutMs: 1_000 });
  assert.equal((await next()).payload.call, 'event_subscribe');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.equal((await next()).payload.call, 'extension_acquisition_status');
  stage.host.write(
    encode({
      channel: 21,
      kind: KIND.event,
      payload: {
        snapshot: 'extension_acquisitions',
        of: {
          job: 'job-1',
          revision: 6,
          state: 'ready',
          coalesced: 0,
        },
      },
    }),
  );
  assert.equal((await next()).kind, KIND.credit);
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'extension_acquisition',
        with: {
          job: 'job-1',
          reference: 'registry/demo:1',
          revision: 5,
          state: 'inspecting',
          progress: null,
          candidate: null,
          error: null,
        },
      },
    }),
  );
  assert.equal((await next()).payload.call, 'extension_acquisition_status');
  const status = {
    job: 'job-1',
    reference: 'registry/demo:1',
    revision: 6,
    state: 'reading-manifest',
    progress: null,
    candidate: null,
    error: null,
  };
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: { reply: 'extension_acquisition', with: status },
    }),
  );
  assert.equal((await next()).payload.call, 'event_unsubscribe');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual(await pending, { changed: true, status });
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('extension facade preserves exact read and control request shapes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const operations = [
    api.extensions.list(),
    api.extensions.catalogue(),
    api.extensions.inspect('top'),
    api.extensions.enable('top', `sha256:${'a'.repeat(64)}`),
    api.extensions.disable('top', `sha256:${'a'.repeat(64)}`),
    api.extensions.retry('top', `sha256:${'a'.repeat(64)}`),
    api.extensions.remove('top', `sha256:${'a'.repeat(64)}`),
  ];
  const calls = [];
  for (let index = 0; index < operations.length; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls, [
    { call: 'extension_list' },
    { call: 'extension_catalogue' },
    { call: 'extension_inspect', with: { name: 'top' } },
    { call: 'extension_enable', with: { name: 'top', image_digest: `sha256:${'a'.repeat(64)}` } },
    { call: 'extension_disable', with: { name: 'top', image_digest: `sha256:${'a'.repeat(64)}` } },
    { call: 'extension_retry', with: { name: 'top', image_digest: `sha256:${'a'.repeat(64)}` } },
    { call: 'extension_remove', with: { name: 'top', image_digest: `sha256:${'a'.repeat(64)}` } },
  ]);
  const summary = { name: 'top', image_digest: 'sha256:abc', status: 'standby' };
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'extensions', with: [summary] } }),
  );
  const catalogue = { entries: [], complete: true };
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: { reply: 'extension_catalogue', with: catalogue },
    }),
  );
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'extension', with: summary } }),
  );
  for (let index = 0; index < 4; index += 1)
    stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual(await Promise.all(operations), [
    [summary],
    catalogue,
    summary,
    undefined,
    undefined,
    undefined,
    undefined,
  ]);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('real Unix marketplace discovery refuses an incomplete catalogue with no continuation', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const extensions = workspace(stage.session).extensions;
  const pending = extensions.requireCompleteCatalogue();
  assert.deepEqual((await next()).payload, { call: 'extension_catalogue' });
  const entry = {
    id: 'postgres',
    title: 'Postgres',
    description: 'Database browser',
    version: '1.0.0',
    reference: 'registry.example/postgres@sha256:abc',
    publisher: 'Example publisher',
    source: 'https://example.invalid/catalogue',
    protocol: 1,
    architectures: ['amd64'],
  };
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'extension_catalogue',
        with: { entries: [entry], complete: false },
      },
    }),
  );
  await assert.rejects(
    pending,
    (error) => error instanceof IncompleteCatalogueError && error.received === 1,
  );

  const complete = extensions.requireCompleteCatalogue();
  assert.equal((await next()).payload.call, 'extension_catalogue');
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: { reply: 'extension_catalogue', with: { entries: [entry], complete: true } },
    }),
  );
  assert.deepEqual(await complete, { entries: [entry], complete: true });
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('real Unix marketplace rejects a catalogue beyond the host entry bound', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const pending = workspace(stage.session).extensions.catalogue();
  assert.equal((await next()).payload.call, 'extension_catalogue');
  const entries = Array.from({ length: 65 }, (_, index) => ({
    id: `entry-${index}`,
    title: `Entry ${index}`,
    description: 'Bounded extension',
    version: '1',
    reference: `registry.example/entry-${index}:1`,
    publisher: 'publisher',
    source: 'first-party',
    protocol: 1,
    architectures: ['amd64'],
  }));
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: { reply: 'extension_catalogue', with: { entries, complete: false } },
    }),
  );
  await assert.rejects(pending, /more than 64 extension catalogue entries/);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('extension enable wait arms inventory before authority and verifies the exact digest', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const digest = `sha256:${'a'.repeat(64)}`;
  const pending = api.extensions.enableAndWait('manager', digest, { timeoutMs: 1_000 });
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'extensions' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual((await next()).payload, {
    call: 'extension_enable',
    with: { name: 'manager', image_digest: digest },
  });
  stage.host.write(
    encode({
      channel: 23,
      kind: KIND.event,
      payload: {
        snapshot: 'extensions',
        of: [
          {
            name: 'manager',
            image_digest: digest,
            version: '1',
            status: 'duty',
            enabled: true,
            pane_providers: [],
          },
        ],
      },
    }),
  );
  assert.equal((await next()).kind, KIND.credit);
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.equal((await next()).payload.call, 'event_unsubscribe');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const result = await pending;
  assert.equal(result.changed, true);
  assert.equal(result.extension.image_digest, digest);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('extension disable wait arms inventory before authority and verifies durable standby', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const digest = `sha256:${'c'.repeat(64)}`;
  const pending = api.extensions.disableAndWait('manager', digest, { timeoutMs: 1_000 });
  assert.equal((await next()).payload.call, 'event_subscribe');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual((await next()).payload, {
    call: 'extension_disable',
    with: { name: 'manager', image_digest: digest },
  });
  stage.host.write(
    encode({
      channel: 24,
      kind: KIND.event,
      payload: {
        snapshot: 'extensions',
        of: [
          {
            name: 'manager',
            image_digest: digest,
            version: '1',
            status: 'standby',
            enabled: false,
            pane_providers: [],
          },
        ],
      },
    }),
  );
  assert.equal((await next()).kind, KIND.credit);
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.equal((await next()).payload.call, 'event_unsubscribe');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const result = await pending;
  assert.equal(result.changed, true);
  assert.equal(result.extension.enabled, false);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('extension remove wait arms before authority and proves exact digest absence with replacement disclosure', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const digest = `sha256:${'e'.repeat(64)}`;
  const replacementDigest = `sha256:${'f'.repeat(64)}`;
  const pending = api.extensions.removeAndWait('manager', digest, { timeoutMs: 1_000 });
  assert.equal((await next()).payload.call, 'event_subscribe');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual((await next()).payload, {
    call: 'extension_remove',
    with: { name: 'manager', image_digest: digest },
  });
  const replacement = {
    name: 'manager',
    image_digest: replacementDigest,
    version: '2',
    status: 'standby',
    enabled: false,
    pane_providers: [],
  };
  stage.host.write(
    encode({
      channel: 25,
      kind: KIND.event,
      payload: { snapshot: 'extensions', of: [replacement] },
    }),
  );
  assert.equal((await next()).kind, KIND.credit);
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.equal((await next()).payload.call, 'event_unsubscribe');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual(await pending, {
    changed: true,
    removed: { name: 'manager', image_digest: digest },
    replacement,
  });
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('extension retry wait arms before authority and requires exact durable duty', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const digest = `sha256:${'d'.repeat(64)}`;
  const pending = api.extensions.retryAndWait('manager', digest, { timeoutMs: 1_000 });
  assert.equal((await next()).payload.call, 'event_subscribe');
  const duty = {
    name: 'manager',
    image_digest: digest,
    version: '1',
    status: 'duty',
    enabled: true,
    pane_providers: [],
  };
  // A watcher's initial state predates restart authority and cannot prove that
  // an already-running extension was actually retried.
  stage.host.write(
    encode({
      channel: 26,
      kind: KIND.event,
      payload: { snapshot: 'extensions', of: [duty] },
    }),
  );
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const first = await next();
  const second = await next();
  const mutation = [first, second].find(({ payload }) => payload?.call === 'extension_retry');
  assert.deepEqual(mutation.payload, {
    call: 'extension_retry',
    with: { name: 'manager', image_digest: digest },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.equal(
    await Promise.race([
      pending.then(() => true),
      new Promise((resolve) => setImmediate(() => resolve(false))),
    ]),
    false,
  );
  stage.host.write(
    encode({
      channel: 26,
      kind: KIND.event,
      payload: { snapshot: 'extensions', of: [duty] },
    }),
  );
  assert.equal((await next()).kind, KIND.credit);
  assert.equal((await next()).payload.call, 'event_unsubscribe');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const result = await pending;
  assert.equal(result.changed, true);
  assert.equal(result.extension.status, 'duty');
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('volume and network facades preserve safe request shapes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const networkId = 'a'.repeat(32);
  const containerId = 'b'.repeat(64);
  assert.throws(() => api.networks.remove('private'), /complete immutable ID/);
  assert.throws(() => api.networks.connect(networkId, 'friendly'), /complete immutable ID/);
  assert.throws(
    () => api.networks.connect(networkId, containerId, { aliases: ['ok', 'ok'] }),
    /unique/,
  );
  const aliasOptions = { aliases: ['database.internal', 'database_2'] };
  const operations = [
    api.volumes.list(),
    api.volumes.inspect('cache'),
    api.volumes.create('cache'),
    api.volumes.remove('cache', 'a'.repeat(32)),
    api.networks.list(),
    api.networks.inspect('private'),
    api.networks.create('private'),
    api.networks.remove(networkId),
    api.networks.connect(networkId, containerId),
    api.networks.connect(networkId, containerId, aliasOptions),
    api.networks.disconnect(networkId, containerId),
    api.subscribe('volumes'),
    api.subscribe('networks'),
    api.subscribe('workspace-events'),
  ];
  const calls = [];
  for (let index = 0; index < operations.length; index += 1) calls.push((await next()).payload);
  assert.deepEqual(
    calls.map(({ call }) => call),
    [
      'volume_list',
      'volume_inspect',
      'volume_create',
      'volume_remove',
      'network_list',
      'network_inspect',
      'network_create',
      'network_remove',
      'network_connect',
      'network_connect',
      'network_disconnect',
      'event_subscribe',
      'event_subscribe',
      'event_subscribe',
    ],
  );
  assert.deepEqual(calls[3].with, { name: 'cache', generation: 'a'.repeat(32) });
  assert.deepEqual(calls[8].with, { reference: networkId, container: containerId });
  assert.deepEqual(calls[9].with, {
    reference: networkId,
    container: containerId,
    aliases: ['database.internal', 'database_2'],
  });
  assert.deepEqual(aliasOptions, { aliases: ['database.internal', 'database_2'] });
  assert.deepEqual(calls[10].with, { reference: networkId, container: containerId });
  const replies = [
    { reply: 'volumes', with: { volumes: [], truncated: false } },
    { reply: 'volume', with: { name: 'cache', driver: 'local', generation: 'a'.repeat(32) } },
    { reply: 'volume', with: { name: 'cache', driver: 'local', generation: 'a'.repeat(32) } },
    { reply: 'done' },
    { reply: 'networks', with: { networks: [], truncated: false } },
    {
      reply: 'network',
      with: { id: 'n1', name: 'private', driver: 'bridge', scope: 'local', kind: 'custom' },
    },
    { reply: 'identity', with: 'n1' },
    ...Array(7).fill({ reply: 'done' }),
  ];
  for (const payload of replies)
    stage.host.write(encode({ channel: 2, kind: KIND.response, payload }));
  await Promise.all(operations);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('resource removal waits arm first and require non-truncated exact absence', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const image = `sha256:${'a'.repeat(64)}`;
  const generation = 'b'.repeat(32);
  const network = 'c'.repeat(32);
  const cases = [
    [
      () => api.images.removeAndWait(image),
      'images',
      'image_remove',
      { reference: image },
      { snapshot: 'images', of: { images: [], truncated: false } },
      { changed: true, id: image },
    ],
    [
      () => api.volumes.removeAndWait('cache', generation),
      'volumes',
      'volume_remove',
      { name: 'cache', generation },
      { snapshot: 'volumes', of: { volumes: [], truncated: false } },
      { changed: true, name: 'cache', generation },
    ],
    [
      () => api.networks.removeAndWait(network),
      'networks',
      'network_remove',
      { reference: network },
      { snapshot: 'networks', of: { networks: [], truncated: false } },
      { changed: true, id: network },
    ],
  ];
  for (const [run, topic, call, argument, event, expected] of cases) {
    const operation = run();
    assert.deepEqual((await next()).payload, { call: 'event_subscribe', with: { topic } });
    stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
    assert.deepEqual((await next()).payload, { call, with: argument });
    stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
    stage.host.write(
      encode({
        channel: 12,
        kind: KIND.event,
        payload: { ...event, of: { ...event.of, truncated: true } },
      }),
    );
    assert.equal((await next()).kind, KIND.credit);
    stage.host.write(encode({ channel: 13, kind: KIND.event, payload: event }));
    assert.equal((await next()).kind, KIND.credit);
    assert.deepEqual((await next()).payload, { call: 'event_unsubscribe', with: { topic } });
    stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
    assert.deepEqual(await operation, expected);
  }
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('image inspection and destructive calls preserve explicit request shapes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const digest = `sha256:${'a'.repeat(64)}`;
  assert.throws(() => api.images.remove('alpine:3.20'), /complete immutable sha256 digest/);
  const operations = [
    api.images.inspect('alpine:3.20'),
    api.images.remove(digest),
    api.images.prune(),
  ];
  const calls = [];
  for (let index = 0; index < operations.length; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls, [
    { call: 'image_inspect', with: { reference: 'alpine:3.20' } },
    { call: 'image_remove', with: { reference: digest } },
    { call: 'image_prune' },
  ]);
  const details = {
    id: 'i1',
    references: [],
    created: '',
    size: 0,
    os: 'linux',
    architecture: 'amd64',
    entrypoint: [],
    command: [],
    working_directory: '',
    user: '',
  };
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'image_details', with: details } }),
  );
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: { reply: 'image_prune', with: { deleted: 2, space_reclaimed: 7 } },
    }),
  );
  assert.deepEqual(await Promise.all(operations), [
    details,
    undefined,
    { deleted: 2, space_reclaimed: 7 },
  ]);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('deep container methods and subscriptions use exact protocol request shapes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const containerId = 'a'.repeat(64);
  const executionId = 'b'.repeat(32);
  await assert.rejects(
    api.containers.executionOutput(executionId, { after: 0, limit: 17 }),
    /between 1 and 16/,
  );
  assert.throws(() => api.containers.signalExecution('7', 'SIGTERM'), /complete immutable ID/);
  assert.throws(() => api.containers.removeExecution('execution-name'), /complete immutable ID/);
  await assert.rejects(api.containers.execution('execution-name'), /complete immutable ID/);
  await assert.rejects(api.containers.executionLogs('abc123'), /complete immutable ID/);
  await assert.rejects(api.containers.waitExecution('7'), /complete immutable ID/);
  assert.throws(() => api.containers.stop('friendly-name'), /observed nonnegative safe generation/);
  assert.throws(() => api.containers.remove('abc123'), /observed nonnegative safe generation/);
  assert.throws(
    () => api.containers.kill('friendly-name', -1, 'SIGTERM'),
    /observed nonnegative safe generation/,
  );
  assert.throws(
    () => api.containers.rename('friendly-name', -1, 'worker'),
    /observed nonnegative safe generation/,
  );
  for (const action of ['start', 'pause', 'unpause', 'restart']) {
    assert.throws(
      () => api.containers[action]('friendly-name'),
      /observed nonnegative safe generation/,
    );
  }
  assert.throws(() => api.containers.rename(containerId, 7, '.worker'), /container name must/);
  assert.throws(
    () => api.containers.rename(containerId, 7, 'x'.repeat(129)),
    /container name must/,
  );
  const operations = [
    api.containers.processes('c1'),
    api.containers.logs('c1', { stdout: true, stderr: false }),
    api.containers.execution(executionId),
    api.containers.executions(),
    api.containers.executionLogs(executionId, { stdout: true, stderr: false }),
    api.containers.executionOutput(executionId, { after: 41, limit: 16 }),
    api.containers.waitExecution(executionId, { timeoutMs: 250 }),
    api.containers.start(containerId, 7),
    api.containers.pause(containerId, 7),
    api.containers.unpause(containerId, 7),
    api.containers.restart(containerId, 7),
    api.containers.rename(containerId, 7, 'worker_2.prod'),
    api.containers.stop(containerId, 7),
    api.containers.remove(containerId, 7),
    api.containers.kill(containerId, 7, 'SIGTERM'),
    api.containers.signalExecution(executionId, 'SIGHUP'),
    api.containers.removeExecution(executionId),
    api.containers.exec(containerId, 7, {
      command: ['sh', '-lc', 'true'],
      user: '1000',
      workingDirectory: '/work',
    }),
    api.subscribe('containers'),
    api.unsubscribe('containers'),
  ];
  const calls = [];
  for (let index = 0; index < operations.length - 1; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls, [
    {
      call: 'container_processes',
      with: { id: 'c1', after: 0, limit: 128 },
    },
    { call: 'container_logs', with: { id: 'c1', stdout: true, stderr: false } },
    { call: 'execution_inspect', with: { id: executionId } },
    { call: 'execution_list' },
    { call: 'execution_logs', with: { id: executionId, stdout: true, stderr: false } },
    { call: 'execution_output', with: { id: executionId, after: 41, limit: 16 } },
    { call: 'execution_wait', with: { id: executionId, timeout_ms: 250 } },
    { call: 'container_start', with: { id: containerId, generation: 7 } },
    { call: 'container_pause', with: { id: containerId, generation: 7 } },
    { call: 'container_unpause', with: { id: containerId, generation: 7 } },
    { call: 'container_restart', with: { id: containerId, generation: 7 } },
    { call: 'container_rename', with: { id: containerId, generation: 7, name: 'worker_2.prod' } },
    { call: 'container_stop', with: { id: containerId, generation: 7 } },
    { call: 'container_remove', with: { id: containerId, generation: 7 } },
    { call: 'container_kill', with: { id: containerId, generation: 7, signal: 'SIGTERM' } },
    { call: 'execution_kill', with: { id: executionId, signal: 'SIGHUP' } },
    { call: 'execution_remove', with: { id: executionId } },
    {
      call: 'container_exec',
      with: {
        id: containerId,
        generation: 7,
        command: ['sh', '-lc', 'true'],
        environment: [],
        user: '1000',
        working_directory: '/work',
      },
    },
    { call: 'event_subscribe', with: { topic: 'containers' } },
  ]);
  const replies = [
    {
      reply: 'processes',
      with: {
        container_id: containerId,
        titles: ['PID', 'PPID', 'USER', 'STAT', 'COMMAND'],
        processes: [['1', '0', 'root', '?', '/usr/bin/server']],
        snapshot: 'a'.repeat(64),
        next: null,
        more: false,
        observed_at_ms: 1_700_000_000_000,
        scope: 'initial',
        pid_identity: 'snapshot',
        truncated: false,
      },
    },
    {
      reply: 'logs',
      with: {
        stdout: [],
        stderr: [],
        truncated: false,
        stdout_truncated: false,
        stderr_truncated: false,
        eof: false,
      },
    },
    {
      reply: 'execution',
      with: {
        id: executionId,
        container_id: containerId,
        running: true,
        exit_code: 0,
        pid: 2,
        command: ['true'],
        user: 'root',
      },
    },
    { reply: 'executions', with: { executions: [], truncated: false } },
    {
      reply: 'logs',
      with: {
        stdout: [],
        stderr: [],
        truncated: false,
        stdout_truncated: false,
        stderr_truncated: false,
        eof: true,
      },
    },
    {
      reply: 'execution_output',
      with: {
        entries: [{ sequence: 42, timestamp_ms: 9, stream: 'stdout', bytes: [114, 111, 119, 10] }],
        next: 42,
        more: false,
        eof: false,
        gap: false,
      },
    },
    {
      reply: 'execution',
      with: {
        id: executionId,
        container_id: containerId,
        running: false,
        exit_code: 0,
        pid: 2,
        command: ['true'],
        user: 'root',
      },
    },
    ...Array(10).fill({ reply: 'done' }),
    { reply: 'identity', with: 'e2' },
    { reply: 'done' },
  ];
  for (const payload of replies)
    stage.host.write(encode({ channel: 2, kind: KIND.response, payload }));
  assert.deepEqual((await next()).payload, {
    call: 'event_unsubscribe',
    with: { topic: 'containers' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const results = await Promise.all(operations);
  assert.deepEqual(
    {
      containerId: results[0].container_id,
      scope: results[0].scope,
      pidIdentity: results[0].pid_identity,
      truncated: results[0].truncated,
    },
    { containerId, scope: 'initial', pidIdentity: 'snapshot', truncated: false },
  );
  assert.equal(results[1].eof, false, 'empty output from a running initial process remains open');
  assert.deepEqual(
    {
      eof: results[4].eof,
      stdout: results[4].stdout_truncated,
      stderr: results[4].stderr_truncated,
    },
    { eof: true, stdout: false, stderr: false },
  );
  assert.equal(results[5].next, 42);
  assert.equal(results[17], 'e2');
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('execution output cursor distinguishes live emptiness, later append, and final EOF', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const id = 'b'.repeat(32);

  const live = api.containers.executionOutput(id, { after: 0, limit: 16 });
  assert.deepEqual((await next()).payload, {
    call: 'execution_output',
    with: { id, after: 0, limit: 16 },
  });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'execution_output',
        with: { entries: [], next: 0, more: false, eof: false, gap: false },
      },
    }),
  );
  assert.equal((await live).eof, false);

  const appended = api.containers.executionOutput(id, { after: 0, limit: 16 });
  await next();
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'execution_output',
        with: {
          entries: [{ sequence: 9, timestamp_ms: 1, stream: 'stdout', bytes: [49, 10] }],
          next: 9,
          more: false,
          eof: false,
          gap: true,
        },
      },
    }),
  );
  assert.deepEqual(await appended, {
    entries: [{ sequence: 9, timestamp_ms: 1, stream: 'stdout', bytes: [49, 10] }],
    next: 9,
    more: false,
    eof: false,
    gap: true,
  });

  const final = api.containers.executionOutput(id, { after: 9, limit: 16 });
  await next();
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'execution_output',
        with: { entries: [], next: 9, more: false, eof: true, gap: false },
      },
    }),
  );
  assert.equal((await final).eof, true);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('real Unix fragmented replies preserve bounded stdin backpressure and explicit EOF order', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-exec-stdin-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  let peer;
  const server = net.createServer((socket) => {
    peer = socket;
    const greeting = encode({
      channel: 0,
      kind: KIND.open,
      payload: {
        protocol: PROTOCOL,
        extension: 'stdin_test',
        granted: PROTOCOL_CAPABILITIES.map(({ wire }) => wire),
      },
    });
    for (const byte of greeting) socket.write(Buffer.of(byte));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        const response = encode({
          channel: frame.channel,
          kind: KIND.response,
          payload: { reply: 'done' },
        });
        for (let offset = 0; offset < response.length; offset += 3)
          socket.write(response.subarray(offset, offset + 3));
      }
    });
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  const session = new Session(net.createConnection(socketPath));
  try {
    await session.ready;
    const id = 'e'.repeat(32);
    let firstAcknowledged = false;
    async function* input() {
      yield 'one\n';
      firstAcknowledged = calls.length === 1;
      yield Uint8Array.of(0, 255, 10);
    }
    assert.deepEqual(await workspace(session).containers.pipeExecutionStdin(id, input()), {
      chunks: 2,
      bytes: 7,
      closed: true,
    });
    assert.equal(
      firstAcknowledged,
      true,
      'the next source chunk is not requested before acknowledgement',
    );
    assert.deepEqual(calls, [
      { call: 'execution_write', with: { id, contents: [111, 110, 101, 10] } },
      { call: 'execution_write', with: { id, contents: [0, 255, 10] } },
      { call: 'execution_close_input', with: { id } },
    ]);
  } finally {
    await session.close();
    peer?.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('execution stdin rejects empty, oversized, malformed and unbounded chunks before framing', async () => {
  const calls = [];
  const api = workspace({
    granted: ['containers:input'],
    async call(name, payload) {
      calls.push([name, payload]);
      return { reply: 'done' };
    },
    onEvent() {
      return () => {};
    },
  });
  const id = 'e'.repeat(32);
  for (const input of ['', [], [256], new Uint8Array(65_537), '🙂'.repeat(20_000)]) {
    assert.throws(() => api.containers.writeExecutionStdin(id, input), /stdin/);
  }
  function* endless() {
    for (;;) yield 1;
  }
  assert.throws(() => api.containers.writeExecutionStdin(id, endless()), /65536/);
  assert.deepEqual(calls, []);
});

test('aborting an input producer does not invent EOF or consume another chunk', async () => {
  const calls = [];
  const controller = new AbortController();
  const api = workspace({
    granted: ['containers:input'],
    async call(name, payload) {
      calls.push([name, payload]);
      return { reply: 'done' };
    },
    onEvent() {
      return () => {};
    },
  });
  async function* source() {
    yield 'accepted';
    controller.abort('producer stopped');
    yield 'must not be pulled';
  }
  await assert.rejects(
    api.containers.pipeExecutionStdin('e'.repeat(32), source(), { signal: controller.signal }),
    { name: 'AbortError' },
  );
  assert.deepEqual(
    calls.map(([name]) => name),
    ['execution_write'],
  );
});

test('configured container creation preserves its bounded typed specification', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  await assert.rejects(
    workspace(stage.session).containers.create('alpine:3.20'),
    /requires a configuration object/,
  );
  const spec = {
    image: 'alpine:3.20',
    name: 'worker',
    hostname: 'h'.repeat(253),
    entrypoint: ['/init'],
    command: ['serve'],
    environment: [['MODE', 'agent']],
    working_directory: '/work',
    user: '1000',
    labels: [['owner', 'agent']],
    mounts: [{ volume: 'cache', target: '/cache' }],
    network: 'private',
    ports: [{ container: 8080, host: 18080, protocol: 'tcp' }],
    memory_mb: 512,
    cpus: 2,
    pids_limit: 128,
  };
  const pending = workspace(stage.session).containers.create(spec);
  assert.deepEqual((await next()).payload, {
    call: 'container_create',
    with: {
      spec: { ...spec, mounts: [{ volume: 'cache', target: '/cache', read_only: false }] },
    },
  });
  assert.equal(
    Object.hasOwn(spec.mounts[0], 'read_only'),
    false,
    'normalization does not mutate caller input',
  );
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'identity', with: 'c-rich' } }),
  );
  assert.equal(await pending, 'c-rich');
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('exec environment rejects secret-bearing invalid pairs before framing', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const id = 'c'.repeat(64);
  for (const environment of [
    [
      ['PGPASSWORD', 'one'],
      ['PGPASSWORD', 'two'],
    ],
    [['BAD=NAME', 'secret']],
    [['PGPASSWORD', 'x'.repeat(8193)]],
    Array.from({ length: 9 }, (_, index) => [`V${index}`, 'x'.repeat(8192)]),
  ])
    await assert.rejects(
      api.containers.exec(id, 4, { command: ['psql'], environment }),
      /environment/,
    );
  const pending = api.containers.exec(id, 4, {
    command: ['psql'],
    environment: [
      ['PGUSER', 'reader'],
      ['PGPASSWORD', 'sentinel-password'],
    ],
  });
  assert.deepEqual((await next()).payload, {
    call: 'container_exec',
    with: {
      id,
      generation: 4,
      command: ['psql'],
      environment: [
        ['PGUSER', 'reader'],
        ['PGPASSWORD', 'sentinel-password'],
      ],
      user: null,
      working_directory: null,
    },
  });
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'identity', with: 'e1' } }),
  );
  assert.equal(await pending, 'e1');
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('container terminal attachment preserves immutable identity and exact argv on the wire', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const id = 'a'.repeat(64);
  const pending = workspace(stage.session).containers.attachTerminal(id, [
    'sh',
    '-lc',
    'printf "%s" "$HOME"',
  ]);
  assert.deepEqual((await next()).payload, {
    call: 'container_attach_terminal',
    with: { id, command: ['sh', '-lc', 'printf "%s" "$HOME"'] },
  });
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'identity', with: 'p7' } }),
  );
  assert.equal(await pending, 'p7');
  assert.throws(
    () => workspace(stage.session).containers.attachTerminal('friendly', ['sh']),
    /complete immutable ID/,
  );
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('pane change observation subscribes over the live transport, filters metadata, returns credit and disposes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  let observed;
  const watching = api.watchPaneChanges((change) => {
    observed = change;
  });
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'pane-changes' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const dispose = await watching;
  const change = { slot: 'pane-7', kind: 'surface', revision: 12, generation: 40, coalesced: 3 };
  stage.host.write(
    encode({ channel: 9, kind: KIND.event, payload: { snapshot: 'pane_changes', of: change } }),
  );
  const credit = await next();
  assert.deepEqual(observed, change);
  assert.equal(credit.channel, 9);
  assert.equal(credit.kind, KIND.credit);
  const stopping = dispose();
  assert.deepEqual((await next()).payload, {
    call: 'event_unsubscribe',
    with: { topic: 'pane-changes' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await stopping;
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('pane change iterator withholds credit for slow consumers and surfaces replacement, abort, and disconnect', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const controller = new AbortController();
  const changes = workspace(stage.session).paneChanges({ signal: controller.signal });
  const first = changes.next();
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'pane-changes' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const original = { slot: 'shell', kind: 'terminal', revision: 7, generation: 3, coalesced: 0 };
  stage.host.write(
    encode({ channel: 19, kind: KIND.event, payload: { snapshot: 'pane_changes', of: original } }),
  );
  assert.deepEqual((await first).value, original);

  const second = changes.next();
  const credit = await next();
  assert.deepEqual([credit.channel, credit.kind, credit.payload], [19, KIND.credit, 1]);
  const replacement = {
    slot: 'shell',
    kind: 'surface',
    revision: 1,
    generation: 4,
    coalesced: 2,
  };
  stage.host.write(
    encode({
      channel: 19,
      kind: KIND.event,
      payload: { snapshot: 'pane_changes', of: replacement },
    }),
  );
  assert.deepEqual((await second).value, replacement);

  const stopped = changes.next();
  assert.equal((await next()).kind, KIND.credit);
  controller.abort('agent stopped');
  assert.deepEqual((await next()).payload, {
    call: 'event_unsubscribe',
    with: { topic: 'pane-changes' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await assert.rejects(stopped, (error) => error.name === 'AbortError');

  const disconnected = workspace(stage.session).paneChanges();
  const pending = disconnected.next();
  assert.equal((await next()).payload.call, 'event_subscribe');
  stage.host.destroy();
  await assert.rejects(pending, /connection closed/);
  stage.session.close();
  stage.server.close();
});

test('terminal topology, bounded input, grid resize and retitle use exact typed calls', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const terminal = workspace(stage.session).terminal;
  const topology = terminal.topology();
  const pinning = terminal.pinTab('t1');
  const splitting = terminal.splitObserved('s1', 4, 7, 'below');
  const spawning = terminal.spawnObserved('s1', 4, 7, ['printf', '%s\n', 'ready']);
  const writing = terminal.writeInput('s1', 4, 7, 'echo hello\n');
  const resizing = terminal.resizeGridObserved('s1', 4, 7, 120, 40);
  const ratio = terminal.ratioObserved('s1', 4, 7, 0.6);
  const focusing = terminal.focusObserved('s1', 4, 7);
  const retitling = terminal.retitleObserved('s1', 4, 7, ' Build 🧪 ');
  const closing = terminal.closeObserved('s1', 4, 7);
  assert.deepEqual((await next()).payload, { call: 'terminal_topology' });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_pin_tab',
    with: { tab: 't1', pinned: true },
  });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_split_observed',
    with: { slot: 's1', generation: 4, revision: 7, division: 'below' },
  });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_spawn_observed',
    with: { slot: 's1', generation: 4, revision: 7, command: ['printf', '%s\n', 'ready'] },
  });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_write_pane',
    with: {
      slot: 's1',
      generation: 4,
      revision: 7,
      contents: [...new TextEncoder().encode('echo hello\n')],
    },
  });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_resize_grid_observed',
    with: { slot: 's1', generation: 4, revision: 7, columns: 120, rows: 40 },
  });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_ratio_observed',
    with: { slot: 's1', generation: 4, revision: 7, ratio: 0.6 },
  });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_focus_pane_observed',
    with: { slot: 's1', generation: 4, revision: 7 },
  });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_retitle_pane_observed',
    with: { slot: 's1', generation: 4, revision: 7, title: ' Build 🧪 ' },
  });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_close_pane_observed',
    with: { slot: 's1', generation: 4, revision: 7 },
  });
  const tree = { active_tab: null, tabs: [] };
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'topology', with: tree } }),
  );
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'identity', with: 's2' } }),
  );
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual(await topology, tree);
  assert.equal(await splitting, 's2');
  await Promise.all([pinning, spawning, writing, resizing, ratio, focusing, retitling, closing]);
  assert.throws(() => terminal.spawn('s1', []), /1\.\.=64/);
  assert.throws(() => terminal.spawn('s1', ['sh', 'bad\0argument']), /NUL-free/);
  assert.throws(() => terminal.spawn('s1', ['x'.repeat(4097)]), /4096 bytes/);
  assert.throws(() => terminal.spawnObserved('s1', 4, -1, ['true']), /generation and revision/);
  assert.throws(() => terminal.writeInput('s1', 4, 7, new Uint8Array(65_537)), /65536 byte limit/);
  assert.throws(() => terminal.writeInput('s1', -1, 7, 'x'), /generation and revision/);
  assert.throws(() => terminal.closeObserved('s1', 4, -1), /generation and revision/);
  assert.throws(() => terminal.splitObserved('s1', 4, -1, 'below'), /generation and revision/);
  assert.throws(() => terminal.ratioObserved('s1', 4, -1, 0.6), /generation and revision/);
  assert.throws(() => terminal.resizeGrid('s1', 0, 24), /1\.\.=1000/);
  assert.throws(() => terminal.resizeGridObserved('s1', 4, -1, 80, 24), /generation and revision/);
  for (const title of ['', '   ', 'line\nbreak', 'nul\0byte', '🧪'.repeat(65)]) {
    assert.throws(() => terminal.retitle('s1', title), /pane title must/);
  }
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('terminal screen read preserves bounded text, truncation and cursor over framing', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const reading = workspace(stage.session).terminal.read('s1', 25);
  assert.deepEqual((await next()).payload, {
    call: 'terminal_read_pane',
    with: { slot: 's1', lines: 25 },
  });
  const screen = {
    slot: 's1',
    generation: 7,
    revision: 11,
    columns: 132,
    rows: 41,
    lines: ['ready'],
    cursor_column: 5,
    cursor_row: 2,
    truncated: true,
  };
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'text', with: screen } }),
  );
  assert.deepEqual(await reading, screen);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('pane discovery uses its distinct bounded inventory reply', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const pending = workspace(stage.session).terminal.panes();
  assert.deepEqual((await next()).payload, { call: 'pane_list' });
  const inventory = {
    panes: [
      {
        slot: 'workspace',
        kind: 'native',
        provider: null,
        tab: null,
        title: 'Workspace',
        focused: false,
      },
    ],
    truncated: false,
  };
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'panes', with: inventory } }),
  );
  assert.deepEqual(await pending, inventory);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('filesystem controls use exact confined protocol request shapes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const files = workspace(stage.session).files;
  const operations = [
    files.stat('logs/app.log'),
    files.mkdir('logs/new'),
    files.rename('logs/a', 'logs/b'),
    files.remove('logs/b'),
  ];
  assert.deepEqual((await next()).payload, {
    call: 'filesystem_stat',
    with: { path: 'logs/app.log' },
  });
  assert.deepEqual((await next()).payload, {
    call: 'filesystem_mkdir',
    with: { path: 'logs/new' },
  });
  assert.deepEqual((await next()).payload, {
    call: 'filesystem_rename',
    with: { from: 'logs/a', to: 'logs/b' },
  });
  assert.deepEqual((await next()).payload, { call: 'filesystem_remove', with: { path: 'logs/b' } });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: { reply: 'entry', with: { path: 'logs/app.log', directory: false, size: 4 } },
    }),
  );
  for (let index = 1; index < operations.length; index += 1) {
    stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  }
  const results = await Promise.all(operations);
  assert.equal(results[0].size, 4);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('filesystem mutations stop adversarial iterables at the host byte bound before Unix framing', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const files = workspace(stage.session).files;
  let produced = 0;
  function* oversized() {
    while (true) {
      produced += 1;
      yield 97;
    }
  }
  await assert.rejects(
    files.writeObserved('src/review.ts', 'v1:1:2:3:4:5:6:7', oversized()),
    /limited to 65536 bytes/,
  );
  assert.equal(produced, 65_537);
  await assert.rejects(files.createObserved('src/review.ts', [0, 256]), /bytes from 0 through 255/);

  const healthy = files.stat('src/review.ts');
  assert.deepEqual((await next()).payload, {
    call: 'filesystem_stat',
    with: { path: 'src/review.ts' },
  });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'entry',
        with: { path: 'src/review.ts', directory: false, size: 12, identity: 'v1:1:2:3:4:5:6:7' },
      },
    }),
  );
  assert.equal((await healthy).size, 12);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('directory pagination carries an authoritative bounded cursor over Unix framing', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const pending = workspace(stage.session).files.listPage('src', {
    after: 'src/a.ts',
    observed: 'dir-v1',
    limit: 2,
  });
  assert.deepEqual((await next()).payload, {
    call: 'filesystem_list_page',
    with: { path: 'src', after: 'src/a.ts', observed: 'dir-v1', limit: 2 },
  });
  const page = {
    entries: [{ path: 'src/b.ts', directory: false, size: 7 }],
    identity: 'dir-v1',
    next: 'src/b.ts',
    more: true,
  };
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'directory_page', with: page } }),
  );
  assert.deepEqual(await pending, page);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('an inconsistent directory page is rejected without poisoning the ordered session', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const files = workspace(stage.session).files;
  await assert.rejects(
    files.listPage('src', { after: 'src/a.ts' }),
    /requires both after and observed/,
  );
  const malformed = files.listPage('src', { limit: 1 });
  assert.equal((await next()).payload.call, 'filesystem_list_page');
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'directory_page',
        with: { entries: [], identity: 'dir-v1', next: null, more: true },
      },
    }),
  );
  await assert.rejects(malformed, /inconsistent filesystem directory page/);
  const healthy = files.stat('src/a.ts');
  assert.equal((await next()).payload.call, 'filesystem_stat');
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'entry',
        with: { path: 'src/a.ts', directory: false, size: 1 },
      },
    }),
  );
  assert.equal((await healthy).size, 1);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('observed filesystem ranges and creation preserve exact identities on the wire', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const files = workspace(stage.session).files;
  const range = files.readRange('logs/app.log', 12, 34, 'v1:1:2:3:4:5:6:7');
  assert.deepEqual((await next()).payload, {
    call: 'filesystem_read_range',
    with: { path: 'logs/app.log', offset: 12, limit: 34, observed: 'v1:1:2:3:4:5:6:7' },
  });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'file_range',
        with: {
          path: 'logs/app.log',
          identity: 'v1:1:2:3:4:5:6:7',
          offset: 12,
          total: 14,
          contents: [111, 107],
          eof: true,
          truncated: false,
        },
      },
    }),
  );
  assert.equal((await range).identity, 'v1:1:2:3:4:5:6:7');

  const write = files.createObserved('logs/new.log', [110, 101, 119]);
  assert.deepEqual((await next()).payload, {
    call: 'filesystem_create_observed',
    with: { path: 'logs/new.log', contents: [110, 101, 119] },
  });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: { reply: 'identity', with: 'v1:1:8:3:4:5:6:7' },
    }),
  );
  assert.equal(await write, 'v1:1:8:3:4:5:6:7');

  const replace = files.writeObserved(
    'logs/new.log',
    'v1:1:8:3:4:5:6:7',
    [117, 112, 100, 97, 116, 101, 100],
  );
  assert.deepEqual((await next()).payload, {
    call: 'filesystem_write_observed',
    with: {
      path: 'logs/new.log',
      observed: 'v1:1:8:3:4:5:6:7',
      contents: [117, 112, 100, 97, 116, 101, 100],
    },
  });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: { reply: 'identity', with: 'v1:1:8:3:4:5:6:9' },
    }),
  );
  assert.equal(await replace, 'v1:1:8:3:4:5:6:9');

  const rename = files.renameObserved('logs/new.log', 'logs/final.log', 'v1:1:8:3:4:5:6:9');
  assert.deepEqual((await next()).payload, {
    call: 'filesystem_rename_observed',
    with: { from: 'logs/new.log', to: 'logs/final.log', observed: 'v1:1:8:3:4:5:6:9' },
  });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: { reply: 'identity', with: 'v1:1:8:3:4:5:6:8' },
    }),
  );
  assert.equal(await rename, 'v1:1:8:3:4:5:6:8');
  const remove = files.removeObserved('logs/final.log', 'v1:1:8:3:4:5:6:8');
  assert.deepEqual((await next()).payload, {
    call: 'filesystem_remove_observed',
    with: { path: 'logs/final.log', observed: 'v1:1:8:3:4:5:6:8' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await remove;
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('pane semantics and actions preserve revision and node identity', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const tree = api.terminal.semantics('pane-7');
  const read = (await next()).payload;
  assert.deepEqual(read, { call: 'pane_semantic_read', with: { slot: 'pane-7' } });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'semantics',
        with: {
          slot: 'pane-7',
          generation: 3,
          revision: 9,
          truncated: false,
          root: {
            id: 0,
            role: 'Column',
            label: null,
            value: null,
            disabled: false,
            destructive: false,
            actions: [],
            children: [],
          },
        },
      },
    }),
  );
  assert.deepEqual(
    { generation: (await tree).generation, revision: (await tree).revision },
    { generation: 3, revision: 9 },
  );
  assert.throws(
    () => api.terminal.act('pane-7', { revision: 9, node: 4, action: 'invoke' }),
    /requires nonnegative safe integer generation/,
  );
  const acted = api.terminal.act('pane-7', {
    generation: 3,
    revision: 9,
    node: 4,
    action: 'invoke',
  });
  assert.deepEqual((await next()).payload, {
    call: 'pane_semantic_action',
    with: {
      slot: 'pane-7',
      action: { generation: 3, revision: 9, node: 4, action: 'invoke' },
    },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await acted;
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('one client operation projects terminal panes into visible screen text', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const pending = workspace(stage.session).terminal.toText('shell', { lines: 40 });
  assert.deepEqual((await next()).payload, { call: 'pane_list' });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'panes',
        with: {
          panes: [
            {
              slot: 'shell',
              generation: 4,
              revision: 8,
              kind: 'terminal',
              provider: null,
              tab: 'tab-1',
              title: 'Shell',
              focused: true,
            },
          ],
          truncated: false,
        },
      },
    }),
  );
  assert.deepEqual((await next()).payload, {
    call: 'terminal_read_pane',
    with: { slot: 'shell', lines: 40 },
  });
  const snapshot = {
    slot: 'shell',
    generation: 4,
    revision: 8,
    columns: 80,
    rows: 24,
    lines: ['$ printf ready', 'ready'],
    cursor_column: 0,
    cursor_row: 2,
    truncated: false,
  };
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'text', with: snapshot } }),
  );
  assert.deepEqual(await pending, { kind: 'terminal', text: '$ printf ready\nready', snapshot });
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('the same client operation projects every UI pane into bounded semantic XML', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const pending = workspace(stage.session).terminal.toText('settings');
  assert.deepEqual((await next()).payload, { call: 'pane_list' });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'panes',
        with: {
          panes: [
            {
              slot: 'settings',
              generation: 2,
              revision: 5,
              kind: 'native',
              provider: null,
              tab: null,
              title: 'Settings',
              focused: false,
            },
          ],
          truncated: false,
        },
      },
    }),
  );
  assert.deepEqual((await next()).payload, {
    call: 'pane_semantic_read',
    with: { slot: 'settings' },
  });
  const snapshot = {
    slot: 'settings',
    generation: 2,
    revision: 5,
    truncated: false,
    root: {
      id: 0,
      role: 'page',
      label: 'Workspace settings',
      value: null,
      disabled: false,
      destructive: false,
      actions: [],
      children: [],
    },
  };
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'semantics', with: snapshot } }),
  );
  const result = await pending;
  assert.equal(result.kind, 'ui');
  assert.deepEqual(result.snapshot, snapshot);
  assert.match(result.text, /^<pane slot="settings" generation="2" revision="5"/);
  assert.match(result.text, /<label>Workspace settings<\/label>/);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('pane text conversion never guesses when bounded discovery omitted the slot', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const pending = workspace(stage.session).terminal.toText('omitted');
  await next();
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'panes',
        with: { panes: [], truncated: true },
      },
    }),
  );
  await assert.rejects(pending, /cannot be resolved from a truncated inventory/);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('bounded all-pane conversion refuses a cursor race instead of mixing inventories', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const pending = workspace(stage.session).terminal.readAll({ lines: 20 });
  assert.deepEqual((await next()).payload, { call: 'pane_list' });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'panes',
        with: {
          panes: [
            {
              slot: 'shell',
              generation: 4,
              revision: 8,
              kind: 'terminal',
              provider: null,
              tab: 'tab-1',
              title: 'Shell',
              focused: true,
            },
          ],
          truncated: true,
        },
      },
    }),
  );
  assert.deepEqual((await next()).payload, {
    call: 'terminal_read_pane',
    with: { slot: 'shell', lines: 20 },
  });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'text',
        with: {
          slot: 'shell',
          generation: 4,
          revision: 9,
          columns: 80,
          rows: 24,
          lines: ['changed'],
          cursor_column: 0,
          cursor_row: 1,
          truncated: false,
        },
      },
    }),
  );
  await assert.rejects(pending, /changed during bounded text conversion/);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('pane text wait arms first, ignores its unchanged cursor, and disposes after a later revision', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const pending = workspace(stage.session).terminal.waitForText(
    'shell',
    { generation: 4, revision: 8 },
    { lines: 40, timeoutMs: 1_000 },
  );
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'pane-changes' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual((await next()).payload, { call: 'pane_list' });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'panes',
        with: {
          panes: [
            {
              slot: 'shell',
              generation: 4,
              revision: 8,
              kind: 'terminal',
              provider: null,
              tab: 'tab-1',
              title: 'Shell',
              focused: true,
            },
          ],
          truncated: false,
        },
      },
    }),
  );
  assert.deepEqual((await next()).payload, {
    call: 'terminal_read_pane',
    with: { slot: 'shell', lines: 40 },
  });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'text',
        with: {
          slot: 'shell',
          generation: 4,
          revision: 8,
          columns: 80,
          rows: 24,
          lines: ['old'],
          cursor_column: 3,
          cursor_row: 0,
          truncated: false,
        },
      },
    }),
  );
  stage.host.write(
    encode({
      channel: 17,
      kind: KIND.event,
      payload: {
        snapshot: 'pane_changes',
        of: {
          slot: 'shell',
          kind: 'terminal',
          generation: 4,
          revision: 8,
          coalesced: 0,
        },
      },
    }),
  );
  assert.equal(
    (await next()).kind,
    KIND.credit,
    'the initial unchanged snapshot is acknowledged without a read',
  );
  stage.host.write(
    encode({
      channel: 17,
      kind: KIND.event,
      payload: {
        snapshot: 'pane_changes',
        of: {
          slot: 'shell',
          kind: 'terminal',
          generation: 4,
          revision: 9,
          coalesced: 2,
        },
      },
    }),
  );
  assert.deepEqual((await next()).payload, { call: 'pane_list' });
  assert.equal((await next()).kind, KIND.credit);
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'panes',
        with: {
          panes: [
            {
              slot: 'shell',
              generation: 4,
              revision: 9,
              kind: 'terminal',
              provider: null,
              tab: 'tab-1',
              title: 'Shell',
              focused: true,
            },
          ],
          truncated: false,
        },
      },
    }),
  );
  assert.deepEqual((await next()).payload, {
    call: 'terminal_read_pane',
    with: { slot: 'shell', lines: 40 },
  });
  const snapshot = {
    slot: 'shell',
    generation: 4,
    revision: 9,
    columns: 80,
    rows: 24,
    lines: ['ready'],
    cursor_column: 5,
    cursor_row: 0,
    truncated: false,
  };
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'text', with: snapshot } }),
  );
  assert.deepEqual((await next()).payload, {
    call: 'event_unsubscribe',
    with: { topic: 'pane-changes' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual(await pending, {
    changed: true,
    readable: { kind: 'terminal', text: 'ready', snapshot },
  });
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('pane text wait accepts slot replacement and rejects incomplete cursors before subscribing', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  await assert.rejects(
    api.terminal.waitForText('shell', { generation: 4 }),
    /exact nonnegative generation and revision/,
  );
  const pending = api.terminal.waitForText(
    'shell',
    { generation: 4, revision: 8 },
    { timeoutMs: 1_000 },
  );
  assert.equal((await next()).payload.call, 'event_subscribe');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.equal((await next()).payload.call, 'pane_list');
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'panes',
        with: {
          panes: [
            {
              slot: 'shell',
              generation: 4,
              revision: 8,
              kind: 'terminal',
              provider: null,
              tab: 'tab-1',
              title: 'Shell',
              focused: true,
            },
          ],
          truncated: false,
        },
      },
    }),
  );
  assert.equal((await next()).payload.call, 'terminal_read_pane');
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'text',
        with: {
          slot: 'shell',
          generation: 4,
          revision: 8,
          columns: 80,
          rows: 24,
          lines: ['old'],
          cursor_column: 3,
          cursor_row: 0,
          truncated: false,
        },
      },
    }),
  );
  stage.host.write(
    encode({
      channel: 18,
      kind: KIND.event,
      payload: {
        snapshot: 'pane_changes',
        of: {
          slot: 'shell',
          kind: 'native',
          generation: 5,
          revision: 1,
          coalesced: 0,
        },
      },
    }),
  );
  assert.equal((await next()).payload.call, 'pane_list');
  await next();
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'panes',
        with: {
          panes: [
            {
              slot: 'shell',
              generation: 5,
              revision: 1,
              kind: 'native',
              provider: null,
              tab: null,
              title: 'Settings',
              focused: true,
            },
          ],
          truncated: false,
        },
      },
    }),
  );
  assert.equal((await next()).payload.call, 'pane_semantic_read');
  const snapshot = {
    slot: 'shell',
    generation: 5,
    revision: 1,
    truncated: false,
    root: {
      id: 0,
      role: 'page',
      label: 'Settings',
      value: null,
      disabled: false,
      destructive: false,
      actions: [],
      children: [],
    },
  };
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'semantics', with: snapshot } }),
  );
  assert.equal((await next()).payload.call, 'event_unsubscribe');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.equal((await pending).readable.kind, 'ui');
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('pane text wait timeout returns its cursor and releases subscription credit', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const after = { generation: 4, revision: 8 };
  const pending = workspace(stage.session).terminal.waitForText('shell', after, { timeoutMs: 5 });
  assert.equal((await next()).payload.call, 'event_subscribe');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.equal((await next()).payload.call, 'pane_list');
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'panes',
        with: {
          panes: [
            {
              slot: 'shell',
              generation: 4,
              revision: 8,
              kind: 'terminal',
              provider: null,
              tab: 'tab-1',
              title: 'Shell',
              focused: true,
            },
          ],
          truncated: false,
        },
      },
    }),
  );
  assert.equal((await next()).payload.call, 'terminal_read_pane');
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'text',
        with: {
          slot: 'shell',
          generation: 4,
          revision: 8,
          columns: 80,
          rows: 24,
          lines: ['old'],
          cursor_column: 3,
          cursor_row: 0,
          truncated: false,
        },
      },
    }),
  );
  assert.deepEqual((await next()).payload, {
    call: 'event_unsubscribe',
    with: { topic: 'pane-changes' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual(await pending, { changed: false, after });
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('semantic action wait subscribes before authority and captures an event preceding the action reply', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const action = { generation: 2, revision: 5, node: 7, action: 'invoke' };
  const pending = api.terminal.actAndWait('settings', action, { timeoutMs: 1_000 });
  assert.deepEqual((await next()).payload, {
    call: 'event_subscribe',
    with: { topic: 'pane-changes' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual((await next()).payload, {
    call: 'pane_semantic_action',
    with: { slot: 'settings', action },
  });
  stage.host.write(
    encode({
      channel: 19,
      kind: KIND.event,
      payload: {
        snapshot: 'pane_changes',
        of: {
          slot: 'settings',
          kind: 'native',
          generation: 2,
          revision: 6,
          coalesced: 0,
        },
      },
    }),
  );
  assert.equal((await next()).kind, KIND.credit);
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual((await next()).payload, { call: 'pane_list' });
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'panes',
        with: {
          panes: [
            {
              slot: 'settings',
              generation: 2,
              revision: 6,
              kind: 'native',
              provider: null,
              tab: null,
              title: 'Settings',
              focused: true,
            },
          ],
          truncated: false,
        },
      },
    }),
  );
  assert.deepEqual((await next()).payload, {
    call: 'pane_semantic_read',
    with: { slot: 'settings' },
  });
  const snapshot = {
    slot: 'settings',
    generation: 2,
    revision: 6,
    truncated: false,
    root: {
      id: 0,
      role: 'page',
      label: 'Updated',
      value: null,
      disabled: false,
      destructive: false,
      actions: [],
      children: [],
    },
  };
  stage.host.write(
    encode({ channel: 2, kind: KIND.response, payload: { reply: 'semantics', with: snapshot } }),
  );
  assert.deepEqual((await next()).payload, {
    call: 'event_unsubscribe',
    with: { topic: 'pane-changes' },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const result = await pending;
  assert.equal(result.changed, true);
  assert.deepEqual(result.readable.snapshot, snapshot);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});

test('semantic action refusal still releases its armed pane subscription', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const pending = api.terminal.actAndWait('settings', {
    generation: 2,
    revision: 5,
    node: 7,
    action: 'invoke',
  });
  assert.equal((await next()).payload.call, 'event_subscribe');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.equal((await next()).payload.call, 'pane_semantic_action');
  stage.host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      flags: 3,
      payload: {
        error: 'conflict',
        detail: 'pane changed',
      },
    }),
  );
  assert.equal((await next()).payload.call, 'event_unsubscribe');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await assert.rejects(pending, /pane changed/);
  stage.session.close();
  stage.host.destroy();
  stage.server.close();
});
