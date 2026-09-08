import assert from 'node:assert/strict';
import { getEventListeners } from 'node:events';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';
import { queryObjects } from 'node:v8';
import {
  connect,
  ExecutionOperationError,
  ExecutionOutputGapError,
  Session,
  TerminalOperationError,
  workspace,
} from '../dist/index.js';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

test('real Unix watcher accepts the host initial snapshot before subscribe acknowledgement', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-initial-snapshot-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        if (frame.payload.call === 'event_subscribe') {
          socket.write(
            encode({
              channel: 101,
              kind: KIND.event,
              payload: { snapshot: 'containers', of: [] },
            }),
          );
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'container_list') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'containers', with: [] },
            }),
          );
        } else if (frame.payload.call === 'event_unsubscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'fixture', granted: ['containers:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const host = workspace(session);
    const seen = [];
    const stop = await host.watchContainers((containers) => seen.push(containers));
    assert.deepEqual(seen, [[]]);
    assert.deepEqual(await host.containers.list(), []);
    await stop();
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix private state preserves bytes and independent read/write authority', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-extension-state-'));
  const socketPath = path.join(directory, 'host.sock');
  const requests = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        requests.push(frame.payload);
        const payload =
          frame.payload.call === 'state_read'
          ? { reply: 'state', with: { identity: 'absent', contents: [0, 17, 255] } }
            : { error: 'refused', capability: 'state:write' };
        socket.write(
          encode({
            channel: frame.channel,
            kind: KIND.response,
            flags: payload.error ? 3 : 1,
            payload,
          }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'state-fixture', granted: ['state:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const api = workspace(session);
    assert.deepEqual(await api.state.read(), { identity: 'absent', contents: [0, 17, 255] });
    await assert.rejects(api.state.write('absent', [1, 2]), /state:write/);
    assert.deepEqual(requests, [{ call: 'state_read' }]);
    await assert.rejects(api.state.write('absent', new Uint8Array(1024 * 1024 + 1)), /1 MiB/);
    await assert.rejects(api.state.write('latest', [1]), /exact identity/);
    assert.equal(requests.length, 1, 'denied, malformed, and oversized writes never reach socket framing');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix private state exposes a stale-writer conflict instead of losing an update', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-extension-state-cas-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  let identity = 'absent';
  let contents = [];
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        let payload;
        if (frame.payload.call === 'state_read') {
          payload = { reply: 'state', with: { identity, contents } };
        } else if (frame.payload.with.observed !== identity) {
          payload = { error: 'conflict', detail: 'extension state changed after it was read' };
        } else {
          contents = frame.payload.with.contents;
          identity = `sha256:${'a'.repeat(64)}`;
          payload = { reply: 'identity', with: identity };
        }
        socket.write(encode({
          channel: frame.channel, kind: KIND.response, flags: payload.error ? 3 : 1, payload,
        }));
      }
    });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: {
      protocol: 1, peer: 'state-cas-fixture', granted: ['state:read', 'state:write'],
    } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const api = workspace(session);
    const foreground = await api.state.read();
    const background = await api.state.read();
    assert.equal(await api.state.write(background.identity, [2]), `sha256:${'a'.repeat(64)}`);
    await assert.rejects(api.state.write(foreground.identity, [1]), /changed after it was read/);
    assert.deepEqual(await api.state.read(), { identity: `sha256:${'a'.repeat(64)}`, contents: [2] });
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix catalogue carries bounded compatibility hints', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-catalogue-compat-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        assert.deepEqual(frame.payload, { call: 'extension_catalogue' });
        socket.write(encode({ channel: frame.channel, kind: KIND.response, payload: {
          reply: 'extension_catalogue', with: { complete: true, entries: [{
            id: 'storybook', title: 'Component playground', description: 'Native components', version: '1.4.0',
            reference: 'registry/storybook:latest', publisher: 'Husklet', source: 'first-party',
            protocol: 1, architectures: ['amd64', 'arm64'],
          }, {
            id: 'metrics', title: 'Metrics explorer', description: 'Workspace metrics', version: '2.1.0',
            reference: 'registry/metrics:2.1.0', publisher: 'Example', source: 'partner:metrics',
            protocol: 1, architectures: ['amd64'],
          }] },
        } }));
      }
    });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: {
      protocol: 1, peer: 'fixture', granted: ['extensions:read'],
    } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const catalogue = await workspace(session).extensions.catalogue();
    assert.equal(catalogue.entries[0].protocol, 1);
    assert.equal(catalogue.entries[0].version, '1.4.0');
    assert.equal(catalogue.entries[1].version, '2.1.0');
    assert.deepEqual(catalogue.entries[0].architectures, ['amd64', 'arm64']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix extension inventory preserves every effective permission dimension', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-extension-grants-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        assert.deepEqual(frame.payload, { call: 'extension_list' });
        socket.write(
          encode({
            channel: frame.channel,
            kind: KIND.response,
            payload: {
              reply: 'extensions',
              with: [{
                name: 'database-tools', image_digest: 'sha256:reviewed', status: 'standby',
                version: '2.0.0', enabled: false, pane_providers: [],
                granted: ['containers:read', 'filesystem:write'],
                containers: { selectors: [{ name: 'postgres' }], create: false },
                filesystem: { read: [], write: [{ exact: 'database.json' }], create: [], delete: [], rename: [] },
                workspace_environment: { read: [{ workspace: 'dev', name: 'PGPASSWORD' }], write: [] },
              }],
            },
          }),
        );
      }
    });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: {
      protocol: 1, peer: 'fixture', granted: ['extensions:read'],
    } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const [installed] = await workspace(session).extensions.list();
    assert.deepEqual(installed.granted, ['containers:read', 'filesystem:write']);
    assert.deepEqual(installed.containers.selectors, [{ name: 'postgres' }]);
    assert.deepEqual(installed.filesystem.write, [{ exact: 'database.json' }]);
    assert.deepEqual(installed.workspace_environment.read, [{ workspace: 'dev', name: 'PGPASSWORD' }]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix filesystem inventory can reconcile immediately after reconnect', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-filesystem-inventory-'));
  const socketPath = path.join(directory, 'host.sock');
  const requests = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        requests.push(frame.payload);
        socket.write(
          encode({
            channel: frame.channel,
            kind: KIND.response,
            payload: {
              reply: 'file_inventory',
              with: {
                entries: [{ path: 'src/index.ts', directory: false, size: 17, identity: 'sha256:abc' }],
                complete: false,
                coalesced: 9,
                revision: 12,
              },
            },
          }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'fixture', granted: ['filesystem:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const inventory = await workspace(session).files.inventory();
    assert.deepEqual(requests, [{ call: 'filesystem_inventory' }]);
    assert.equal(inventory.complete, false);
    assert.equal(inventory.coalesced, 9);
    assert.equal(inventory.entries[0].identity, 'sha256:abc');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix execution cancellation is one bounded ordered operation', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-execution-cancel-'));
  const socketPath = path.join(directory, 'host.sock');
  const executionId = 'c'.repeat(32);
  const requests = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        requests.push(frame.payload);
        socket.write(
          encode({ channel: frame.channel, kind: KIND.response, payload: { reply: 'done' } }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'fixture', granted: ['containers:create', 'containers:execute', 'containers:lifecycle', 'containers:remove'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    assert.throws(
      () => containers.cancelExecution(executionId, { timeoutMs: 0 }),
      /cancellation timeout/,
    );
    assert.equal(requests.length, 0, 'invalid cancellation is rejected before framing');
    await containers.cancelExecution(executionId, { signal: 'SIGINT', timeoutMs: 750 });
    assert.deepEqual(requests, [
      {
        call: 'execution_cancel',
        with: { id: executionId, signal: 'SIGINT', timeout_ms: 750 },
      },
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix streaming cancellation preserves inspection, cleanup, and session reuse', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-stream-cancel-'));
  const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64);
  const executionId = 'e'.repeat(32);
  const requests = [];
  const connections = new Set();
  const execution = {
    id: executionId,
    container_id: containerId,
    running: false,
    exit_code: 130,
    pid: 42,
    command: ['psql'],
    user: 'postgres',
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        requests.push(frame.payload);
        const call = frame.payload.call;
        const payload =
          call === 'container_exec'
            ? { reply: 'identity', with: executionId }
            : call === 'execution_output'
              ? {
                  reply: 'execution_output',
                  with: { entries: [], next: 0, more: false, eof: false, gap: false },
                }
              : call === 'execution_inspect'
                ? { reply: 'execution', with: execution }
                : call === 'container_list'
                  ? { reply: 'containers', with: [] }
                  : { reply: 'done' };
        socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'fixture',
          granted: ['containers:read', 'containers:execute'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    const controller = new AbortController();
    await assert.rejects(
      containers.execStreaming(
        containerId,
        7,
        { command: ['psql'], signal: controller.signal, cancelSignal: 'SIGINT' },
        () => controller.abort('query view closed'),
      ),
      (error) => {
        assert(error instanceof ExecutionOperationError);
        assert.equal(error.executionId, executionId);
        assert.equal(error.phase, 'output');
        assert.equal(error.cause.name, 'AbortError');
        return true;
      },
    );
    assert.deepEqual(
      requests.map(({ call }) => call),
      ['container_exec', 'execution_output', 'execution_cancel'],
      'cancellation follows the final delivered page and does not inspect or remove implicitly',
    );
    assert.deepEqual(requests[2].with, {
      id: executionId,
      signal: 'SIGINT',
      timeout_ms: 1_000,
    });
    assert.equal(getEventListeners(controller.signal, 'abort').length, 0);

    assert.deepEqual(await containers.execution(executionId), execution);
    await containers.removeExecution(executionId);
    assert.deepEqual(await containers.list(), []);
    assert.deepEqual(requests.map(({ call }) => call), [
      'container_exec',
      'execution_output',
      'execution_cancel',
      'execution_inspect',
      'execution_remove',
      'container_list',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix filesystem iterators page recursively, stream stable chunks, and cancel locally', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-filesystem-iterator-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  const requests = [];
  let rangeCalls = 0;
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        requests.push(frame.payload);
        let reply;
        let value;
        if (frame.payload.call === 'filesystem_list_page') {
          const input = frame.payload.with;
          reply = 'directory_page';
          if (input.path === 'src/lib') {
            value = {
              entries: [{ path: 'src/lib/nested.ts', directory: false, size: 3 }],
              identity: 'lib-v1',
              next: 'src/lib/nested.ts',
              more: false,
            };
          } else if (input.after === null) {
            value = {
              entries: [
                { path: 'src/a.ts', directory: false, size: 4 },
                { path: 'src/lib', directory: true, size: 0 },
              ],
              identity: 'src-v1',
              next: 'src/lib',
              more: true,
            };
          } else {
            assert.deepEqual([input.after, input.observed], ['src/lib', 'src-v1']);
            value = {
              entries: [{ path: 'src/z.ts', directory: false, size: 2 }],
              identity: 'src-v1',
              next: 'src/z.ts',
              more: false,
            };
          }
        } else if (frame.payload.call === 'filesystem_read_range') {
          const input = frame.payload.with;
          rangeCalls += 1;
          reply = 'file_range';
          if (input.path === 'src/bad.ts') {
            value = {
              path: 'src/wrong.ts',
              identity: 'bad-v1',
              offset: 0,
              total: 0,
              contents: [],
              eof: true,
              truncated: false,
            };
            socket.write(
              encode({ channel: 2, kind: KIND.response, payload: { reply, with: value } }),
            );
            continue;
          }
          assert.equal(input.path, 'src/a.ts');
          if (input.offset === 0) {
            assert.equal(input.observed, null);
            value = {
              path: input.path,
              identity: 'file-v1',
              offset: 0,
              total: 4,
              contents: [65, 66],
              eof: false,
              truncated: true,
            };
          } else {
            assert.deepEqual([input.offset, input.observed], [2, 'file-v1']);
            value = {
              path: input.path,
              identity: 'file-v1',
              offset: 2,
              total: 4,
              contents: [67, 68],
              eof: true,
              truncated: false,
            };
          }
        } else if (frame.payload.call === 'workspace_info') {
          reply = 'workspace';
          value = { name: 'demo', image: 'alpine', architecture: 'amd64' };
        }
        if (reply) {
          socket.write(
            encode({ channel: 2, kind: KIND.response, payload: { reply, with: value } }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'fixture', granted: ['filesystem:read', 'workspaces:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const host = workspace(session);
    const walk = host.files.walk('src', { pageSize: 2 });
    assert.equal((await walk.next()).value.path, 'src/a.ts');
    await new Promise((resolve) => setTimeout(resolve, 20));
    assert.equal(
      requests.filter(({ call }) => call === 'filesystem_list_page').length,
      1,
      'an unconsumed traversal does not fetch another page',
    );
    const paths = ['src/a.ts'];
    for await (const entry of walk) paths.push(entry.path);
    assert.deepEqual(paths, ['src/a.ts', 'src/lib', 'src/lib/nested.ts', 'src/z.ts']);

    const controller = new AbortController();
    const cancelled = host.files.readChunks('src/a.ts', {
      chunkBytes: 2,
      signal: controller.signal,
    });
    assert.deepEqual((await cancelled.next()).value.contents, [65, 66]);
    controller.abort('document removed from index');
    await assert.rejects(cancelled.next(), (error) => error.name === 'AbortError');
    assert.equal(rangeCalls, 1, 'cancellation between chunks sends no ambiguous ordered call');
    assert.equal((await host.info()).name, 'demo', 'local cancellation leaves the session usable');

    const chunks = [];
    for await (const range of host.files.readChunks('src/a.ts', { chunkBytes: 2 })) {
      chunks.push(...range.contents);
    }
    assert.deepEqual(chunks, [65, 66, 67, 68]);
    await assert.rejects(
      host.files.readRange('src/bad.ts'),
      /host returned an inconsistent filesystem file range/,
    );
    assert.equal((await host.info()).name, 'demo', 'a malformed range leaves correlation intact');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix filesystem walk rejects directory cycles and handles a very deep tree iteratively', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-filesystem-depth-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  let listCalls = 0;
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.payload.call !== 'filesystem_list_page') continue;
        listCalls += 1;
        const requested = frame.payload.with.path;
        let entries;
        if (requested === 'cycle') {
          entries = [{ path: 'cycle', directory: true, size: 0 }];
        } else if (requested === 'duplicate') {
          entries = [
            { path: 'duplicate/child', directory: true, size: 0 },
            { path: 'duplicate/child', directory: true, size: 0 },
          ];
        } else if (requested === 'duplicate/child') {
          entries = [];
        } else {
          const depth = requested.split('/').length - 1;
          entries = depth < 1_200 ? [{ path: `${requested}/d`, directory: true, size: 0 }] : [];
        }
        socket.write(
          encode({
            channel: frame.channel,
            kind: KIND.response,
            payload: {
              reply: 'directory_page',
              with: {
                entries,
                identity: `directory-${listCalls}`,
                next: entries.at(-1)?.path ?? null,
                more: false,
              },
            },
          }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'fixture', granted: ['filesystem:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const files = workspace(session).files;
    await assert.rejects(
      files.walk('cycle').next(),
      /host returned an inconsistent filesystem directory page/,
    );
    const duplicate = files.walk('duplicate');
    assert.equal((await duplicate.next()).value.path, 'duplicate/child');
    await assert.rejects(
      duplicate.next(),
      /host returned repeated filesystem directory "duplicate\/child"/,
    );
    let depth = 0;
    for await (const entry of files.walk('deep')) {
      assert.equal(entry.directory, true);
      depth += 1;
    }
    assert.equal(depth, 1_200);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix output pages apply backpressure, cancel locally, and fail closed on a gap', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-output-pages-'));
  const socketPath = path.join(directory, 'host.sock');
  const executionId = 'e'.repeat(32);
  const connections = new Set();
  let outputCalls = 0;
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        if (frame.payload.call === 'execution_output') {
          outputCalls += 1;
          const page =
            outputCalls === 1
              ? { entries: [], next: 0, more: false, eof: false, gap: false }
              : {
                  entries: [
                    { sequence: 10, timestamp_ms: 1, stream: 'stdout', bytes: [114, 111, 119] },
                  ],
                  next: 10,
                  more: false,
                  eof: false,
                  gap: true,
                };
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'execution_output', with: page },
            }),
          );
        } else if (frame.payload.call === 'workspace_info') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: {
                reply: 'workspace',
                with: { name: 'demo', image: 'alpine', architecture: 'amd64' },
              },
            }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'fixture',
          granted: ['containers:read', 'workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const host = workspace(session);
    const controller = new AbortController();
    const pages = host.containers.executionOutputPages(executionId, {
      pollIntervalMs: 100,
      signal: controller.signal,
    });
    assert.deepEqual((await pages.next()).value, {
      entries: [],
      next: 0,
      more: false,
      eof: false,
      gap: false,
    });
    await new Promise((resolve) => setTimeout(resolve, 20));
    assert.equal(outputCalls, 1, 'an unconsumed iterator never prefetches output');
    const waiting = pages.next();
    controller.abort('query view closed');
    await assert.rejects(waiting, (error) => error.name === 'AbortError');
    assert.equal(outputCalls, 1, 'poll cancellation sends no ambiguous ordered call');
    assert.equal((await host.info()).name, 'demo', 'local cancellation leaves the session usable');

    const incomplete = host.containers.executionOutputPages(executionId, { pollIntervalMs: 10 });
    await assert.rejects(incomplete.next(), (error) => {
      assert(error instanceof ExecutionOutputGapError);
      assert.deepEqual([error.executionId, error.after, error.next], [executionId, 0, 10]);
      return true;
    });
    assert.equal((await host.info()).name, 'demo', 'a retention gap does not corrupt correlation');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix stream drives a typed inventory watcher and returns event credit', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-client-'));
  const socketPath = path.join(directory, 'host.sock');
  const observed = [];
  let creditSeen;
  const credit = new Promise((resolve) => {
    creditSeen = resolve;
  });
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        observed.push(frame);
        if (frame.channel === 7 && frame.kind === KIND.credit) creditSeen();
        if (frame.channel === CONTROL && frame.kind === KIND.response) continue;
        if (
          frame.channel === 2 &&
          ['event_subscribe', 'event_unsubscribe'].includes(frame.payload.call)
        ) {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        }
        if (frame.channel === 2 && frame.payload.call === 'workspace_info') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: {
                reply: 'workspace',
                with: { name: 'demo', image: 'alpine', architecture: 'amd64' },
              },
            }),
          );
          socket.write(
            encode({
              channel: 7,
              kind: KIND.event,
              payload: { snapshot: 'images', of: { images: [], truncated: false } },
            }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'fixture', granted: ['workspaces:read', 'images:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const pushed = [];
    const session = await connect({ path: socketPath });
    assert.deepEqual(session.granted, ['workspaces:read', 'images:read']);
    const stop = await workspace(session).watchImages((images) => pushed.push(images));
    assert.equal((await workspace(session).info()).name, 'demo');
    await credit;
    assert.deepEqual(pushed, [[]]);
    assert(observed.some((frame) => frame.channel === CONTROL && frame.kind === KIND.response));
    assert(
      observed.some(
        (frame) => frame.channel === 7 && frame.kind === KIND.credit && frame.payload === 1,
      ),
    );
    await stop();
    assert(
      observed.some(
        (frame) =>
          frame.payload?.call === 'event_subscribe' && frame.payload.with.topic === 'images',
      ),
    );
    assert(
      observed.some(
        (frame) =>
          frame.payload?.call === 'event_unsubscribe' && frame.payload.with.topic === 'images',
      ),
    );
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix semantic action wait arms before authority and disposes after changed text', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-text-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (frame.payload.call === 'event_subscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'pane_semantic_action') {
          socket.write(
            encode({
              channel: 11,
              kind: KIND.event,
              payload: {
                snapshot: 'pane_changes',
                of: {
                  slot: 'settings',
                  kind: 'native',
                  generation: 2,
                  revision: 4,
                  coalesced: 0,
                },
              },
            }),
          );
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'pane_list') {
          socket.write(
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
                      revision: 4,
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
        } else if (frame.payload.call === 'pane_semantic_read') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: {
                reply: 'semantics',
                with: {
                  slot: 'settings',
                  generation: 2,
                  revision: 4,
                  truncated: false,
                  root: {
                    id: 0,
                    role: 'page',
                    label: 'Done',
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
        } else if (frame.payload.call === 'event_unsubscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'action-wait',
          granted: ['panes:observe', 'panes:semantic-read', 'panes:semantic-control'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).terminal.actAndWait('settings', {
      generation: 2,
      revision: 3,
      node: 7,
      action: 'invoke',
    });
    assert.equal(result.changed, true);
    assert.match(result.readable.text, /<label>Done<\/label>/);
    assert.deepEqual(calls, [
      'event_subscribe',
      'pane_semantic_action',
      'pane_list',
      'pane_semantic_read',
      'event_unsubscribe',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix acquisition wait reconnects from authoritative status without a new event', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-acquisition-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (frame.payload.call === 'event_subscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'extension_acquisition_status') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: {
                reply: 'extension_acquisition',
                with: {
                  job: 'job-7',
                  reference: 'registry/demo:1',
                  revision: 5,
                  state: 'ready',
                  progress: null,
                  candidate: null,
                  error: null,
                },
              },
            }),
          );
        } else if (frame.payload.call === 'event_unsubscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'acquisition-wait',
          granted: ['extensions:install'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).extensions.waitForAcquisition('job-7', 4, {
      timeoutMs: 100,
    });
    assert.equal(result.changed, true);
    assert.equal(result.status.revision, 5);
    assert.deepEqual(calls, [
      'event_subscribe',
      'extension_acquisition_status',
      'event_unsubscribe',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix occupant switch arms before CAS and verifies provider inventory', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-switch-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (
          frame.payload.call === 'event_subscribe' ||
          frame.payload.call === 'event_unsubscribe'
        ) {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'terminal_switch_occupant_observed') {
          socket.write(
            encode({
              channel: 13,
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
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'pane_list') {
          socket.write(
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
                      provider: { extension: 'manager', provider: 'main' },
                      tab: 'tab',
                      title: 'Manager',
                      focused: true,
                    },
                  ],
                  truncated: false,
                },
              },
            }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'switch-wait',
          granted: ['panes:observe', 'terminals:layout-control', 'terminals:process-control'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).terminal.switchOccupantAndWait('pane-1', 7, 11, {
      kind: 'surface',
      extension: 'manager',
      provider: 'main',
    });
    assert.equal(result.changed, true);
    assert.equal(result.pane.provider.extension, 'manager');
    assert.deepEqual(calls, [
      'event_subscribe',
      'terminal_switch_occupant_observed',
      'pane_list',
      'event_unsubscribe',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix extension enable arms inventory before digest-bound authority', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-enable-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const digest = `sha256:${'b'.repeat(64)}`;
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        if (frame.payload.call === 'extension_enable')
          socket.write(
            encode({
              channel: 14,
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
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'enable-wait',
          granted: ['extensions:read', 'extensions:control'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).extensions.enableAndWait('manager', digest);
    assert.equal(result.changed, true);
    assert.deepEqual(calls, ['event_subscribe', 'extension_enable', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix extension disable arms inventory before digest-bound authority', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-disable-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const digest = `sha256:${'d'.repeat(64)}`;
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        if (frame.payload.call === 'extension_disable')
          socket.write(
            encode({
              channel: 15,
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
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'disable-wait',
          granted: ['extensions:read', 'extensions:control'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).extensions.disableAndWait('manager', digest);
    assert.equal(result.changed, true);
    assert.deepEqual(calls, ['event_subscribe', 'extension_disable', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix extension remove arms inventory before authority and observes absence', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-remove-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const digest = `sha256:${'e'.repeat(64)}`;
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        if (frame.payload.call === 'extension_remove')
          socket.write(
            encode({
              channel: 16,
              kind: KIND.event,
              payload: {
                snapshot: 'extensions',
                of: [],
              },
            }),
          );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'remove-wait',
          granted: ['extensions:read', 'extensions:remove'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).extensions.removeAndWait('manager', digest);
    assert.deepEqual(result, {
      changed: true,
      removed: { name: 'manager', image_digest: digest },
      replacement: null,
    });
    assert.deepEqual(calls, ['event_subscribe', 'extension_remove', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix extension retry arms inventory before digest-bound authority', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-retry-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const digest = `sha256:${'f'.repeat(64)}`;
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        if (frame.payload.call === 'extension_retry')
          socket.write(
            encode({
              channel: 17,
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
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'retry-wait',
          granted: ['extensions:read', 'extensions:control'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).extensions.retryAndWait('manager', digest);
    assert.equal(result.changed, true);
    assert.deepEqual(calls, ['event_subscribe', 'extension_retry', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('negotiated grants are immutable and deny calls and topics before any socket write', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-grants-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload);
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: {
              reply: 'workspace',
              with: { name: 'demo', image: 'alpine', architecture: 'amd64' },
            },
          }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'grants',
          granted: ['workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    assert.deepEqual(session.grantedCapabilities, ['workspaces:read']);
    assert(Object.isFrozen(session.grantedCapabilities));
    assert.throws(() => session.grantedCapabilities.push('containers:read'), TypeError);
    await assert.rejects(
      session.call('container_list'),
      (error) =>
        error instanceof Error &&
        error.name === 'ExtensionError' &&
        error.kind === 'denied' &&
        error.capability === 'containers:read',
    );
    await assert.rejects(
      session.call('event_subscribe', { topic: 'containers' }),
      /containers:read/,
    );
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(calls, [], 'locally denied authority writes no request frame');
    assert.equal((await session.call('workspace_info')).reply, 'workspace');
    assert.deepEqual(
      calls,
      [{ call: 'workspace_info' }],
      'a negotiated authority still reaches the host',
    );
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('AbortSignal writes nothing before a call and closes ordered Unix calls after write', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-abort-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const delivered = [];
  let peerClosed;
  let peer;
  let observedTwo;
  const closed = new Promise((resolve) => {
    peerClosed = resolve;
  });
  const twoCalls = new Promise((resolve) => {
    observedTwo = resolve;
  });
  const server = net.createServer((socket) => {
    peer = socket;
    socket.on('error', () => {});
    socket.on('close', peerClosed);
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel === 2) {
          calls.push(frame.payload);
          if (calls.length === 2) observedTwo();
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'abort',
          granted: ['workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath, onReply: (reply) => delivered.push(reply) });
    const before = new AbortController();
    before.abort('not sent');
    await assert.rejects(session.call('workspace_info', undefined, { signal: before.signal }), {
      name: 'AbortError',
    });
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(calls, [], 'an already-aborted call emits no protocol frame');

    const firstAbort = new AbortController();
    const first = session.call('workspace_info', undefined, { signal: firstAbort.signal });
    const second = session.call('workspace_list');
    await twoCalls;
    firstAbort.abort('stop');
    // A host racing cancellation may still try to answer the written calls;
    // the destroyed stream must not deliver either answer to another caller.
    for (const payload of [
      { reply: 'workspace', with: { name: 'demo', image: 'alpine', architecture: 'amd64' } },
      { reply: 'workspaces', with: [] },
    ])
      peer.write(encode({ channel: 2, kind: KIND.response, payload }));
    await assert.rejects(first, { name: 'AbortError' });
    await assert.rejects(second, { name: 'AbortError' });
    await closed;
    assert.deepEqual(
      calls.map(({ call }) => call),
      ['workspace_info', 'workspace_list'],
    );
    assert.deepEqual(delivered, [], 'no reply can be rebound after cancellation closes the stream');
    await assert.rejects(session.call('workspace_info'), /closed/);
  } finally {
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix control frames ping both directions and close every pending operation', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-control-'));
  const socketPath = path.join(directory, 'host.sock');
  const frames = [];
  let connected;
  const server = net.createServer((socket) => {
    connected = socket;
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        frames.push(frame);
        if (frame.kind === KIND.ping)
          socket.write(encode({ channel: frame.channel, kind: KIND.pong, payload: frame.payload }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'control', granted: ['workspaces:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const closed = [];
    const session = await connect({
      path: socketPath,
      timeout: 40,
      onClose: (error) => closed.push(error.message),
    });
    connected.write(encode({ channel: 17, kind: KIND.ping, payload: Buffer.from([0, 255, 4]) }));
    await session.ping();
    await new Promise((resolve) => setImmediate(resolve));
    const pong = frames.find((frame) => frame.kind === KIND.pong);
    assert.deepEqual(pong?.payload, Buffer.from([0, 255, 4]));
    const pending = session.call('workspace_info');
    connected.write(encode({ channel: CONTROL, kind: KIND.close, payload: Buffer.alloc(0) }));
    await assert.rejects(pending, /host closed the session/);
    await new Promise((resolve) => setTimeout(resolve, 60));
    assert.equal(closed.length, 1);
  } finally {
    connected?.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('a matching pong outside the control channel cannot complete a heartbeat', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-pong-channel-'));
  const socketPath = path.join(directory, 'host.sock');
  let connected;
  const server = net.createServer((socket) => {
    connected = socket;
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind === KIND.ping) {
          socket.write(encode({ channel: 17, kind: KIND.pong, payload: frame.payload }));
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'pong_channel', granted: [] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath, timeout: 200 });
    await assert.rejects(session.ping(), /pong arrived outside the control channel/);
    await assert.rejects(session.ping(), /session is closed/);
    await session.close();
  } finally {
    connected?.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('a timed-out real Unix heartbeat closes concurrent calls before a late pong can cross generations', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-ping-timeout-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  let latePong;
  let reportLargeCall;
  const largeCallSeen = new Promise((resolve) => {
    reportLargeCall = resolve;
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind === KIND.ping) latePong = frame;
        if (frame.payload?.call === 'event_subscribe') {
          socket.write(
            encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }),
          );
        }
        if (frame.payload?.call === 'state_write') {
          reportLargeCall();
          socket.write(
            encode({
              channel: 19,
              kind: KIND.event,
              payload: { snapshot: 'containers', of: [] },
            }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'ping_timeout',
          granted: ['state:write', 'containers:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const closed = [];
    const events = [];
    const session = await connect({
      path: socketPath,
      timeout: 80,
      onClose: (error) => closed.push(error.message),
      onEvent: (event) => events.push(event),
    });
    await session.call('event_subscribe', { topic: 'containers' });
    const heartbeat = session.ping();
    const largeCall = session.call('state_write', {
      observed: 'absent',
      contents: new Array(256 * 1024).fill(7),
    });
    await largeCallSeen;
    while (events.length === 0) await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(events, [{ snapshot: 'containers', of: [] }]);
    await assert.rejects(heartbeat, /ping timed out/);
    await assert.rejects(largeCall, /ping timed out/);
    await session.closed;
    assert.equal(closed.length, 1, 'timeout establishes exactly one close boundary');
    assert(latePong, 'the peer retained a real heartbeat token');
    await assert.rejects(session.call('state_write', { observed: 'absent', contents: [] }), /closed/);
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix partial EOF and illegal headers fail closed without sending credit', async () => {
  for (const malformed of ['partial', 'flags']) {
    const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-malformed-'));
    const socketPath = path.join(directory, 'host.sock');
    const replies = [];
    const server = net.createServer((socket) => {
      socket.on('data', (chunk) => replies.push(chunk));
      if (malformed === 'partial') {
        const frame = encode({
          channel: CONTROL,
          kind: KIND.open,
          payload: { protocol: 1, peer: 'bad', granted: [] },
        });
        socket.end(frame.subarray(0, frame.length - 2));
      } else {
        const frame = encode({
          channel: CONTROL,
          kind: KIND.open,
          payload: { protocol: 1, peer: 'bad', granted: [] },
        });
        frame[9] = 0x80;
        socket.end(frame);
      }
    });
    await new Promise((resolve) => server.listen(socketPath, resolve));
    try {
      await assert.rejects(
        connect({ path: socketPath, connectTimeout: 1_000 }),
        malformed === 'partial' ? /unfinished frame/ : /unknown flags/,
      );
      assert.equal(
        Buffer.concat(replies).length,
        0,
        'a malformed peer receives neither greeting nor event credit',
      );
    } finally {
      await new Promise((resolve) => server.close(resolve));
      await rm(directory, { recursive: true, force: true });
    }
  }
});

test('truncated Unix frame tears down a pending call and subscription before clean reconnect', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-truncated-active-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  let generation = 0;
  const server = net.createServer((socket) => {
    generation += 1;
    const connection = generation;
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        if (frame.payload.call === 'event_subscribe') {
          socket.write(
            encode({ channel: frame.channel, kind: KIND.response, payload: { reply: 'done' } }),
          );
          socket.write(
            encode({
              channel: 41,
              kind: KIND.event,
              payload: { snapshot: 'containers', of: [] },
            }),
          );
        } else if (frame.payload.call === 'workspace_info' && connection === 1) {
          const response = encode({
            channel: frame.channel,
            kind: KIND.response,
            payload: {
              reply: 'workspace',
              with: { name: 'broken', image: 'alpine', architecture: 'amd64' },
            },
          });
          socket.end(response.subarray(0, response.length - 3));
        } else if (frame.payload.call === 'workspace_info') {
          socket.write(
            encode({
              channel: frame.channel,
              kind: KIND.response,
              payload: {
                reply: 'workspace',
                with: { name: 'reconnected', image: 'alpine', architecture: 'amd64' },
              },
            }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: `fixture-${connection}`,
          granted: ['containers:read', 'workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const events = [];
    const controller = new AbortController();
    const first = await connect({ path: socketPath, timeout: 1_000 });
    const stop = await workspace(first).watchContainers((snapshot) => events.push(snapshot));
    while (events.length === 0) await new Promise((resolve) => setImmediate(resolve));
    const pending = workspace(first, { signal: controller.signal }).info();
    await assert.rejects(pending, /unfinished frame/);
    assert.match((await first.closed).message, /unfinished frame/);
    assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
    const delivered = events.length;
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(events.length, delivered, 'closed sessions deliver no later subscription callbacks');
    await assert.rejects(stop(), /closed/);

    const second = await connect({ path: socketPath, timeout: 1_000 });
    assert.equal((await workspace(second).info()).name, 'reconnected');
    await second.close();
    assert.equal(generation, 2, 'the same Unix listener accepted a fresh session generation');
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('invalid UTF-8 tears down active Unix state before bounded clean reconnect', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-invalid-utf8-active-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  let generation = 0;
  const server = net.createServer((socket) => {
    generation += 1;
    const connection = generation;
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        if (frame.payload.call === 'event_subscribe') {
          socket.write(
            encode({ channel: frame.channel, kind: KIND.response, payload: { reply: 'done' } }),
          );
          socket.write(
            encode({
              channel: 43,
              kind: KIND.event,
              payload: { snapshot: 'containers', of: [] },
            }),
          );
        } else if (frame.payload.call === 'workspace_info' && connection === 1) {
          const malformed = Buffer.alloc(13);
          malformed.writeUInt32LE(1, 0);
          malformed.writeUInt32LE(frame.channel, 4);
          malformed.writeUInt8(KIND.response, 8);
          malformed.writeUInt8(1, 9);
          malformed[12] = 0xff;
          const lateEvent = encode({
            channel: 43,
            kind: KIND.event,
            payload: { snapshot: 'containers', of: [] },
          });
          socket.write(Buffer.concat([malformed, lateEvent]));
        } else if (frame.payload.call === 'workspace_info') {
          socket.write(
            encode({
              channel: frame.channel,
              kind: KIND.response,
              payload: {
                reply: 'workspace',
                with: { name: 'reconnected', image: 'alpine', architecture: 'amd64' },
              },
            }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: `fixture-${connection}`,
          granted: ['containers:read', 'workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const events = [];
    const controller = new AbortController();
    const first = await connect({ path: socketPath, pendingLimit: 2, timeout: 1_000 });
    const stop = await workspace(first).watchContainers((snapshot) => events.push(snapshot));
    while (events.length === 0) await new Promise((resolve) => setImmediate(resolve));

    const pending = workspace(first, { signal: controller.signal }).info();
    await assert.rejects(pending, /not valid UTF-8 JSON/);
    assert.match((await first.closed).message, /not valid UTF-8 JSON/);
    assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
    const delivered = events.length;
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(events.length, delivered, 'bytes after the malformed frame never reach callbacks');
    await assert.rejects(stop(), /closed/);

    const second = await connect({ path: socketPath, pendingLimit: 2, timeout: 1_000 });
    assert.equal((await workspace(second).info()).name, 'reconnected');
    await second.close();
    assert.equal(generation, 2, 'the same listener accepted a fresh bounded session generation');
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('a real Unix event flood cannot outgrow callback delivery or cross reconnection', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-event-backlog-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  let generation = 0;
  const server = net.createServer((socket) => {
    generation += 1;
    const connection = generation;
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        if (frame.payload.call === 'event_subscribe') {
          socket.write(
            encode({ channel: frame.channel, kind: KIND.response, payload: { reply: 'done' } }),
          );
        } else if (frame.payload.call === 'workspace_info' && connection === 1) {
          const events = [1, 2, 3].map((revision) =>
            encode({
              channel: 47,
              kind: KIND.event,
              payload: { snapshot: 'containers', of: [], revision },
            }),
          );
          socket.write(Buffer.concat(events));
        } else if (frame.payload.call === 'workspace_info') {
          socket.write(
            encode({
              channel: frame.channel,
              kind: KIND.response,
              payload: {
                reply: 'workspace',
                with: { name: 'fresh', image: 'alpine', architecture: 'amd64' },
              },
            }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: `backlog-${connection}`,
          granted: ['containers:read', 'workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const delivered = [];
    const controller = new AbortController();
    const first = await connect({ path: socketPath, pendingLimit: 2, timeout: 1_000 });
    const host = workspace(first);
    const stopFirst = await host.watchContainers((snapshot) => delivered.push(['first', snapshot]));
    const stopSecond = await host.watchContainers((snapshot) => delivered.push(['second', snapshot]));

    const pending = workspace(first, { signal: controller.signal }).info();
    await assert.rejects(pending, /event delivery limit of 2 is exhausted/);
    assert.match((await first.closed).message, /event delivery limit of 2 is exhausted/);
    assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(delivered, [], 'queued old-generation events never reach either GUI listener');
    await stopFirst();
    await assert.rejects(stopSecond(), /closed/);

    const second = await connect({ path: socketPath, pendingLimit: 2, timeout: 1_000 });
    assert.equal((await workspace(second).info()).name, 'fresh');
    await second.close();
    assert.equal(generation, 2);
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('a malformed greeting fails immediately instead of stranding readiness', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-greeting-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { peer: 'missing-version', granted: [] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  const started = Date.now();
  try {
    await assert.rejects(
      connect({ path: socketPath, connectTimeout: 1_000 }),
      /protocol must be an integer/,
    );
    assert(
      Date.now() - started < 500,
      'structural greeting errors do not wait for the connection deadline',
    );
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('missing, malformed, and duplicated greetings fail closed before calls', async () => {
  for (const greeting of [
    { protocol: 1, granted: [] },
    { protocol: 1, peer: '-host', granted: [] },
    { protocol: 1, peer: 'A'.repeat(65), granted: [] },
  ]) {
    const connections = new Set();
    const server = net.createServer((socket) => {
      connections.add(socket);
      socket.on('close', () => connections.delete(socket));
      socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: greeting }));
    });
    await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
    try {
      await assert.rejects(
        connect({ path: server.address(), connectTimeout: 500 }),
        /valid extension name/,
      );
    } finally {
      for (const connection of connections) connection.destroy();
      await new Promise((resolve) => server.close(resolve));
    }
  }

  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: { protocol: 1, peer: 'duplicate', granted: ['workspaces:read'] },
    });
    socket.write(Buffer.concat([greeting, greeting]));
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  try {
    await assert.rejects(
      connect({ path: server.address(), connectTimeout: 500 }),
      /second greeting|connection closed/,
    );
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
  }
});

test('real socket write backpressure admits no further calls until drain', async () => {
  const bounded = (promise, label) =>
    Promise.race([
      promise,
      new Promise((_, reject) => setTimeout(() => reject(new Error(`${label} timed out`)), 1_000)),
    ]);
  const server = net.createServer();
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const accepted = new Promise((resolve) => server.once('connection', resolve));
  const client = net.createConnection(server.address().port, '127.0.0.1');
  await new Promise((resolve, reject) => {
    client.once('connect', resolve);
    client.once('error', reject);
  });
  const host = await accepted;
  const baselineListeners = Object.fromEntries(
    ['data', 'end', 'drain'].map((event) => [event, client.listenerCount(event)]),
  );
  host.write(
    encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: { protocol: 1, peer: 'pressure', granted: ['workspaces:read'] },
    }),
  );
  const session = new Session(client, { timeout: 1_000 });
  await bounded(session.ready, 'greeting');
  const write = client.write.bind(client);
  client.write = (bytes) => {
    write(bytes);
    return false;
  };
  const first = session.call('workspace_info');
  await assert.rejects(session.call('workspace_info'), /write backpressure/);
  host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'workspace',
        with: { name: 'demo', image: 'alpine', architecture: 'amd64' },
      },
    }),
  );
  await bounded(first, 'first call');
  client.write = write;
  client.emit('drain');
  const second = session.call('workspace_info');
  host.write(
    encode({
      channel: 2,
      kind: KIND.response,
      payload: {
        reply: 'workspace',
        with: { name: 'demo', image: 'alpine', architecture: 'amd64' },
      },
    }),
  );
  await bounded(second, 'second call');
  const ended = session.closed;
  await bounded(session.close(), 'close');
  assert.match((await bounded(ended, 'closed lifecycle')).message, /extension session closed/);
  for (const [event, count] of Object.entries(baselineListeners))
    assert.equal(client.listenerCount(event), count);
  host.destroy();
  await new Promise((resolve) => server.close(resolve));
});

test('a pending large Unix call retains correlation metadata but not its request argument', async () => {
  class RetainedBytes extends Array {}
  const server = net.createServer((socket) => {
    socket.on('error', () => {});
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'retention', granted: ['state:write'] },
      }),
    );
    socket.on('data', () => {});
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const connections = new Set();
  server.on('connection', (socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
  });
  try {
    const session = await connect({ path: server.address(), timeout: 1_000 });
    const issue = () => {
      const contents = new RetainedBytes(256 * 1024).fill(255);
      return session.call('state_write', { observed: 'absent', contents });
    };
    const pending = issue();
    await new Promise((resolve) => setImmediate(resolve));

    assert.equal(queryObjects(RetainedBytes, { format: 'count' }), 0);
    const rejected = assert.rejects(pending, /closed/);
    await session.close();
    await rejected;
    assert.equal(queryObjects(RetainedBytes, { format: 'count' }), 0);
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
  }
});

test('real Unix event credit waits for drain when its listener fills the write buffer', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-event-pressure-'));
  const socketPath = path.join(directory, 'host.sock');
  const received = [];
  let peer;
  let reportCredit;
  const credit = new Promise((resolve) => {
    reportCredit = resolve;
  });
  const server = net.createServer((socket) => {
    peer = socket;
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        received.push(frame);
        if (frame.channel === 19 && frame.kind === KIND.credit) reportCredit(frame);
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'event_pressure', granted: ['workspaces:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  const client = net.createConnection(socketPath);
  await new Promise((resolve, reject) => {
    client.once('connect', resolve);
    client.once('error', reject);
  });
  let call;
  let reportEvent;
  const event = new Promise((resolve) => {
    reportEvent = resolve;
  });
  const closed = [];
  const session = new Session(client, {
    timeout: 1_000,
    onClose: (error) => closed.push(error),
    onEvent: () => {
      call = session.call('workspace_info');
      call.catch(() => {});
      reportEvent();
    },
  });
  try {
    await session.ready;
    const write = client.write.bind(client);
    client.write = (bytes) => {
      write(bytes);
      return false;
    };
    peer.write(
      encode({
        channel: 19,
        kind: KIND.event,
        payload: { pane_provider: 'database', slot: 'pane-17' },
      }),
    );
    await Promise.race([
      event,
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error('event listener did not run')), 200),
      ),
    ]);
    assert(call, 'the event listener issued its ordered call');
    assert.equal(closed.length, 0, 'accepted backpressure does not close the session');
    assert.equal(
      received.some((frame) => frame.channel === 19 && frame.kind === KIND.credit),
      false,
      'credit is retained until the transport drains',
    );

    client.write = write;
    client.emit('drain');
    const returned = await Promise.race([
      credit,
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error('deferred event credit did not drain')), 200),
      ),
    ]);
    assert.equal(returned.payload, 1);
    peer.write(
      encode({
        channel: 2,
        kind: KIND.response,
        payload: {
          reply: 'workspace',
          with: { name: 'demo', image: 'alpine', architecture: 'amd64' },
        },
      }),
    );
    assert.equal((await call).reply, 'workspace');
    assert.equal(closed.length, 0);
    await session.close();
  } finally {
    peer?.destroy();
    client.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix event credit waits for slow async consumers and serializes delivery', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-async-event-credit-'));
  const socketPath = path.join(directory, 'host.sock');
  const returned = [];
  let peer;
  const server = net.createServer((socket) => {
    peer = socket;
    const reader = new Reader();
    socket.on('data', (chunk) => returned.push(...reader.take(chunk)));
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'slow_consumer', granted: [] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  const releases = [];
  const entered = [];
  let reportEntered;
  const entry = () => new Promise((resolve) => { reportEntered = resolve; });
  let nextEntry = entry();
  const session = await connect({
    path: socketPath,
    timeout: 1_000,
    onEvent: async (event) => {
      entered.push(event.slot);
      reportEntered();
      await new Promise((resolve) => releases.push(resolve));
    },
  });
  const promptly = (promise, label) =>
    Promise.race([
      promise,
      new Promise((_, reject) => setTimeout(() => reject(new Error(`${label} timed out`)), 200)),
    ]);
  try {
    peer.write(Buffer.concat([
      encode({ channel: 19, kind: KIND.event, payload: { pane_provider: 'one', slot: 'pane-1' } }),
      encode({ channel: 19, kind: KIND.event, payload: { pane_provider: 'two', slot: 'pane-2' } }),
    ]));
    await promptly(nextEntry, 'first async listener');
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(entered, ['pane-1']);
    assert.equal(returned.filter((frame) => frame.kind === KIND.credit).length, 0);

    nextEntry = entry();
    releases.shift()();
    await promptly(nextEntry, 'second async listener');
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(entered, ['pane-1', 'pane-2']);
    assert.equal(returned.filter((frame) => frame.kind === KIND.credit).length, 1);

    releases.shift()();
    for (let attempt = 0; attempt < 20 && returned.filter((frame) => frame.kind === KIND.credit).length < 2; attempt += 1)
      await new Promise((resolve) => setTimeout(resolve, 5));
    assert.equal(returned.filter((frame) => frame.kind === KIND.credit).length, 2);
  } finally {
    await session.close();
    peer?.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix half-close revokes queued event work from the old session generation', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-stale-event-generation-'));
  const socketPath = path.join(directory, 'host.sock');
  let peer;
  const server = net.createServer((socket) => {
    peer = socket;
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'stale_generation', granted: [] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  const session = await connect({ path: socketPath, timeout: 1_000 });
  const entered = [];
  let release;
  let reportEntered;
  const firstEntered = new Promise((resolve) => { reportEntered = resolve; });
  const dispose = session.onEvent(async (event) => {
    entered.push(event.slot);
    reportEntered();
    await new Promise((resolve) => { release = resolve; });
  });
  try {
    peer.write(Buffer.concat([
      encode({ channel: 23, kind: KIND.event, payload: { pane_provider: 'one', slot: 'pane-1' } }),
      encode({ channel: 23, kind: KIND.event, payload: { pane_provider: 'two', slot: 'pane-2' } }),
    ]));
    await Promise.race([
      firstEntered,
      new Promise((_, reject) => setTimeout(() => reject(new Error('first event timed out')), 200)),
    ]);

    dispose();
    peer.end();
    await session.closed;
    release();
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(entered, ['pane-1']);
  } finally {
    peer?.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('a mixed cross-surface event cannot masquerade as a pane selection over Unix framing', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-pane-selection-'));
  const socketPath = path.join(directory, 'host.sock');
  let peer;
  const returned = [];
  const server = net.createServer((socket) => {
    peer = socket;
    const reader = new Reader();
    socket.on('data', (chunk) => returned.push(...reader.take(chunk)));
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'selection', granted: [] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  const seen = [];
  let session;
  try {
    session = await connect({
      path: socketPath,
      timeout: 500,
      onEvent: (event) => seen.push(event),
    });
    peer.write(
      encode({
        channel: 19,
        kind: KIND.event,
        payload: {
          pane_provider: 'database',
          slot: 'pane-17',
          interaction: 'click',
          trigger: 'pointer',
          node: 7,
          id: 'foreign-action',
        },
      }),
    );
    const reason = await Promise.race([
      session.closed,
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error('ambiguous pane event did not close the session')), 200),
      ),
    ]);
    assert.match(reason.message, /exactly pane_provider and slot/);
    assert.deepEqual(seen, [], 'ambiguous cross-surface bytes never reach event listeners');
    assert.equal(
      returned.some((frame) => frame.channel === 19 && frame.kind === KIND.credit),
      false,
      'rejected bytes earn no event credit',
    );
  } finally {
    await session?.close();
    peer?.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('a real Unix reply on an uncorrelated channel fails the ordered session closed', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-channel-'));
  const socketPath = path.join(directory, 'host.sock');
  let peer;
  const server = net.createServer((socket) => {
    peer = socket;
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'channel', granted: ['workspaces:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath, timeout: 200 });
    const pending = session.call('workspace_info');
    peer.write(
      encode({
        channel: 4,
        kind: KIND.response,
        payload: {
          reply: 'workspace',
          with: { name: 'wrong', image: 'alpine', architecture: 'amd64' },
        },
      }),
    );
    await assert.rejects(pending, /unexpected 2 frame on channel 4/);
    await session.close();
  } finally {
    peer?.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('a real Unix response without a pending ordered call closes the session', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-unsolicited-response-'));
  const socketPath = path.join(directory, 'host.sock');
  let peer;
  const server = net.createServer((socket) => {
    peer = socket;
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'unsolicited', granted: ['workspaces:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    let reportClose;
    const closed = new Promise((resolve) => {
      reportClose = resolve;
    });
    const session = await connect({ path: socketPath, timeout: 200, onClose: reportClose });
    peer.write(
      encode({
        channel: 2,
        kind: KIND.response,
        payload: {
          reply: 'workspace',
          with: { name: 'wrong', image: 'alpine', architecture: 'amd64' },
        },
      }),
    );
    await Promise.race([
      closed,
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error('unsolicited response did not close session')), 200),
      ),
    ]);
    await assert.rejects(session.call('workspace_info'), /session is closed/);
    await session.close();
  } finally {
    peer?.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('a stalled real Unix peer cannot grow the shared call and ping ledger past its bound', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-ping-bound-'));
  const socketPath = path.join(directory, 'host.sock');
  const received = [];
  let reportPings;
  const pingsSeen = new Promise((resolve) => {
    reportPings = resolve;
  });
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      received.push(...reader.take(chunk));
      if (received.filter((frame) => frame.kind === KIND.ping).length === 2) reportPings();
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'ping_bound', granted: ['workspaces:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath, pendingLimit: 2, timeout: 1_000 });
    const first = session.ping();
    const second = session.ping();
    // Observe rejections immediately; the peer deliberately never answers the
    // two operations already occupying the bounded correlation ledger.
    const firstClosed = first.catch((error) => error);
    const secondClosed = second.catch((error) => error);
    await assert.rejects(session.ping(), /operation limit of 2 is exhausted/);
    await assert.rejects(session.call('workspace_info'), /call limit of 2 is exhausted/);
    await Promise.race([
      pingsSeen,
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error('the two admitted pings did not reach the peer')), 200),
      ),
    ]);
    assert.equal(
      received.filter((frame) => frame.kind === KIND.ping).length,
      2,
      'rejected operations never reach the transport',
    );
    await session.close();
    for (const error of await Promise.all([firstClosed, secondClosed])) {
      assert(error instanceof Error, 'closing rejects every retained ping');
    }
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix install wait inspects revision, arms inventory, then commits exact candidate', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-install-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const digest = `sha256:${'a'.repeat(64)}`;
  const candidate = {
    name: 'sample',
    version: '1',
    image_digest: digest,
    requested: ['extensions:read'],
    installed_image_digest: null,
  };
  const summary = {
    name: 'sample',
    image_digest: digest,
    version: '1',
    status: 'standby',
    enabled: false,
    pane_providers: [],
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (frame.payload.call === 'extension_acquisition_status')
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: {
                reply: 'extension_acquisition',
                with: {
                  job: 'job-1',
                  reference: 'sample:1',
                  revision: 7,
                  state: 'ready',
                  progress: null,
                  candidate,
                  error: null,
                },
              },
            }),
          );
        else if (frame.payload.call === 'extension_install') {
          assert.equal(frame.payload.with.image_digest, candidate.image_digest);
          socket.write(
            encode({
              channel: 21,
              kind: KIND.event,
              payload: { snapshot: 'extensions', of: [summary] },
            }),
          );
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'extension', with: summary },
            }),
          );
        } else
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'install-wait',
          granted: ['extensions:read', 'extensions:install'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).extensions.installAndWait('job-1', 7, [
      'extensions:read',
    ]);
    assert.equal(result.changed, true);
    assert.deepEqual(calls, [
      'extension_acquisition_status',
      'event_subscribe',
      'extension_install',
      'event_unsubscribe',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix container start wait arms first and ignores unchanged initial state', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-container-start-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const id = 'a'.repeat(32);
  const summary = (state) => ({ id, name: 'agent', image: 'alpine:3.20', state, created: 1 });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (frame.payload.call === 'event_subscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
          setImmediate(() =>
            socket.write(
              encode({
                channel: 31,
                kind: KIND.event,
                payload: { snapshot: 'containers', of: [summary('created')] },
              }),
            ),
          );
        } else if (frame.payload.call === 'container_start') {
          socket.write(
            encode({
              channel: 32,
              kind: KIND.event,
              payload: { snapshot: 'containers', of: [summary('created')] },
            }),
          );
          socket.write(
            encode({
              channel: 33,
              kind: KIND.event,
              payload: { snapshot: 'containers', of: [summary('running')] },
            }),
          );
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'container-start-wait',
          granted: ['containers:read', 'containers:create', 'containers:execute', 'containers:lifecycle', 'containers:remove'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).containers.startAndWait(id, 4);
    assert.equal(result.changed, true);
    assert.equal(result.container.state, 'running');
    assert.deepEqual(calls, ['event_subscribe', 'container_start', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix container stop wait arms first and ignores unchanged running state', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-container-stop-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const id = 'b'.repeat(64);
  const summary = (state) => ({ id, name: 'agent', image: 'alpine:3.20', state, created: 1 });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (frame.payload.call === 'event_subscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
          setImmediate(() =>
            socket.write(
              encode({
                channel: 34,
                kind: KIND.event,
                payload: { snapshot: 'containers', of: [summary('running')] },
              }),
            ),
          );
        } else if (frame.payload.call === 'container_stop') {
          socket.write(
            encode({
              channel: 35,
              kind: KIND.event,
              payload: { snapshot: 'containers', of: [summary('running')] },
            }),
          );
          socket.write(
            encode({
              channel: 36,
              kind: KIND.event,
              payload: { snapshot: 'containers', of: [summary('exited')] },
            }),
          );
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'container-stop-wait',
          granted: ['containers:read', 'containers:create', 'containers:execute', 'containers:lifecycle', 'containers:remove'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).containers.stopAndWait(id, 4);
    assert.equal(result.changed, true);
    assert.equal(result.container.state, 'exited');
    assert.deepEqual(calls, ['event_subscribe', 'container_stop', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix container remove wait rejects incomplete absence then accepts complete absence', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-container-remove-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  let completeAbsenceSent = false;
  const id = 'c'.repeat(64);
  const summary = { id, name: 'agent', image: 'alpine', state: 'exited', created: 1 };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        if (frame.payload.call === 'container_remove') {
          setImmediate(() => {
            socket.write(
              encode({
                channel: 40,
                kind: KIND.event,
                payload: {
                  snapshot: 'container_inventory',
                  of: { containers: [], complete: false },
                },
              }),
            );
            setImmediate(() => {
              socket.write(
                encode({
                  channel: 41,
                  kind: KIND.event,
                  payload: {
                    snapshot: 'container_inventory',
                    of: { containers: [summary], complete: true },
                  },
                }),
              );
              setTimeout(() => {
                completeAbsenceSent = true;
                socket.write(
                  encode({
                    channel: 42,
                    kind: KIND.event,
                    payload: {
                      snapshot: 'container_inventory',
                      of: { containers: [], complete: true },
                    },
                  }),
                );
              }, 20);
            });
          });
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'remove-wait',
          granted: ['containers:read', 'containers:create', 'containers:execute', 'containers:lifecycle', 'containers:remove'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    assert.deepEqual(await workspace(session).containers.removeAndWait(id, 4), {
      changed: true,
      id,
    });
    assert.equal(completeAbsenceSent, true, 'incomplete absence cannot settle removal');
    assert.deepEqual(calls, ['event_subscribe', 'container_remove', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix restart wait requires the same container at a newer running generation', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-container-restart-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const id = 'd'.repeat(64);
  const summary = (state, generation) => ({
    id,
    name: 'agent',
    image: 'alpine',
    state,
    created: 1,
    generation,
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        if (frame.payload.call === 'container_restart') {
          socket.write(
            encode({
              channel: 45,
              kind: KIND.event,
              payload: { snapshot: 'containers', of: [summary('running', 7)] },
            }),
          );
          socket.write(
            encode({
              channel: 46,
              kind: KIND.event,
              payload: { snapshot: 'containers', of: [summary('exited', 8)] },
            }),
          );
          socket.write(
            encode({
              channel: 47,
              kind: KIND.event,
              payload: { snapshot: 'containers', of: [summary('running', 8)] },
            }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'restart-wait',
          granted: ['containers:read', 'containers:create', 'containers:execute', 'containers:lifecycle', 'containers:remove'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).containers.restartAndWait(id, 7);
    assert.equal(result.changed, true);
    assert.equal(result.container.generation, 8);
    assert.equal(result.container.state, 'running');
    assert.deepEqual(calls, ['event_subscribe', 'container_restart', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix splitAndWait arms before CAS, verifies the returned slot, and disposes on timeout', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-split-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const pane = {
    slot: 'pane-new',
    generation: 1,
    revision: 1,
    kind: 'terminal',
    provider: null,
    tab: 'tab-1',
    title: 'Shell',
    focused: false,
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (
          frame.payload.call === 'event_subscribe' ||
          frame.payload.call === 'event_unsubscribe'
        ) {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'terminal_split_observed') {
          assert.deepEqual(frame.payload.with, {
            slot: 'pane-source',
            generation: 4,
            revision: 9,
            division: frame.payload.with.division,
          });
          if (frame.payload.with.division === 'beside')
            socket.write(
              encode({
                channel: 61,
                kind: KIND.event,
                payload: {
                  snapshot: 'pane_changes',
                  of: {
                    slot: 'pane-source',
                    kind: 'terminal',
                    generation: 4,
                    revision: 10,
                    coalesced: 0,
                  },
                },
              }),
            );
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: {
                reply: 'identity',
                with: frame.payload.with.division === 'beside' ? 'pane-new' : 'pane-timeout',
              },
            }),
          );
        } else if (frame.payload.call === 'pane_list') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'panes', with: { panes: [pane], truncated: false } },
            }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'split-wait',
          granted: ['panes:observe', 'terminals:layout-control'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const terminal = workspace(session).terminal;
    await assert.rejects(
      terminal.splitAndWait('pane-source', 4, 9, 'beside', { timeoutMs: 0 }),
      /timeout/,
    );
    assert.deepEqual(calls, [], 'invalid timeout must not subscribe or mutate');
    assert.deepEqual(await terminal.splitAndWait('pane-source', 4, 9, 'beside'), {
      changed: true,
      pane,
    });
    assert.deepEqual(calls, [
      'event_subscribe',
      'terminal_split_observed',
      'pane_list',
      'event_unsubscribe',
    ]);
    calls.length = 0;
    assert.deepEqual(await terminal.splitAndWait('pane-source', 4, 9, 'below', { timeoutMs: 5 }), {
      changed: false,
      slot: 'pane-timeout',
      after: { generation: 4, revision: 9 },
    });
    assert.deepEqual(calls, ['event_subscribe', 'terminal_split_observed', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix closeAndWait requires complete absence and disposes success and timeout', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-close-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const slot = 'pane-close';
  let inventoryReads = 0;
  let closeRequests = 0;
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (
          frame.payload.call === 'event_subscribe' ||
          frame.payload.call === 'event_unsubscribe'
        ) {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'terminal_close_pane_observed') {
          closeRequests += 1;
          assert.deepEqual(frame.payload.with, { slot, generation: 6, revision: 10 });
          if (closeRequests === 1) {
            socket.write(
              encode({
                channel: 70,
                kind: KIND.event,
                payload: {
                  snapshot: 'pane_changes',
                  of: {
                    slot,
                    kind: 'terminal',
                    generation: 6,
                    revision: 11,
                    coalesced: 0,
                  },
                },
              }),
            );
          }
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'pane_list') {
          inventoryReads += 1;
          const incomplete = inventoryReads === 1;
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: {
                reply: 'panes',
                with: {
                  panes: [],
                  truncated: incomplete,
                },
              },
            }),
          );
          if (incomplete)
            setTimeout(
              () =>
                socket.write(
                  encode({
                    channel: 71,
                    kind: KIND.event,
                    payload: {
                      snapshot: 'pane_changes',
                      of: { slot, kind: 'terminal', generation: 6, revision: 12, coalesced: 0 },
                    },
                  }),
                ),
              1,
            );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'close-wait',
          granted: ['panes:observe', 'terminals:layout-control'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const terminal = workspace(session).terminal;
    await assert.rejects(terminal.closeAndWait(slot, 6, 10, { timeoutMs: 0 }), /timeout/);
    assert.deepEqual(calls, [], 'invalid timeout must not subscribe or close');
    assert.deepEqual(await terminal.closeAndWait(slot, 6, 10), { changed: true, slot });
    assert.deepEqual(calls, [
      'event_subscribe',
      'terminal_close_pane_observed',
      'pane_list',
      'pane_list',
      'event_unsubscribe',
    ]);
    calls.length = 0;
    assert.deepEqual(await terminal.closeAndWait(slot, 6, 10, { timeoutMs: 5 }), {
      changed: false,
      slot,
      after: { generation: 6, revision: 10 },
    });
    assert.deepEqual(calls, [
      'event_subscribe',
      'terminal_close_pane_observed',
      'event_unsubscribe',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix retitleAndWait arms before CAS and verifies exact title and revision', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-retitle-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const slot = 'pane-title';
  let retitles = 0;
  const pane = {
    slot,
    generation: 8,
    revision: 15,
    kind: 'terminal',
    provider: null,
    tab: 'tab-1',
    title: 'Build logs',
    focused: true,
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (
          frame.payload.call === 'event_subscribe' ||
          frame.payload.call === 'event_unsubscribe'
        ) {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'terminal_retitle_pane_observed') {
          retitles += 1;
          assert.deepEqual(frame.payload.with, {
            slot,
            generation: 8,
            revision: 14,
            title: 'Build logs',
          });
          if (retitles === 1)
            socket.write(
              encode({
                channel: 80,
                kind: KIND.event,
                payload: {
                  snapshot: 'pane_changes',
                  of: { slot, kind: 'terminal', generation: 8, revision: 15, coalesced: 0 },
                },
              }),
            );
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'pane_list') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'panes', with: { panes: [pane], truncated: false } },
            }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'retitle-wait',
          granted: ['panes:observe', 'terminals:layout-control'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const terminal = workspace(session).terminal;
    await assert.rejects(
      terminal.retitleAndWait(slot, 8, 14, 'Build logs', { timeoutMs: 0 }),
      /timeout/,
    );
    assert.deepEqual(calls, [], 'invalid timeout must not subscribe or retitle');
    assert.deepEqual(await terminal.retitleAndWait(slot, 8, 14, 'Build logs'), {
      changed: true,
      pane,
    });
    assert.deepEqual(calls, [
      'event_subscribe',
      'terminal_retitle_pane_observed',
      'pane_list',
      'event_unsubscribe',
    ]);
    calls.length = 0;
    assert.deepEqual(await terminal.retitleAndWait(slot, 8, 14, 'Build logs', { timeoutMs: 5 }), {
      changed: false,
      title: 'Build logs',
      after: { generation: 8, revision: 14 },
    });
    assert.deepEqual(calls, [
      'event_subscribe',
      'terminal_retitle_pane_observed',
      'event_unsubscribe',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix focusAndWait arms before CAS and verifies exact focused pane identity', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-focus-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const slot = 'pane-focus';
  let focuses = 0;
  const pane = {
    slot,
    generation: 3,
    revision: 6,
    kind: 'terminal',
    provider: null,
    tab: 'tab-1',
    title: 'Shell',
    focused: true,
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (
          frame.payload.call === 'event_subscribe' ||
          frame.payload.call === 'event_unsubscribe'
        ) {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'terminal_focus_pane_observed') {
          focuses += 1;
          assert.deepEqual(frame.payload.with, { slot, generation: 3, revision: 5 });
          if (focuses === 1)
            socket.write(
              encode({
                channel: 90,
                kind: KIND.event,
                payload: {
                  snapshot: 'pane_changes',
                  of: { slot, kind: 'terminal', generation: 3, revision: 6, coalesced: 0 },
                },
              }),
            );
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'pane_list') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'panes', with: { panes: [pane], truncated: false } },
            }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'focus-wait',
          granted: ['panes:observe', 'terminals:layout-control'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const terminal = workspace(session).terminal;
    await assert.rejects(terminal.focusAndWait(slot, 3, 5, { timeoutMs: 0 }), /timeout/);
    assert.deepEqual(calls, [], 'invalid timeout must not subscribe or focus');
    assert.deepEqual(await terminal.focusAndWait(slot, 3, 5), { changed: true, pane });
    assert.deepEqual(calls, [
      'event_subscribe',
      'terminal_focus_pane_observed',
      'pane_list',
      'event_unsubscribe',
    ]);
    calls.length = 0;
    assert.deepEqual(await terminal.focusAndWait(slot, 3, 5, { timeoutMs: 5 }), {
      changed: false,
      slot,
      after: { generation: 3, revision: 5 },
    });
    assert.deepEqual(calls, [
      'event_subscribe',
      'terminal_focus_pane_observed',
      'event_unsubscribe',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix writeAndWait subscribes and reads before bytes, then returns advanced screen', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-write-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const slot = 'pane-input';
  let writes = 0;
  let reads = 0;
  const screen = (revision, lines) => ({
    slot,
    generation: 4,
    revision,
    columns: 80,
    rows: 24,
    lines,
    cursor_column: 0,
    cursor_row: 1,
    truncated: false,
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (
          frame.payload.call === 'event_subscribe' ||
          frame.payload.call === 'event_unsubscribe'
        ) {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'terminal_read_pane') {
          reads += 1;
          assert.deepEqual(frame.payload.with, { slot, lines: 20 });
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: {
                reply: 'text',
                with: screen(reads === 1 ? 7 : 8, reads === 1 ? ['$ '] : ['$ ^C']),
              },
            }),
          );
        } else if (frame.payload.call === 'terminal_write_pane') {
          writes += 1;
          assert.deepEqual(frame.payload.with, {
            slot,
            generation: 4,
            revision: 7,
            contents: writes === 1 ? [0, 3, 255] : [3],
          });
          if (writes === 1)
            socket.write(
              encode({
                channel: 100,
                kind: KIND.event,
                payload: {
                  snapshot: 'pane_changes',
                  of: { slot, kind: 'terminal', generation: 4, revision: 8, coalesced: 0 },
                },
              }),
            );
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'write-wait',
          granted: ['panes:observe', 'terminals:output', 'terminals:input'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const terminal = workspace(session).terminal;
    await assert.rejects(terminal.writeAndWait(slot, 4, 7, [256], { lines: 20 }), /0 through 255/);
    await assert.rejects(
      terminal.writeAndWait(slot, 4, 7, [3], { lines: 20, timeoutMs: 0 }),
      /timeout/,
    );
    assert.deepEqual(calls, [], 'invalid input must not subscribe, read, or write');
    const result = await terminal.writeAndWait(slot, 4, 7, [0, 3, 255], { lines: 20 });
    assert.equal(result.changed, true);
    assert.equal(result.before.revision, 7);
    assert.equal(result.after.revision, 8);
    assert.deepEqual(calls, [
      'event_subscribe',
      'terminal_read_pane',
      'terminal_write_pane',
      'terminal_read_pane',
      'event_unsubscribe',
    ]);
    calls.length = 0;
    reads = 0;
    assert.deepEqual(await terminal.writeAndWait(slot, 4, 7, [3], { lines: 20, timeoutMs: 5 }), {
      changed: false,
      before: screen(7, ['$ ']),
    });
    assert.deepEqual(calls, [
      'event_subscribe',
      'terminal_read_pane',
      'terminal_write_pane',
      'event_unsubscribe',
    ]);
    calls.length = 0;
    reads = 0;
    const cancellation = new AbortController();
    const cancelled = terminal.writeObservedAndWait(screen(7, ['$ ']), [3], {
      lines: 20,
      timeoutMs: 1_000,
      signal: cancellation.signal,
    });
    setTimeout(() => cancellation.abort('agent stopped'), 5);
    await assert.rejects(cancelled, (error) => error?.name === 'AbortError');
    assert.deepEqual(calls, [
      'event_subscribe',
      'terminal_read_pane',
      'terminal_write_pane',
      'event_unsubscribe',
    ]);
    assert.equal((await terminal.read(slot, 20)).revision, 8);
    assert.equal(calls.at(-1), 'terminal_read_pane', 'the session remains usable after cancellation');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix signalExecutionAndWait ignores initial state and awaits exact immutable transition', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-signal-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const id = 'e'.repeat(32);
  let signals = 0;
  const summary = (running, exitCode, pid = 42) => ({
    id,
    container_id: 'c'.repeat(64),
    running,
    exit_code: exitCode,
    pid,
    command: ['sleep', '30'],
    user: 'root',
  });
  const initial = summary(true, 0);
  const exited = summary(false, 143);
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (frame.payload.call === 'event_subscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
          socket.write(
            encode({
              channel: 110,
              kind: KIND.event,
              payload: { snapshot: 'executions', of: { executions: [initial], truncated: false } },
            }),
          );
        } else if (frame.payload.call === 'event_unsubscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'execution_inspect') {
          assert.deepEqual(frame.payload.with, { id });
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'execution', with: initial },
            }),
          );
        } else if (frame.payload.call === 'execution_kill') {
          signals += 1;
          assert.deepEqual(frame.payload.with, { id, signal: 'SIGTERM' });
          if (signals === 1)
            socket.write(
              encode({
                channel: 111,
                kind: KIND.event,
                payload: { snapshot: 'executions', of: { executions: [exited], truncated: false } },
              }),
            );
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'signal-wait',
          granted: ['containers:read', 'containers:create', 'containers:execute', 'containers:lifecycle', 'containers:remove'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    const cursor = { running: true, exit_code: 0, pid: 42 };
    await assert.rejects(containers.signalExecutionAndWait(id, 'SIG TERM', cursor), /signal/);
    await assert.rejects(
      containers.signalExecutionAndWait(id, 'SIGTERM', cursor, { timeoutMs: 0 }),
      /timeout/,
    );
    assert.deepEqual(calls, [], 'invalid signal or timeout must not subscribe or signal');
    assert.deepEqual(await containers.signalExecutionAndWait(id, 'SIGTERM', cursor), {
      changed: true,
      execution: exited,
    });
    assert.deepEqual(calls, [
      'event_subscribe',
      'execution_inspect',
      'execution_kill',
      'event_unsubscribe',
    ]);
    calls.length = 0;
    assert.deepEqual(
      await containers.signalExecutionAndWait(id, 'SIGTERM', cursor, { timeoutMs: 5 }),
      {
        changed: false,
        id,
        state: 'exited',
        after: cursor,
      },
    );
    assert.deepEqual(calls, [
      'event_subscribe',
      'execution_inspect',
      'execution_kill',
      'event_unsubscribe',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix removeExecutionAndWait requires a finished cursor and complete later absence', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-execution-remove-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const id = 'f'.repeat(32);
  let removals = 0;
  const finished = {
    id,
    container_id: 'c'.repeat(64),
    running: false,
    exit_code: 0,
    pid: 0,
    command: ['true'],
    user: 'root',
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (
          frame.payload.call === 'event_subscribe' ||
          frame.payload.call === 'event_unsubscribe'
        ) {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'execution_inspect') {
          assert.deepEqual(frame.payload.with, { id });
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'execution', with: finished },
            }),
          );
        } else if (frame.payload.call === 'execution_remove') {
          removals += 1;
          assert.deepEqual(frame.payload.with, { id });
          if (removals === 1) {
            socket.write(
              encode({
                channel: 120,
                kind: KIND.event,
                payload: { snapshot: 'executions', of: { executions: [], truncated: true } },
              }),
            );
            socket.write(
              encode({
                channel: 121,
                kind: KIND.event,
                payload: { snapshot: 'executions', of: { executions: [], truncated: false } },
              }),
            );
          }
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'execution-remove-wait',
          granted: ['containers:read', 'containers:create', 'containers:execute', 'containers:lifecycle', 'containers:remove'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    const cursor = { running: false, exit_code: 0, pid: 0 };
    await assert.rejects(
      containers.removeExecutionAndWait(id, { ...cursor, running: true }),
      /finished/,
    );
    await assert.rejects(
      containers.removeExecutionAndWait(id, cursor, { timeoutMs: 0 }),
      /timeout/,
    );
    assert.deepEqual(calls, [], 'invalid cursor or timeout must not subscribe, inspect, or remove');
    assert.deepEqual(await containers.removeExecutionAndWait(id, cursor), { changed: true, id });
    assert.deepEqual(calls, [
      'event_subscribe',
      'execution_inspect',
      'execution_remove',
      'event_unsubscribe',
    ]);
    calls.length = 0;
    assert.deepEqual(await containers.removeExecutionAndWait(id, cursor, { timeoutMs: 5 }), {
      changed: false,
      id,
    });
    assert.deepEqual(calls, [
      'event_subscribe',
      'execution_inspect',
      'execution_remove',
      'event_unsubscribe',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix spawnAndWait subscribes and reads before CAS argv, then returns advanced screen', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-spawn-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const slot = 'pane-spawn';
  let spawns = 0;
  let reads = 0;
  const screen = (revision, lines) => ({
    slot,
    generation: 5,
    revision,
    columns: 80,
    rows: 24,
    lines,
    cursor_column: 0,
    cursor_row: 1,
    truncated: false,
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (
          frame.payload.call === 'event_subscribe' ||
          frame.payload.call === 'event_unsubscribe'
        ) {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'terminal_read_pane') {
          reads += 1;
          assert.deepEqual(frame.payload.with, { slot, lines: 40 });
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: {
                reply: 'text',
                with: screen(reads === 1 ? 9 : 10, reads === 1 ? ['$ '] : ['monitor']),
              },
            }),
          );
        } else if (frame.payload.call === 'terminal_spawn_observed') {
          spawns += 1;
          assert.deepEqual(frame.payload.with, {
            slot,
            generation: 5,
            revision: 9,
            command: ['monitor', '--once'],
          });
          if (spawns === 1)
            socket.write(
              encode({
                channel: 120,
                kind: KIND.event,
                payload: {
                  snapshot: 'pane_changes',
                  of: { slot, kind: 'terminal', generation: 5, revision: 10, coalesced: 0 },
                },
              }),
            );
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'spawn-wait',
          granted: ['panes:observe', 'terminals:output', 'terminals:process-control'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const terminal = workspace(session).terminal;
    await assert.rejects(terminal.spawnAndWait(slot, 5, 9, [], { lines: 40 }), /command/);
    await assert.rejects(
      terminal.spawnAndWait(slot, 5, 9, ['monitor'], { lines: 40, timeoutMs: 0 }),
      /timeout/,
    );
    assert.deepEqual(calls, [], 'invalid argv or timeout must not subscribe, read, or spawn');
    const result = await terminal.spawnAndWait(slot, 5, 9, ['monitor', '--once'], { lines: 40 });
    assert.equal(result.changed, true);
    assert.equal(result.before.revision, 9);
    assert.equal(result.after.revision, 10);
    assert.deepEqual(calls, [
      'event_subscribe',
      'terminal_read_pane',
      'terminal_spawn_observed',
      'terminal_read_pane',
      'event_unsubscribe',
    ]);
    calls.length = 0;
    reads = 0;
    assert.deepEqual(
      await terminal.spawnAndWait(slot, 5, 9, ['monitor', '--once'], { lines: 40, timeoutMs: 5 }),
      {
        changed: false,
        command: ['monitor', '--once'],
        before: screen(9, ['$ ']),
      },
    );
    assert.deepEqual(calls, [
      'event_subscribe',
      'terminal_read_pane',
      'terminal_spawn_observed',
      'event_unsubscribe',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix openTabAndWait arms before creation and verifies returned tab identity', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-open-tab-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  let opens = 0;
  const pane = {
    slot: 'surface-1',
    generation: 1,
    revision: 1,
    kind: 'surface',
    provider: { extension: 'demo', provider: 'main' },
    tab: 'tab-owned',
    title: 'Agent tools',
    focused: false,
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (
          frame.payload.call === 'event_subscribe' ||
          frame.payload.call === 'event_unsubscribe'
        ) {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'terminal_open_tab') {
          opens += 1;
          assert.deepEqual(frame.payload.with, { title: 'Agent tools' });
          if (opens === 1)
            socket.write(
              encode({
                channel: 130,
                kind: KIND.event,
                payload: {
                  snapshot: 'pane_changes',
                  of: {
                    slot: 'surface-1',
                    kind: 'surface',
                    generation: 1,
                    revision: 1,
                    coalesced: 0,
                  },
                },
              }),
            );
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'identity', with: 'tab-owned' },
            }),
          );
        } else if (frame.payload.call === 'pane_list') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'panes', with: { panes: [pane], truncated: false } },
            }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'open-tab-wait',
          granted: ['panes:observe', 'terminals:layout-control'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const terminal = workspace(session).terminal;
    await assert.rejects(terminal.openTabAndWait('\n', { timeoutMs: 10 }), /title/);
    await assert.rejects(terminal.openTabAndWait('Agent tools', { timeoutMs: 0 }), /timeout/);
    assert.deepEqual(calls, [], 'invalid title or timeout must not subscribe or open');
    assert.deepEqual(await terminal.openTabAndWait('Agent tools'), {
      changed: true,
      tab: 'tab-owned',
      pane,
    });
    assert.deepEqual(calls, [
      'event_subscribe',
      'terminal_open_tab',
      'pane_list',
      'event_unsubscribe',
    ]);
    calls.length = 0;
    assert.deepEqual(await terminal.openTabAndWait('Agent tools', { timeoutMs: 5 }), {
      changed: false,
      tab: 'tab-owned',
      title: 'Agent tools',
    });
    assert.deepEqual(calls, ['event_subscribe', 'terminal_open_tab', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix openTabAndWait retains the created tab when inventory verification fails', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-open-tab-recovery-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (
          frame.payload.call === 'event_subscribe' ||
          frame.payload.call === 'event_unsubscribe'
        ) {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'terminal_open_tab') {
          socket.write(
            encode({
              channel: 130,
              kind: KIND.event,
              payload: {
                snapshot: 'pane_changes',
                of: {
                  slot: 'new-pane',
                  kind: 'terminal',
                  generation: 1,
                  revision: 1,
                  coalesced: 0,
                },
              },
            }),
          );
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'identity', with: 'tab-created' },
            }),
          );
        } else if (frame.payload.call === 'pane_list') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              flags: 3,
              payload: { error: 'failed', detail: 'inventory unavailable' },
            }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'open-tab-recovery',
          granted: ['panes:observe', 'terminals:layout-control'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    await assert.rejects(workspace(session).terminal.openTabAndWait('Recovery'), (error) => {
      assert(error instanceof TerminalOperationError);
      assert.equal(error.operation, 'open-tab');
      assert.deepEqual(error.result, { tab: 'tab-created', title: 'Recovery' });
      assert.equal(error.cause?.kind, 'failed');
      return true;
    });
    assert.deepEqual(calls, [
      'event_subscribe',
      'terminal_open_tab',
      'pane_list',
      'event_unsubscribe',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix inspectAndAct validates live semantic authority before revision-bound action', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-inspect-act-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  let reads = 0;
  const slot = 'settings';
  const tree = (revision, value) => ({
    slot,
    generation: 2,
    revision,
    truncated: false,
    root: {
      id: 0,
      role: 'page',
      label: 'Settings',
      value: null,
      disabled: false,
      destructive: false,
      actions: [],
      children: [
        {
          id: 7,
          role: 'button',
          label: 'Toggle',
          value,
          disabled: false,
          destructive: false,
          actions: ['invoke'],
          children: [],
        },
      ],
    },
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (
          frame.payload.call === 'event_subscribe' ||
          frame.payload.call === 'event_unsubscribe'
        ) {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'pane_semantic_read') {
          reads += 1;
          assert.deepEqual(frame.payload.with, { slot });
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: {
                reply: 'semantics',
                with: tree(reads === 1 ? 4 : 5, reads === 1 ? 'off' : 'on'),
              },
            }),
          );
        } else if (frame.payload.call === 'pane_semantic_action') {
          assert.deepEqual(frame.payload.with, {
            slot,
            action: { generation: 2, revision: 4, node: 7, action: 'invoke', value: null },
          });
          socket.write(
            encode({
              channel: 140,
              kind: KIND.event,
              payload: {
                snapshot: 'pane_changes',
                of: {
                  slot,
                  kind: 'native',
                  generation: 2,
                  revision: 5,
                  coalesced: 0,
                },
              },
            }),
          );
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'inspect-act',
          granted: ['panes:observe', 'panes:semantic-read', 'panes:semantic-control'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const terminal = workspace(session).terminal;
    await assert.rejects(terminal.inspectAndAct(slot, { node: -1, action: 'invoke' }), /node/);
    await assert.rejects(
      terminal.inspectAndAct(slot, { node: 7, action: 'invoke' }, { timeoutMs: 0 }),
      /timeout/,
    );
    assert.deepEqual(calls, [], 'invalid proposal or timeout must not subscribe or inspect');
    const result = await terminal.inspectAndAct(slot, { node: 7, action: 'invoke' });
    assert.equal(result.changed, true);
    assert.equal(result.before.snapshot.revision, 4);
    assert.equal(result.after.snapshot.revision, 5);
    assert.match(result.before.text, /<value>off<\/value>/);
    assert.match(result.after.text, /<value>on<\/value>/);
    assert.deepEqual(calls, [
      'event_subscribe',
      'pane_semantic_read',
      'pane_semantic_action',
      'pane_semantic_read',
      'event_unsubscribe',
    ]);
    calls.length = 0;
    await assert.rejects(
      terminal.inspectAndAct(slot, { node: 7, action: 'toggle' }),
      /does not advertise/,
    );
    assert.deepEqual(
      calls,
      ['event_subscribe', 'pane_semantic_read', 'event_unsubscribe'],
      'non-advertised action must not reach authority',
    );
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix execAndWait prevalidates then executes, waits, and reads bounded output in order', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-exec-and-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const containerId = 'c'.repeat(64);
  const executionId = 'e'.repeat(32);
  const execution = {
    id: executionId,
    container_id: containerId,
    running: false,
    exit_code: 0,
    pid: 22,
    command: ['printf', 'ok'],
    user: 'root',
  };
  const output = {
    stdout: [111, 107],
    stderr: [],
    truncated: false,
    stdout_truncated: false,
    stderr_truncated: false,
    eof: true,
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (frame.payload.call === 'container_exec') {
          assert.deepEqual(frame.payload.with, {
            id: containerId,
            generation: 4,
            command: ['printf', 'ok'],
            environment: [],
            user: 'root',
            working_directory: '/tmp',
          });
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'identity', with: executionId },
            }),
          );
        } else if (frame.payload.call === 'execution_wait') {
          assert.deepEqual(frame.payload.with, { id: executionId, timeout_ms: 321 });
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'execution', with: execution },
            }),
          );
        } else if (frame.payload.call === 'execution_logs') {
          assert.deepEqual(frame.payload.with, { id: executionId, stdout: true, stderr: false });
          socket.write(
            encode({ channel: 2, kind: KIND.response, payload: { reply: 'logs', with: output } }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'exec-wait',
          granted: ['containers:read', 'containers:create', 'containers:execute', 'containers:lifecycle', 'containers:remove'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    await assert.rejects(
      containers.execAndWait(containerId, 4, { command: ['true'], timeoutMs: 0 }),
      /timeout/,
    );
    await assert.rejects(
      containers.execAndWait(containerId, 4, { command: ['true'], stdout: false, stderr: false }),
      /at least one/,
    );
    assert.deepEqual(calls, [], 'invalid later-stage options must not create an execution');
    assert.deepEqual(
      await containers.execAndWait(containerId, 4, {
        command: ['printf', 'ok'],
        user: 'root',
        workingDirectory: '/tmp',
        timeoutMs: 321,
        stderr: false,
      }),
      { execution, output },
    );
    assert.deepEqual(calls, ['container_exec', 'execution_wait', 'execution_logs']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix execAndWait preserves execution identity when waiting fails and never removes it', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-exec-wait-failure-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const containerId = 'a'.repeat(32);
  const executionId = 'b'.repeat(32);
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (frame.payload.call === 'container_exec') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'identity', with: executionId },
            }),
          );
        } else if (frame.payload.call === 'execution_wait') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              flags: 3,
              payload: { error: 'failed', detail: 'wait timed out' },
            }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'exec-wait-failure',
          granted: ['containers:read', 'containers:create', 'containers:execute', 'containers:lifecycle', 'containers:remove'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    await assert.rejects(
      workspace(session).containers.execAndWait(containerId, 4, {
        command: ['sleep', '1'],
        timeoutMs: 1,
      }),
      (error) => {
        assert(error instanceof ExecutionOperationError);
        assert.equal(error.executionId, executionId);
        assert.equal(error.phase, 'wait');
        assert.equal(error.cause?.kind, 'failed');
        return true;
      },
    );
    assert.deepEqual(calls, ['container_exec', 'execution_wait']);
    assert(!calls.includes('execution_remove'));
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix execAndWait preserves the completed execution when bounded log retrieval fails', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-exec-log-failure-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const containerId = 'c'.repeat(64);
  const executionId = 'd'.repeat(32);
  const execution = {
    id: executionId,
    container_id: containerId,
    running: false,
    exit_code: 7,
    pid: 42,
    command: ['false'],
    user: 'root',
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (frame.payload.call === 'container_exec') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'identity', with: executionId },
            }),
          );
        } else if (frame.payload.call === 'execution_wait') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'execution', with: execution },
            }),
          );
        } else if (frame.payload.call === 'execution_logs') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              flags: 3,
              payload: { error: 'failed', detail: 'logs unavailable' },
            }),
          );
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'exec-log-failure',
          granted: ['containers:read', 'containers:create', 'containers:execute', 'containers:lifecycle', 'containers:remove'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    await assert.rejects(
      workspace(session).containers.execAndWait(containerId, 4, { command: ['false'] }),
      (error) => {
        assert(error instanceof ExecutionOperationError);
        assert.equal(error.executionId, executionId);
        assert.equal(error.phase, 'logs');
        assert.deepEqual(error.execution, execution);
        assert.equal(error.cause?.kind, 'failed');
        return true;
      },
    );
    assert.deepEqual(calls, ['container_exec', 'execution_wait', 'execution_logs']);
    assert(!calls.includes('execution_remove'));
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
