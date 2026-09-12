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
  ExecutionDeadlineError,
  ExecutionOperationError,
  ExecutionOutputGapError,
  ExecutionOutputProtocolError,
  FileIdentityChangedError,
  FilesystemJournalGapError,
  JsonLineDecodeError,
  JsonLineParseError,
  PaneInventoryChangedError,
  PaneUnavailableError,
  Session,
  StateDecodeError,
  TerminalOperationError,
  workspace,
} from '../dist/index.js';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

const FILE_JOURNAL = '0123456789abcdef0123456789abcdef';
const REPLACEMENT_FILE_JOURNAL = 'fedcba9876543210fedcba9876543210';

test('real Unix filesystem catch-up is bounded, resumable, and journal-gap safe', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-file-catch-up-'));
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
        const input = frame.payload.with;
        let payload;
        if (frame.payload.call !== 'filesystem_changes') {
          payload = {
            reply: 'workspace',
            with: { name: 'index', image: 'toolbox', architecture: 'amd64' },
          };
        } else if (input.observed === FILE_JOURNAL && input.after < 2) {
          const revision = input.after + 1;
          payload = {
            reply: 'file_changes',
            with: {
              changes: [
                {
                  revision,
                  kind: revision === 1 ? 'create' : 'modify',
                  path: 'src/a.ts',
                  entry: {
                    path: 'src/a.ts',
                    directory: false,
                    size: revision * 4,
                    identity: `a-v${revision}`,
                  },
                },
              ],
              journal: FILE_JOURNAL,
              next: revision,
              current: 4,
              more: true,
              truncated: false,
            },
          };
        } else if (input.observed === FILE_JOURNAL) {
          payload = {
            reply: 'file_changes',
            with: {
              changes: [],
              journal: REPLACEMENT_FILE_JOURNAL,
              next: 8,
              current: 8,
              more: false,
              truncated: true,
            },
          };
        } else {
          payload = {
            reply: 'file_changes',
            with: {
              changes: [
                { revision: 9, kind: 'remove', path: 'src/old.ts', entry: null },
              ],
              journal: REPLACEMENT_FILE_JOURNAL,
              next: 9,
              current: 9,
              more: false,
              truncated: false,
            },
          };
        }
        const response = encode({ channel: frame.channel, kind: KIND.response, payload });
        socket.write(response.subarray(0, 5));
        socket.write(response.subarray(5));
      }
    });
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'embeddings-catch-up',
        granted: ['filesystem:read', 'workspaces:read'],
      },
    });
    socket.write(greeting.subarray(0, 3));
    socket.write(greeting.subarray(3));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const files = workspace(session).files;
    await assert.rejects(
      files.catchUpChanges({
        cursor: { journal: FILE_JOURNAL, revision: 0 },
        maxPages: 0,
      }),
      /maxPages.*1.*256/,
    );
    const controller = new AbortController();
    controller.abort('indexer stopped');
    await assert.rejects(
      files.catchUpChanges({
        cursor: { journal: FILE_JOURNAL, revision: 0 },
        signal: controller.signal,
      }),
      (error) => error.name === 'AbortError' && error.cause === controller.signal.reason,
    );
    assert.deepEqual(requests, [], 'invalid and cancelled catch-up must not read history');

    const partial = await files.catchUpChanges({
      cursor: { journal: FILE_JOURNAL, revision: 0 },
      pageSize: 1,
      maxChanges: 4,
      maxPages: 2,
    });
    assert.equal(partial.caughtUp, false);
    assert.deepEqual(partial.cursor, { journal: FILE_JOURNAL, revision: 2 });
    assert.equal(partial.current, 4);
    assert.deepEqual(
      partial.changes.map(({ revision, kind }) => [revision, kind]),
      [
        [1, 'create'],
        [2, 'modify'],
      ],
    );

    let recovery;
    await assert.rejects(files.catchUpChanges({ cursor: partial.cursor, pageSize: 1 }), (error) => {
      assert(error instanceof FilesystemJournalGapError);
      assert.deepEqual(error.requested, partial.cursor);
      assert.deepEqual(error.replacement, {
        journal: REPLACEMENT_FILE_JOURNAL,
        revision: 8,
      });
      recovery = error.replacement;
      return true;
    });
    const resumed = await files.catchUpChanges({ cursor: recovery, pageSize: 1 });
    assert.equal(resumed.caughtUp, true);
    assert.deepEqual(resumed.cursor, {
      journal: REPLACEMENT_FILE_JOURNAL,
      revision: 9,
    });
    assert.deepEqual(resumed.changes.map(({ kind, path }) => [kind, path]), [
      ['remove', 'src/old.ts'],
    ]);
    assert.equal((await workspace(session).info()).name, 'index');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('filesystem catch-up aborts a stalled ordered read and reconnects cleanly', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-filesystem-abort-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  let connectionNumber = 0;
  let requested;
  const requestArrived = new Promise((resolve) => {
    requested = resolve;
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    connectionNumber += 1;
    const current = connectionNumber;
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        if (current === 1) {
          assert.equal(frame.payload.call, 'filesystem_changes');
          requested();
          continue;
        }
        assert.equal(frame.payload.call, 'workspace_info');
        const response = encode({
          channel: 2,
          kind: KIND.response,
          payload: {
            reply: 'workspace',
            with: { name: 'reconnected-indexer', image: 'toolbox', architecture: 'amd64' },
          },
        });
        socket.write(response.subarray(0, 7));
        socket.write(response.subarray(7));
      }
    });
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: `filesystem-abort-${current}`,
        granted: ['filesystem:read', 'workspaces:read'],
      },
    });
    socket.write(greeting.subarray(0, 3));
    socket.write(greeting.subarray(3));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const first = await connect({ path: socketPath });
    const controller = new AbortController();
    const catchingUp = workspace(first).files.catchUpChanges({
      cursor: { journal: FILE_JOURNAL, revision: 4 },
      signal: controller.signal,
    });
    await requestArrived;
    controller.abort('indexer restarted');
    await assert.rejects(
      catchingUp,
      (error) => error.name === 'AbortError' && error.cause === controller.signal.reason,
    );
    await first.closed;

    const recovered = await connect({ path: socketPath });
    assert.equal((await workspace(recovered).info()).name, 'reconnected-indexer');
    await recovered.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix query deadline cancels its exact execution and preserves the session', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-query-deadline-'));
  const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64);
  const executionId = 'e'.repeat(32);
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
          frame.payload.call === 'container_exec'
            ? { reply: 'identity', with: executionId }
            : frame.payload.call === 'execution_output'
              ? {
                  reply: 'execution_output',
                  with: { entries: [], next: 0, more: false, eof: false, gap: false },
                }
              : frame.payload.call === 'workspace_info'
                ? {
                    reply: 'workspace',
                    with: { name: 'database', image: 'postgres:17', architecture: 'amd64' },
                  }
                : { reply: 'done' };
        const response = encode({ channel: frame.channel, kind: KIND.response, payload });
        socket.write(response.subarray(0, 6));
        socket.write(response.subarray(6));
      }
    });
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'postgres-query-deadline',
        granted: [
          'containers:read',
          'containers:execute',
          'containers:lifecycle',
          'workspaces:read',
        ],
      },
    });
    socket.write(greeting.subarray(0, 4));
    socket.write(greeting.subarray(4));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    await assert.rejects(
      containers.execJsonLines(
        containerId,
        4,
        { command: ['psql'], maxLineBytes: 4096, deadlineMs: 0 },
        () => {},
      ),
      /deadline.*1.*86400000/,
    );
    assert.deepEqual(requests, [], 'invalid deadlines must not start a query');

    await assert.rejects(
      containers.execJsonLines(
        containerId,
        4,
        {
          command: ['psql', '--csv'],
          maxLineBytes: 4096,
          deadlineMs: 20,
          pollIntervalMs: 50,
          cancelSignal: 'SIGINT',
          cancelTimeoutMs: 750,
        },
        () => {},
      ),
      (error) =>
        error instanceof ExecutionOperationError &&
        error.executionId === executionId &&
        error.phase === 'output' &&
        error.cause?.name === 'AbortError' &&
        error.cause.cause instanceof ExecutionDeadlineError &&
        error.cause.cause.executionId === executionId &&
        error.cause.cause.deadlineMs === 20,
    );
    assert.deepEqual(
      requests.map(({ call }) => call),
      ['container_exec', 'execution_output', 'execution_cancel'],
    );
    assert.deepEqual(requests[2].with, {
      id: executionId,
      signal: 'SIGINT',
      timeout_ms: 750,
    });
    assert.equal((await workspace(session).info()).name, 'database');
    assert.deepEqual(
      requests.map(({ call }) => call),
      ['container_exec', 'execution_output', 'execution_cancel', 'workspace_info'],
    );
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix chunk reads pin a prior file identity across fragmented frames', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-observed-chunks-'));
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
        requests.push(frame.payload.with);
        const input = frame.payload.with;
        const stale = input.path === 'src/stale.ts';
        const reply = encode({
          channel: frame.channel,
          kind: KIND.response,
          payload: {
            reply: 'file_range',
            with: {
              path: input.path,
              identity: stale ? 'changed-v2' : 'source-v1',
              offset: input.offset,
              total: 4,
              contents: input.offset === 0 ? [65, 66] : [67, 68],
              eof: input.offset === 2,
              truncated: input.offset !== 2,
            },
          },
        });
        socket.write(reply.subarray(0, 7));
        socket.write(reply.subarray(7));
      }
    });
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: { protocol: 1, peer: 'observed-chunks', granted: ['filesystem:read'] },
    });
    socket.write(greeting.subarray(0, 3));
    socket.write(greeting.subarray(3));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const files = workspace(session).files;
    const contents = [];
    for await (const range of files.readChunks('src/a.ts', {
      chunkBytes: 2,
      observed: 'source-v1',
    })) {
      contents.push(...range.contents);
    }
    assert.deepEqual(contents, [65, 66, 67, 68]);
    assert.deepEqual(
      requests.slice(0, 2).map(({ offset, observed }) => ({ offset, observed })),
      [
        { offset: 0, observed: 'source-v1' },
        { offset: 2, observed: 'source-v1' },
      ],
    );
    await assert.rejects(
      async () => {
        for await (const _range of files.readChunks('src/stale.ts', {
          chunkBytes: 2,
          observed: 'source-v1',
        })) {
          // The mismatched first range must never be delivered.
        }
      },
      (error) => {
        assert(error instanceof FileIdentityChangedError);
        assert.equal(error.path, 'src/stale.ts');
        assert.equal(error.expected, 'source-v1');
        assert.equal(error.actual, 'changed-v2');
        assert.equal(error.offset, 0);
        return true;
      },
    );
    assert.equal(requests.at(-1).observed, 'source-v1');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix composite execution authority rejects before framing and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-exec-authority-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: {
              reply: 'workspace',
              with: { name: 'database', image: 'toolbox', architecture: 'amd64' },
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
          peer: 'fixture',
          granted: ['containers:execute', 'workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    await assert.rejects(
      containers.execWithCredentials('c'.repeat(64), 1, {
        command: ['psql'],
        credentials: [['PGPASSWORD', 'postgres.password']],
      }),
      /credentials:inject/,
    );
    await assert.rejects(
      containers.exec('c'.repeat(64), 1, { command: ['debug-helper'], stdin: true }),
      /containers:input/,
    );
    assert.deepEqual(calls, [], 'secondary authority denial must not frame an execution mutation');
    assert.equal((await workspace(session).info()).name, 'database');
    assert.deepEqual(calls, ['workspace_info']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix range batch preserves ordered paths and one bounded frame', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-range-batch-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const fragmented = (socket, frame) => {
    socket.write(frame.subarray(0, 5));
    setImmediate(() => socket.write(frame.subarray(5)));
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        if (frame.payload.call === 'filesystem_stat') {
          socket.write(
            encode({
              channel: frame.channel,
              kind: KIND.response,
              payload: {
                reply: 'entry',
                with: { path: 'src/a.rs', directory: false, size: 1, identity: 'id:src/a.rs' },
              },
            }),
          );
          continue;
        }
        const samePath =
          frame.payload.with.ranges.length === 2 &&
          frame.payload.with.ranges[0].path === frame.payload.with.ranges[1].path;
        const ranges = frame.payload.with.ranges.map((range, index) => ({
          path: range.path,
          identity: samePath && index === 1 ? `changed:${range.path}` : `id:${range.path}`,
          offset: range.offset,
          total: samePath ? 2 : 1,
          contents: [range.path.charCodeAt(0)],
          eof: samePath ? index === 1 : true,
          truncated: samePath ? index === 0 : false,
        }));
        fragmented(
          socket,
          encode({
            channel: frame.channel,
            kind: KIND.response,
            payload: { reply: 'file_ranges', with: ranges },
          }),
        );
      }
    });
    const greeting = encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'range-batch',
          granted: ['filesystem:read'],
        },
      });
    socket.write(greeting.subarray(0, 3));
    setImmediate(() => socket.write(greeting.subarray(3)));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const values = await workspace(session).files.readRanges([
      { path: 'src/a.rs', limit: 1 },
      { path: 'src/b.rs', limit: 1 },
    ]);
    assert.deepEqual(
      values.map(({ path }) => path),
      ['src/a.rs', 'src/b.rs'],
    );
    assert.equal(calls.length, 1);
    assert.deepEqual(calls[0], {
      call: 'filesystem_read_ranges',
      with: {
        ranges: [
          { path: 'src/a.rs', offset: 0, limit: 1, observed: null },
          { path: 'src/b.rs', offset: 0, limit: 1, observed: null },
        ],
      },
    });
    await assert.rejects(
      workspace(session).files.readRanges([
        { path: 'src/a.rs', limit: 1, observed: 'review-snapshot-v1' },
      ]),
      (error) => {
        assert(error instanceof FileIdentityChangedError);
        assert.equal(error.path, 'src/a.rs');
        assert.equal(error.expected, 'review-snapshot-v1');
        assert.equal(error.actual, 'id:src/a.rs');
        assert.equal(error.offset, 0);
        return true;
      },
    );
    assert.equal(calls[1].with.ranges[0].observed, 'review-snapshot-v1');
    await assert.rejects(
      workspace(session).files.readRanges([
        { path: 'src/a.rs', offset: 0, limit: 1 },
        { path: 'src/a.rs', offset: 1, limit: 1 },
      ]),
      /inconsistent filesystem range batch/,
    );
    assert.equal((await workspace(session).files.stat('src/a.rs')).identity, 'id:src/a.rs');
    assert.equal(calls[3].call, 'filesystem_stat');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix stat rejects another file identity and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-stat-identity-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload);
        const path = calls.length === 1 ? 'src/replacement.ts' : frame.payload.with.path;
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: {
              reply: 'entry',
              with: { path, directory: false, size: 12, identity: `identity:${path}` },
            },
          }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'stat-identity', granted: ['filesystem:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const files = workspace(session).files;
    await assert.rejects(
      files.stat('src/index.ts'),
      /metadata for src\/replacement\.ts, expected src\/index\.ts; no file identity was assumed/,
    );
    assert.deepEqual(
      calls.map(({ call }) => call),
      ['filesystem_stat'],
    );
    assert.equal((await files.stat('src/index.ts')).identity, 'identity:src/index.ts');
    assert.deepEqual(
      calls.map(({ call }) => call),
      ['filesystem_stat', 'filesystem_stat'],
    );
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix process inspection rejects another container and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-process-identity-'));
  const socketPath = path.join(directory, 'host.sock');
  const containerId = 'a'.repeat(64);
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload);
        const owner = calls.length === 1 ? 'b'.repeat(64) : containerId;
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: {
              reply: 'processes',
              with: {
                container_id: owner,
                titles: ['PID', 'CMD'],
                processes: [['7', 'postgres']],
                snapshot: 'c'.repeat(64),
                next: null,
                more: false,
                observed_at_ms: 1,
                scope: 'namespace',
                pid_identity: 'snapshot',
                truncated: false,
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
        payload: { protocol: 1, peer: 'process-identity', granted: ['containers:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    await assert.rejects(
      containers.processes(containerId),
      new RegExp(`processes for container ${'b'.repeat(64)}, expected ${containerId}`),
    );
    assert.deepEqual(
      calls.map(({ call }) => call),
      ['container_processes'],
    );
    assert.equal((await containers.processes(containerId)).container_id, containerId);
    assert.deepEqual(
      calls.map(({ call }) => call),
      ['container_processes', 'container_processes'],
    );
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix execution inventory rejects duplicate identities and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-execution-identities-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const execution = {
    id: 'e'.repeat(32),
    container_id: 'c'.repeat(64),
    running: true,
    exit_code: 0,
    pid: 17,
    command: ['test-runner'],
    user: 'worker',
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        const executions =
          calls.length === 1 ? [execution, { ...execution, running: false }] : [execution];
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: { reply: 'executions', with: { executions, truncated: false } },
          }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'execution-identities', granted: ['containers:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    await assert.rejects(containers.executions(), /duplicate immutable execution identities/);
    assert.deepEqual(calls, ['execution_list']);
    assert.deepEqual((await containers.executions()).executions, [execution]);
    assert.deepEqual(calls, ['execution_list', 'execution_list']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix network inventory rejects duplicate identities and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-network-identities-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const network = {
    id: 'a'.repeat(32),
    name: 'database',
    driver: 'bridge',
    scope: 'local',
    kind: 'custom',
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        const networks =
          calls.length === 1 ? [network, { ...network, name: 'replacement' }] : [network];
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: { reply: 'networks', with: { networks, truncated: false } },
          }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'network-identities', granted: ['networks:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const networks = workspace(session).networks;
    await assert.rejects(networks.inventory(), /duplicate immutable network identities/);
    assert.deepEqual(calls, ['network_list']);
    assert.deepEqual((await networks.inventory()).networks, [network]);
    assert.deepEqual(calls, ['network_list', 'network_list']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix volume inventory rejects duplicate identities without mutation and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-volume-identities-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const volume = {
    name: 'index-cache',
    driver: 'local',
    generation: 'a'.repeat(32),
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        const volumes =
          calls.length === 1 ? [volume, { ...volume, generation: 'b'.repeat(32) }] : [volume];
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: { reply: 'volumes', with: { volumes, truncated: false } },
          }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'volume-identities', granted: ['volumes:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const volumes = workspace(session).volumes;
    await assert.rejects(volumes.inventory(), /duplicate volume identities/);
    assert.deepEqual(calls, ['volume_list'], 'rejection emits no follow-up or mutation');
    assert.deepEqual((await volumes.inventory()).volumes, [volume]);
    assert.deepEqual(calls, ['volume_list', 'volume_list']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix network inspection rejects another immutable identity and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-network-inspect-'));
  const socketPath = path.join(directory, 'host.sock');
  const networkId = 'a'.repeat(32);
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        const id = calls.length === 1 ? 'b'.repeat(32) : networkId;
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: {
              reply: 'network',
              with: { id, name: 'database', driver: 'bridge', scope: 'local', kind: 'custom' },
            },
          }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'network-inspect', granted: ['networks:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const networks = workspace(session).networks;
    await assert.rejects(networks.inspect(networkId), /host returned network/);
    assert.deepEqual(calls, ['network_inspect']);
    assert.equal((await networks.inspect(networkId)).id, networkId);
    assert.deepEqual(calls, ['network_inspect', 'network_inspect']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix volume inspection rejects another name and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-volume-inspect-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: {
              reply: 'volume',
              with: {
                name: calls.length === 1 ? 'other-cache' : 'index-cache',
                driver: 'local',
                generation: 'a'.repeat(32),
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
        payload: { protocol: 1, peer: 'volume-inspect', granted: ['volumes:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const volumes = workspace(session).volumes;
    await assert.rejects(volumes.inspect('index-cache'), /host returned volume other-cache/);
    assert.deepEqual(calls, ['volume_inspect'], 'rejection emits no follow-up or mutation');
    assert.equal((await volumes.inspect('index-cache')).name, 'index-cache');
    assert.deepEqual(calls, ['volume_inspect', 'volume_inspect']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix workspace inspection rejects another name without mutation and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-workspace-inspect-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const configuration = (name) => ({
    name,
    image: 'alpine:3.20',
    architecture: 'amd64',
    generation: 'a'.repeat(32),
    configuration_revision: 'b'.repeat(32),
    storage: null,
    shell: null,
    cpus: null,
    memory_mb: null,
    environment: [],
    mounts: [],
    docker_socket: false,
    scrollback: null,
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
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: {
              reply: 'workspace_configuration',
              with: configuration(calls.length === 1 ? 'production' : 'review'),
            },
          }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'workspace-inspect', granted: ['workspaces:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const api = workspace(session);
    await assert.rejects(api.inspect('review'), /configuration for production, expected review/);
    assert.deepEqual(calls, ['workspace_inspect'], 'rejection emits no follow-up or mutation');
    assert.equal((await api.inspect('review')).name, 'review');
    assert.deepEqual(calls, ['workspace_inspect', 'workspace_inspect']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix workspace inventory rejects duplicate identities and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-workspace-identities-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const workspaceState = (running, current) => ({
    name: 'test-runner',
    image: 'alpine:3.20',
    architecture: 'amd64',
    running,
    current,
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: {
              reply: 'workspaces',
              with:
                calls.length === 1
                  ? [workspaceState(true, true), workspaceState(false, false)]
                  : [workspaceState(true, true)],
            },
          }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'test-orchestrator', granted: ['workspaces:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const api = workspace(session);
    await assert.rejects(api.list(), /duplicate workspace identities/);
    assert.deepEqual(calls, ['workspace_list'], 'rejection emits no follow-up or mutation');
    assert.equal((await api.list())[0].name, 'test-runner');
    assert.deepEqual(calls, ['workspace_list', 'workspace_list']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix extension inspection rejects another name without mutation and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-extension-inspect-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const digest = `sha256:${'a'.repeat(64)}`;
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: {
              reply: 'extension',
              with: {
                name: calls.length === 1 ? 'untrusted-store' : 'catalogue',
                image_digest: digest,
                status: 'standby',
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
        payload: { protocol: 1, peer: 'catalogue', granted: ['extensions:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const extensions = workspace(session).extensions;
    await assert.rejects(extensions.inspect('catalogue'), /extension untrusted-store/);
    assert.deepEqual(calls, ['extension_inspect'], 'rejection emits no follow-up or mutation');
    assert.equal((await extensions.inspect('catalogue')).name, 'catalogue');
    assert.deepEqual(calls, ['extension_inspect', 'extension_inspect']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix extension inventory rejects duplicate names without mutation and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-extension-identities-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const extension = {
    name: 'catalogue',
    image_digest: `sha256:${'a'.repeat(64)}`,
    status: 'standby',
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        const extensions =
          calls.length === 1
            ? [extension, { ...extension, image_digest: `sha256:${'b'.repeat(64)}` }]
            : [extension];
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: { reply: 'extensions', with: extensions },
          }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'catalogue', granted: ['extensions:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const extensions = workspace(session).extensions;
    await assert.rejects(extensions.list(), /duplicate extension identities/);
    assert.deepEqual(calls, ['extension_list'], 'rejection emits no follow-up or mutation');
    assert.deepEqual(await extensions.list(), [extension]);
    assert.deepEqual(calls, ['extension_list', 'extension_list']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix digest-pinned image inspection rejects another identity and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-image-inspect-'));
  const socketPath = path.join(directory, 'host.sock');
  const digest = `sha256:${'a'.repeat(64)}`;
  const calls = [];
  const connections = new Set();
  const details = (id) => ({
    id,
    references: ['embedding-worker:1'],
    created: '',
    size: 0,
    os: 'linux',
    architecture: 'amd64',
    entrypoint: [],
    command: [],
    working_directory: '',
    user: '',
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: {
              reply: 'image_details',
              with: details(calls.length === 1 ? `sha256:${'b'.repeat(64)}` : digest),
            },
          }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'embedding-worker', granted: ['images:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const images = workspace(session).images;
    await assert.rejects(images.inspect(digest), /host returned image/);
    assert.deepEqual(calls, ['image_inspect'], 'rejection emits no follow-up or mutation');
    assert.equal((await images.inspect(digest)).id, digest);
    assert.deepEqual(calls, ['image_inspect', 'image_inspect']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix image inventory rejects duplicate digests without mutation and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-image-identities-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const image = {
    id: `sha256:${'a'.repeat(64)}`,
    reference: 'worker:1',
    references: ['worker:1'],
    size: 1024,
    created: 1,
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        const images =
          calls.length === 1
            ? [image, { ...image, reference: 'worker:latest', references: ['worker:latest'] }]
            : [image];
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: { reply: 'images', with: { images, truncated: false } },
          }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'image-auditor', granted: ['images:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const images = workspace(session).images;
    await assert.rejects(images.inventory(), /duplicate immutable image identities/);
    assert.deepEqual(calls, ['image_list'], 'rejection emits no follow-up or mutation');
    assert.deepEqual((await images.inventory()).images, [image]);
    assert.deepEqual(calls, ['image_list', 'image_list']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix execution reads reject another identity and preserve session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-execution-identity-'));
  const socketPath = path.join(directory, 'host.sock');
  const executionId = 'a'.repeat(32);
  const wrongId = 'b'.repeat(32);
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: {
              reply: 'execution',
              with: {
                id: calls.length < 3 ? wrongId : executionId,
                container_id: 'c'.repeat(64),
                running: false,
                exit_code: 0,
                pid: 7,
                command: ['psql'],
                user: 'postgres',
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
        payload: { protocol: 1, peer: 'execution-identity', granted: ['containers:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    await assert.rejects(containers.execution(executionId), /returned inspection for execution/);
    await assert.rejects(
      containers.waitExecution(executionId),
      /returned wait result for execution/,
    );
    assert.deepEqual(calls, ['execution_inspect', 'execution_wait']);
    assert.equal((await containers.execution(executionId)).id, executionId);
    assert.deepEqual(calls, ['execution_inspect', 'execution_wait', 'execution_inspect']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix credential injection sends only the key without granting secret reads', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-credential-exec-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        socket.write(
          encode({
            channel: frame.channel,
            kind: KIND.response,
            payload: { reply: 'identity', with: 'exec-1' },
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
          peer: 'credential-exec',
          granted: ['containers:execute', 'credentials:inject'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    assert.equal(
      await workspace(session).containers.execWithCredentials('a'.repeat(64), 4, {
        command: ['psql', '-c', 'select 1'],
        credentials: [['PGPASSWORD', 'postgres.password']],
      }),
      'exec-1',
    );
    assert.deepEqual(calls, [
      {
        call: 'container_exec_credential',
        with: {
          id: 'a'.repeat(64),
          generation: 4,
          command: ['psql', '-c', 'select 1'],
          environment: [],
          credentials: [['PGPASSWORD', 'postgres.password']],
          user: null,
          working_directory: null,
        },
      },
    ]);
    assert.ok(!JSON.stringify(calls).includes('sentinel-password'));
    await assert.rejects(
      workspace(session).credentials.read('postgres.password'),
      /credentials:read/,
    );
    assert.equal(calls.length, 1, 'denied plaintext reads never reach the Unix socket');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix credential calls reveal only the named value and preserve CAS framing', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-credential-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        const reply =
          frame.payload.call === 'credential_read'
            ? {
                reply: 'credential',
                with: { key: 'postgres.password', revision: 7, value: [0, 255, 10] },
              }
            : { reply: 'revision', with: frame.payload.call === 'credential_set' ? 8 : 9 };
        socket.write(encode({ channel: frame.channel, kind: KIND.response, payload: reply }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'credential-test',
          granted: ['credentials:read', 'credentials:write'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const credentials = workspace(session).credentials;
    assert.deepEqual(await credentials.read('postgres.password'), {
      key: 'postgres.password',
      revision: 7,
      value: [0, 255, 10],
    });
    assert.equal(await credentials.set(7, 'postgres.password', [0, 255, 10]), 8);
    assert.equal(await credentials.remove(8, 'postgres.password'), 9);
    assert.deepEqual(calls, [
      { call: 'credential_read', with: { key: 'postgres.password' } },
      {
        call: 'credential_set',
        with: { observed: 7, key: 'postgres.password', value: [0, 255, 10] },
      },
      { call: 'credential_remove', with: { observed: 8, key: 'postgres.password' } },
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix preference read rejects duplicate keys without mutation and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-preference-identities-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: {
              reply: 'preferences',
              with: {
                revision: 7,
                entries:
                  calls.length === 1
                    ? [
                        ['database', { kind: 'string', value: 'development' }],
                        ['database', { kind: 'string', value: 'production' }],
                      ]
                    : [['database', { kind: 'string', value: 'development' }]],
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
        payload: { protocol: 1, peer: 'database-ui', granted: ['preferences:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const preferences = workspace(session).preferences;
    await assert.rejects(preferences.read(), /duplicate preference keys/);
    assert.deepEqual(calls, ['preference_read'], 'rejection emits no follow-up or mutation');
    assert.deepEqual((await preferences.read()).entries, [
      ['database', { kind: 'string', value: 'development' }],
    ]);
    assert.deepEqual(calls, ['preference_read', 'preference_read']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix credential read rejects another key without mutation and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-credential-identity-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: {
              reply: 'credential',
              with: {
                key: calls.length === 1 ? 'production.password' : 'postgres.password',
                revision: 7,
                value: [115, 101, 99, 114, 101, 116],
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
        payload: { protocol: 1, peer: 'postgres', granted: ['credentials:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const credentials = workspace(session).credentials;
    await assert.rejects(credentials.read('postgres.password'), /credential production.password/);
    assert.deepEqual(calls, ['credential_read'], 'rejection emits no follow-up or mutation');
    assert.equal((await credentials.read('postgres.password')).key, 'postgres.password');
    assert.deepEqual(calls, ['credential_read', 'credential_read']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix PostgreSQL discovery preserves exact list and inspection host bindings', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-postgres-port-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  const id = 'd'.repeat(64);
  const container = {
    id,
    name: 'postgres',
    image: 'postgres:17',
    state: 'running',
    created: 1,
    generation: 4,
    ports: [{ container: 5432, host: 15432, host_ip: '0.0.0.0', protocol: 'tcp' }],
  };
  const calls = [];
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        const payload =
          frame.payload.call === 'container_list'
            ? { reply: 'containers', with: [container] }
            : {
                reply: 'container',
                with: {
                  ...container,
                  ports: [
                    { container: 5432, host: 15432, host_ip: '127.0.0.1', protocol: 'tcp' },
                  ],
                },
              };
        socket.write(
          encode({
            channel: frame.channel,
            kind: KIND.response,
            payload,
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
          peer: 'postgres-port',
          granted: ['containers:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    assert.equal((await containers.list())[0].ports[0].host_ip, '0.0.0.0');
    assert.equal((await containers.inspect(id)).ports[0].host_ip, '127.0.0.1');
    assert.deepEqual(calls, [
      { call: 'container_list' },
      { call: 'container_inspect', with: { id } },
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix container inventory rejects duplicate identities and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-container-identities-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const first = {
    id: 'a'.repeat(64),
    name: 'postgres-primary',
    image: 'postgres:17',
    state: 'running',
    created: 1,
    generation: 4,
    ports: [{ container: 5432, host: 15432, protocol: 'tcp' }],
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        const containers =
          calls.length === 1 ? [first, { ...first, name: 'postgres-replica' }] : [first];
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: { reply: 'containers', with: containers },
          }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'container-identities', granted: ['containers:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    await assert.rejects(containers.list(), /duplicate immutable container identities/);
    assert.deepEqual(calls, ['container_list']);
    assert.deepEqual(await containers.list(), [first]);
    assert.deepEqual(calls, ['container_list', 'container_list']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix observed inspection rejects a replacement before publishing its endpoint', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-observed-preview-'));
  const socketPath = path.join(directory, 'host.sock');
  const id = 'a'.repeat(64);
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        const replacement = {
          id,
          name: 'preview',
          image: 'web:latest',
          state: 'running',
          created: 2,
          generation: 8,
          ports: [{ container: 8080, host: 18080, protocol: 'tcp' }],
        };
        const payload =
          frame.payload.call === 'container_inspect_observed'
            ? { reply: 'container', with: replacement }
            : { reply: 'containers', with: [replacement] };
        socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'preview', granted: ['containers:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    await assert.rejects(
      containers.inspectObserved(id, 7),
      /different observed container generation/,
    );
    assert.deepEqual(calls[0], {
      call: 'container_inspect_observed',
      with: { id, generation: 7 },
    });
    assert.equal((await containers.list())[0].generation, 8);
    assert.equal(calls[1].call, 'container_list');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix container inspection rejects another identity without mutation and preserves session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-container-inspect-'));
  const socketPath = path.join(directory, 'host.sock');
  const id = 'a'.repeat(64);
  const calls = [];
  const connections = new Set();
  const summary = (containerId) => ({
    id: containerId,
    name: 'postgres',
    image: 'postgres:17',
    state: 'running',
    created: 2,
    generation: 8,
    ports: [{ container: 5432, host: 15432, protocol: 'tcp' }],
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: {
              reply: 'container',
              with: summary(calls.length === 1 ? 'b'.repeat(64) : id),
            },
          }),
        );
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'postgres-preview', granted: ['containers:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    await assert.rejects(containers.inspect(id), /host returned container/);
    assert.deepEqual(calls, ['container_inspect'], 'rejection emits no follow-up or mutation');
    assert.equal((await containers.inspect(id)).id, id);
    assert.equal((await containers.inspect(id.slice(0, 32))).id, id);
    assert.deepEqual(calls, ['container_inspect', 'container_inspect', 'container_inspect']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix beginWalk captures the reconciliation cursor before recursive enumeration', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-index-walk-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        const payload =
          frame.payload.call === 'filesystem_inventory'
            ? {
                reply: 'file_inventory',
                with: {
                  entries: [],
                  complete: true,
                  coalesced: 0,
                  journal: FILE_JOURNAL,
                  revision: 41,
                },
              }
            : frame.payload.call === 'filesystem_list_page'
              ? {
                  reply: 'directory_page',
                  with: {
                    entries: [
                      { path: 'src/document.md', directory: false, size: 12, identity: 'doc-v1' },
                    ],
                    identity: 'src-v1',
                    next: 'src/document.md',
                    more: false,
                  },
                }
              : {
                  reply: 'file_changes',
                  with: {
                    journal: FILE_JOURNAL,
                    changes: [],
                    next: 41,
                    current: 41,
                    more: false,
                    truncated: false,
                  },
                };
        socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'index-walk', granted: ['filesystem:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const scan = await workspace(session).files.beginWalk('src', { pageSize: 32 });
    assert.deepEqual(calls, [{ call: 'filesystem_inventory' }]);
    assert.deepEqual(scan.cursor, { journal: FILE_JOURNAL, revision: 41 });
    assert.equal(scan.inventory.complete, true);
    assert.deepEqual(
      await Array.fromAsync(scan.entries),
      [{ path: 'src/document.md', directory: false, size: 12, identity: 'doc-v1' }],
    );
    await workspace(session).files.changes(scan.cursor, 32);
    assert.deepEqual(calls, [
      { call: 'filesystem_inventory' },
      {
        call: 'filesystem_list_page',
        with: { path: 'src', after: null, observed: null, limit: 32 },
      },
      {
        call: 'filesystem_changes',
        with: { observed: FILE_JOURNAL, after: 41, limit: 32 },
      },
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix change iterator reports a typed journal rotation and remains reusable', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-journal-gap-'));
  const socketPath = path.join(directory, 'host.sock');
  const replacementJournal = 'fedcba9876543210fedcba9876543210';
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        const payload =
          frame.payload.call === 'filesystem_changes'
            ? {
                reply: 'file_changes',
                with: {
                  journal: replacementJournal,
                  changes: [],
                  next: 88,
                  current: 88,
                  more: false,
                  truncated: true,
                },
              }
            : {
                reply: 'workspace',
                with: { name: 'index', image: 'toolbox', architecture: 'amd64' },
              };
        socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'restartable-indexer',
          granted: ['filesystem:read', 'workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const files = workspace(session).files;
    const invalid = files.changePages({
      cursor: { journal: FILE_JOURNAL, revision: 40 },
      gapPolicy: 'ignore',
    });
    await assert.rejects(invalid.next(), /gapPolicy.*yield.*throw/);
    const cancelled = new AbortController();
    cancelled.abort('indexer stopped');
    const aborted = files.changePages({
      cursor: { journal: FILE_JOURNAL, revision: 40 },
      gapPolicy: 'throw',
      signal: cancelled.signal,
    });
    await assert.rejects(aborted.next(), (error) => error?.name === 'AbortError');
    assert.deepEqual(calls, [], 'invalid and cancelled iterators must not poll the journal');

    const pages = files.changePages({
      cursor: { journal: FILE_JOURNAL, revision: 40 },
      pageSize: 32,
      gapPolicy: 'throw',
    });
    await assert.rejects(
      pages.next(),
      (error) =>
        error instanceof FilesystemJournalGapError &&
        error.requested.journal === FILE_JOURNAL &&
        error.requested.revision === 40 &&
        error.replacement.journal === replacementJournal &&
        error.replacement.revision === 88,
    );
    assert.deepEqual(calls, [
      {
        call: 'filesystem_changes',
        with: { observed: FILE_JOURNAL, after: 40, limit: 32 },
      },
    ]);
    assert.equal((await workspace(session).info()).name, 'index');
    assert.deepEqual(calls.map(({ call }) => call), ['filesystem_changes', 'workspace_info']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix change iterator rejects a non-advancing continuation and keeps the session reusable', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-journal-stall-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload.call);
        const payload = frame.payload.call === 'filesystem_changes'
          ? {
              reply: 'file_changes',
              with: {
                journal: FILE_JOURNAL,
                changes: [],
                next: 12,
                current: 13,
                more: true,
                truncated: false,
              },
            }
          : {
              reply: 'workspace',
              with: { name: 'index', image: 'toolbox', architecture: 'amd64' },
            };
        const response = encode({ channel: frame.channel, kind: KIND.response, payload });
        socket.write(response.subarray(0, 2));
        socket.write(response.subarray(2, 9));
        socket.write(response.subarray(9));
      }
    });
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'adversarial-indexer-host',
        granted: ['filesystem:read', 'workspaces:read'],
      },
    });
    socket.write(greeting.subarray(0, 1));
    socket.write(greeting.subarray(1));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  let session;
  try {
    session = await connect({ path: socketPath });
    await assert.rejects(
      workspace(session).files.changes({ journal: FILE_JOURNAL, revision: 12 }),
      /inconsistent filesystem change page/,
    );
    assert.deepEqual(calls, ['filesystem_changes'], 'a stalled continuation must not be requested again');
    assert.equal((await workspace(session).info()).name, 'index');
    assert.deepEqual(calls, ['filesystem_changes', 'workspace_info']);
    await session.close();
  } finally {
    await session?.close();
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix filesystem watcher publishes filtered cursor-only progress for restart', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-filtered-cursor-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const page = {
    journal: FILE_JOURNAL,
    changes: [],
    next: 19,
    current: 19,
    more: false,
    truncated: false,
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        socket.write(
          encode({
            channel: frame.channel,
            kind: KIND.response,
            payload: {
              reply: 'file_changes',
              with: page,
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
          peer: 'filtered-cursor',
          granted: ['filesystem:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const controller = new AbortController();
    let delivered;
    const seen = new Promise((resolve) => {
      delivered = resolve;
    });
    const stop = await workspace(session).files.watchChanges(
      (value) => {
        controller.abort();
        delivered(value);
      },
      {
        cursor: { journal: FILE_JOURNAL, revision: 7 },
        pageSize: 32,
        pollMs: 1_000,
        signal: controller.signal,
      },
    );
    assert.deepEqual(await seen, page);
    await stop();
    assert.deepEqual(calls, [
      { call: 'filesystem_changes', with: { observed: FILE_JOURNAL, after: 7, limit: 32 } },
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix latest-change watcher supersedes long test work without blocking its next cursor', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-latest-change-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        if (frame.payload.call === 'filesystem_changes') {
          const revision = frame.payload.with.after + 1;
          socket.write(
            encode({
              channel: frame.channel,
              kind: KIND.response,
              payload: {
                reply: 'file_changes',
                with: {
                  journal: FILE_JOURNAL,
                  changes: [
                    {
                      revision,
                      kind: 'modify',
                      path: `src/revision-${revision}.ts`,
                      entry: null,
                    },
                  ],
                  next: revision,
                  current: revision,
                  more: false,
                  truncated: false,
                },
              },
            }),
          );
        } else if (frame.payload.call === 'workspace_info') {
          socket.write(
            encode({
              channel: frame.channel,
              kind: KIND.response,
              payload: {
                reply: 'workspace',
                with: { name: 'tests', image: 'toolbox', architecture: 'amd64' },
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
          peer: 'latest-test-runner',
          granted: ['filesystem:read', 'workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  const controller = new AbortController();
  const started = [];
  const delivered = [];
  const superseded = [];
  let stop;
  try {
    const session = await connect({ path: socketPath, timeout: 1_000 });
    stop = await workspace(session).files.watchLatestChanges(
      async (page, signal) => {
        const revisions = page.changes.map(({ revision }) => revision);
        delivered.push(revisions);
        const revision = revisions.at(-1);
        started.push(revision);
        if (revision === 2) {
          controller.abort('latest revision observed');
          return;
        }
        await new Promise((_, reject) => {
          signal.addEventListener(
            'abort',
            () => {
              superseded.push(revision);
              const error = new Error('test execution superseded');
              error.name = 'AbortError';
              reject(error);
            },
            { once: true },
          );
        });
      },
      {
        cursor: { journal: FILE_JOURNAL, revision: 0 },
        pollMs: 1,
        signal: controller.signal,
      },
    );
    await Promise.race([
      stop.done,
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error('latest watcher blocked behind stale test work')), 200),
      ),
    ]);
    assert.deepEqual(started, [1, 2]);
    assert.deepEqual(
      delivered,
      [[1], [1, 2]],
      'replacement work retains the change whose prior generation was superseded',
    );
    assert.deepEqual(superseded, [1]);
    assert.deepEqual(
      calls
        .filter(({ call }) => call === 'filesystem_changes')
        .map(({ with: cursor }) => cursor.after),
      [0, 1],
      'revision 2 is requested while revision 1 work is still pending',
    );
    assert.equal((await workspace(session).info()).name, 'tests');
    await session.close();
  } finally {
    await stop?.().catch(() => {});
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix filesystem watcher exposes listener failure without poisoning the session', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-watch-supervision-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  const calls = [];
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        const payload =
          frame.payload.call === 'filesystem_changes'
            ? {
                reply: 'file_changes',
                with: {
                  journal: FILE_JOURNAL,
                  changes: [{ revision: 3, kind: 'modify', path: 'src/a.ts', entry: null }],
                  next: 3,
                  current: 3,
                  more: false,
                  truncated: false,
                },
              }
            : {
                reply: 'workspace',
                with: { name: 'index', image: 'toolbox', architecture: 'amd64' },
              };
        socket.write(encode({ channel: 2, kind: KIND.response, payload }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'fixture',
          granted: ['filesystem:read', 'workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const failure = new Error('checkpoint write failed');
    const stop = await workspace(session).files.watchChanges(
      async () => {
        throw failure;
      },
      { cursor: { journal: FILE_JOURNAL, revision: 2 }, pollMs: 10_000 },
    );
    await assert.rejects(stop.done, (error) => error === failure);
    assert.deepEqual(calls, ['filesystem_changes']);
    assert.equal((await workspace(session).info()).name, 'index');
    await assert.rejects(stop(), (error) => error === failure);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix cancelled state update cannot publish a stale checkpoint', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-state-update-abort-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload.call);
        const payload =
          frame.payload.call === 'state_read'
            ? { reply: 'state', with: { identity: 'absent', contents: [] } }
            : {
                reply: 'workspace',
                with: { name: 'index', image: 'toolbox', architecture: 'amd64' },
              };
        socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'state-update-abort',
          granted: ['state:read', 'state:write', 'workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const controller = new AbortController();
    const codec = {
      decode(value) {
        return value ?? { revision: 0 };
      },
      encode(value) {
        return value;
      },
    };
    await assert.rejects(
      workspace(session).state.updateJson(
        codec,
        async (current) => {
          await Promise.resolve();
          controller.abort('new indexing run started');
          return { revision: current.revision + 1 };
        },
        { signal: controller.signal },
      ),
      (error) => error.name === 'AbortError' && error.cause === controller.signal.reason,
    );
    assert.deepEqual(calls, ['state_read'], 'abort after computation must prevent state_write');
    assert.equal((await workspace(session).info()).name, 'index');
    assert.deepEqual(calls, ['state_read', 'workspace_info']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix JSON state decode failure retains exact identity for CAS recovery', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-state-recovery-'));
  const socketPath = path.join(directory, 'host.sock');
  const staleIdentity = `sha256:${'a'.repeat(64)}`;
  const recoveredIdentity = `sha256:${'b'.repeat(64)}`;
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        const payload =
          frame.payload.call === 'state_read'
            ? {
                reply: 'state',
                with: {
                  identity: staleIdentity,
                  contents: [...Buffer.from('{"version":0,"revision":41}')],
                },
              }
            : frame.payload.call === 'state_write'
              ? { reply: 'identity', with: recoveredIdentity }
              : {
                  reply: 'workspace',
                  with: { name: 'index', image: 'toolbox', architecture: 'amd64' },
                };
        socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'checkpoint-recovery',
          granted: ['state:read', 'state:write', 'workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const state = workspace(session).state;
    await assert.rejects(state.readJson({ decode: 'invalid', encode: () => null }), /codec/);
    assert.deepEqual(calls, [], 'invalid codecs must not read durable state');

    const codec = {
      decode(value) {
        if (value?.version !== 1) throw new TypeError('unsupported checkpoint schema');
        return value;
      },
      encode(value) {
        return value;
      },
    };
    let failure;
    try {
      await state.readJson(codec);
    } catch (error) {
      failure = error;
    }
    assert.ok(failure instanceof StateDecodeError);
    assert.equal(failure.identity, staleIdentity);
    assert.ok(failure.cause instanceof TypeError);
    assert.match(failure.cause.message, /unsupported checkpoint schema/);

    const checkpoint = { version: 1, revision: 0 };
    assert.equal(await state.writeJson(failure.identity, checkpoint, codec), recoveredIdentity);
    assert.equal(calls[1].with.observed, staleIdentity);
    assert.deepEqual(
      JSON.parse(new TextDecoder().decode(Uint8Array.from(calls[1].with.contents))),
      checkpoint,
    );
    assert.equal((await workspace(session).info()).name, 'index');
    assert.deepEqual(
      calls.map(({ call }) => call),
      ['state_read', 'state_write', 'workspace_info'],
    );
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix text wait reconciles an unread revision without requiring a later event', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-text-reconcile-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const snapshot = {
    slot: 'shell',
    generation: 4,
    revision: 9,
    columns: 80,
    rows: 24,
    lines: ['unread output'],
    cursor_column: 13,
    cursor_row: 0,
    truncated: false,
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload.call);
        const reply =
          frame.payload.call === 'pane_list'
            ? {
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
              }
            : frame.payload.call === 'terminal_read_pane'
              ? { reply: 'text', with: snapshot }
              : { reply: 'done' };
        socket.write(encode({ channel: frame.channel, kind: KIND.response, payload: reply }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'text-reconcile',
          granted: ['panes:observe', 'terminals:output'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).terminal.waitForText(
      'shell',
      { generation: 4, revision: 8 },
      { lines: 40, timeoutMs: 1_000 },
    );
    assert.deepEqual(result, {
      changed: true,
      readable: { kind: 'terminal', text: 'unread output', snapshot },
    });
    assert.deepEqual(calls, [
      'event_subscribe',
      'pane_list',
      'terminal_read_pane',
      'event_unsubscribe',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix text wait retries a racing full-screen projection until it is coherent', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-text-race-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  let inventories = 0;
  const pane = (revision) => ({
    slot: 'tui',
    generation: 7,
    revision,
    kind: 'terminal',
    provider: null,
    tab: 'tab-1',
    title: 'Editor',
    focused: true,
  });
  const screen = (revision) => ({
    slot: 'tui',
    generation: 7,
    revision,
    columns: 120,
    rows: 40,
    lines: ['NORMAL  document.md', 'ready'],
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
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload.call);
        let payload;
        if (frame.payload.call === 'pane_list') {
          inventories += 1;
          payload = {
            reply: 'panes',
            with: { panes: [pane(inventories === 1 ? 9 : 11)], truncated: false },
          };
        } else if (frame.payload.call === 'terminal_read_pane') {
          payload = { reply: 'text', with: screen(inventories === 1 ? 10 : 11) };
        } else {
          payload = { reply: 'done' };
        }
        socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'full-screen-agent',
          granted: ['panes:observe', 'terminals:output'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).terminal.waitForText(
      'tui',
      { generation: 7, revision: 8 },
      { lines: 40, timeoutMs: 1_000 },
    );
    assert.deepEqual(result, {
      changed: true,
      readable: { kind: 'terminal', text: 'NORMAL  document.md\nready', snapshot: screen(11) },
    });
    assert.deepEqual(calls, [
      'event_subscribe',
      'pane_list',
      'terminal_read_pane',
      'pane_list',
      'terminal_read_pane',
      'event_unsubscribe',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix text wait aborts promptly, releases its subscription, and leaves reconnect usable', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-text-abort-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  let initialRead;
  const readReady = new Promise((resolve) => {
    initialRead = resolve;
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload.call);
        const reply =
          frame.payload.call === 'pane_list'
            ? {
                reply: 'panes',
                with: {
                  panes: [
                    {
                      slot: 'shell',
                      generation: 2,
                      revision: 3,
                      kind: 'terminal',
                      provider: null,
                      tab: 'tab-1',
                      title: 'Shell',
                      focused: true,
                    },
                  ],
                  truncated: false,
                },
              }
            : frame.payload.call === 'terminal_read_pane'
              ? {
                  reply: 'text',
                  with: {
                    slot: 'shell',
                    generation: 2,
                    revision: 3,
                    columns: 80,
                    rows: 24,
                    lines: ['still running'],
                    cursor_column: 13,
                    cursor_row: 0,
                    truncated: false,
                  },
                }
              : { reply: 'done' };
        socket.write(encode({ channel: frame.channel, kind: KIND.response, payload: reply }));
        if (frame.payload.call === 'terminal_read_pane') initialRead();
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'text-abort',
          granted: ['panes:observe', 'terminals:output'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const controller = new AbortController();
    const pending = workspace(session).terminal.waitForText(
      'shell',
      { generation: 2, revision: 3 },
      { timeoutMs: 30_000, signal: controller.signal },
    );
    await readReady;
    controller.abort(new Error('agent cancelled'));
    await assert.rejects(pending, /agent cancelled|aborted/);
    assert.deepEqual(calls, [
      'event_subscribe',
      'pane_list',
      'terminal_read_pane',
      'event_unsubscribe',
    ]);
    assert.equal((await workspace(session).terminal.panes()).panes[0].slot, 'shell');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix text wait reports pane removal distinctly and can observe its restored generation', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-pane-restore-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  let absent = true;
  const restored = {
    slot: 'agent',
    generation: 9,
    revision: 1,
    kind: 'terminal',
    provider: null,
    tab: 'tab-1',
    title: 'Restored shell',
    focused: true,
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload.call);
        const payload =
          frame.payload.call === 'pane_list'
            ? { reply: 'panes', with: { panes: absent ? [] : [restored], truncated: false } }
            : frame.payload.call === 'terminal_read_pane'
              ? {
                  reply: 'text',
                  with: {
                    slot: 'agent',
                    generation: 9,
                    revision: 1,
                    columns: 80,
                    rows: 24,
                    lines: ['$ restored'],
                    cursor_column: 10,
                    cursor_row: 0,
                    truncated: false,
                  },
                }
              : { reply: 'done' };
        socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
        if (frame.payload.call === 'event_unsubscribe') absent = false;
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'pane-restore-agent',
          granted: ['panes:observe', 'terminals:output'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const terminal = workspace(session).terminal;
    await assert.rejects(
      terminal.waitForText('agent', { generation: 8, revision: 12 }, { timeoutMs: 1_000 }),
      (error) =>
        error instanceof PaneUnavailableError &&
        error.slot === 'agent' &&
        error.reason === 'absent',
    );
    assert.deepEqual(calls, ['event_subscribe', 'pane_list', 'event_unsubscribe']);
    assert.deepEqual((await terminal.panes()).panes, [restored]);

    const controller = new AbortController();
    const waiting = terminal.waitForText(
      'agent',
      { generation: 9, revision: 1 },
      { timeoutMs: 30_000, signal: controller.signal },
    );
    setTimeout(() => controller.abort('agent stopped'), 5);
    await assert.rejects(waiting, (error) => error?.name === 'AbortError');
    assert.equal((await terminal.panes()).panes[0].generation, 9);
    assert.deepEqual(calls.slice(-6), [
      'pane_list',
      'event_subscribe',
      'pane_list',
      'terminal_read_pane',
      'event_unsubscribe',
      'pane_list',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

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
    assert.equal(
      requests.length,
      1,
      'denied, malformed, and oversized writes never reach socket framing',
    );
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
        payload: {
          protocol: 1,
          peer: 'state-cas-fixture',
          granted: ['state:read', 'state:write'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const api = workspace(session);
    const foreground = await api.state.read();
    const background = await api.state.read();
    assert.equal(await api.state.write(background.identity, [2]), `sha256:${'a'.repeat(64)}`);
    await assert.rejects(api.state.write(foreground.identity, [1]), /changed after it was read/);
    assert.deepEqual(await api.state.read(), {
      identity: `sha256:${'a'.repeat(64)}`,
      contents: [2],
    });
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
        socket.write(
          encode({
            channel: frame.channel,
            kind: KIND.response,
            payload: {
              reply: 'extension_catalogue',
              with: {
                complete: true,
                entries: [
                  {
                    id: 'storybook',
                    title: 'Component playground',
                    description: 'Native components',
                    version: '1.4.0',
                    reference: 'registry/storybook:latest',
                    publisher: 'Husklet',
                    source: 'first-party',
                    protocol: 1,
                    architectures: ['amd64', 'arm64'],
                  },
                  {
                    id: 'metrics',
                    title: 'Metrics explorer',
                    description: 'Workspace metrics',
                    version: '2.1.0',
                    reference: 'registry/metrics:2.1.0',
                    publisher: 'Example',
                    source: 'partner:metrics',
                    protocol: 1,
                    architectures: ['amd64'],
                  },
                ],
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
        payload: {
          protocol: 1,
          peer: 'fixture',
          granted: ['extensions:read'],
        },
      }),
    );
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
              with: [
                {
                  name: 'database-tools',
                  image_digest: 'sha256:reviewed',
                  status: 'standby',
                  version: '2.0.0',
                  enabled: false,
                  pane_providers: [],
                  granted: ['containers:read', 'filesystem:write'],
                  containers: { selectors: [{ name: 'postgres' }], create: false },
                  filesystem: {
                    read: [],
                    write: [{ exact: 'database.json' }],
                    create: [],
                    delete: [],
                    rename: [],
                  },
                  workspace_environment: {
                    read: [{ workspace: 'dev', name: 'PGPASSWORD' }],
                    write: [],
                  },
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
          peer: 'fixture',
          granted: ['extensions:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const [installed] = await workspace(session).extensions.list();
    assert.deepEqual(installed.granted, ['containers:read', 'filesystem:write']);
    assert.deepEqual(installed.containers.selectors, [{ name: 'postgres' }]);
    assert.deepEqual(installed.filesystem.write, [{ exact: 'database.json' }]);
    assert.deepEqual(installed.workspace_environment.read, [
      { workspace: 'dev', name: 'PGPASSWORD' },
    ]);
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
                entries: [
                  { path: 'src/index.ts', directory: false, size: 17, identity: 'sha256:abc' },
                ],
                complete: false,
                coalesced: 9,
                revision: 12,
                journal: FILE_JOURNAL,
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
        payload: {
          protocol: 1,
          peer: 'fixture',
          granted: [
            'containers:create',
            'containers:execute',
            'containers:lifecycle',
            'containers:remove',
          ],
        },
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

test('real Unix structured execution identifies the malformed record and preserves recovery', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-json-line-'));
  const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64);
  const executionId = 'e'.repeat(32);
  const requests = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        requests.push(frame.payload.call);
        const payload =
          frame.payload.call === 'container_exec'
            ? { reply: 'identity', with: executionId }
            : frame.payload.call === 'execution_output'
              ? {
                  reply: 'execution_output',
                  with: {
                    entries: [
                      {
                        sequence: 1,
                        timestamp_ms: 1,
                        stream: 'stdout',
                        bytes: [...Buffer.from('{"ok":true}\nnot-json\n')],
                      },
                    ],
                    next: 1,
                    more: false,
                    eof: true,
                    gap: false,
                  },
                }
              : frame.payload.call === 'workspace_info'
                ? {
                    reply: 'workspace',
                    with: { name: 'tests', image: 'toolbox', architecture: 'amd64' },
                  }
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
          peer: 'structured-test-runner',
          granted: [
            'containers:read',
            'containers:execute',
            'containers:lifecycle',
            'workspaces:read',
          ],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const values = [];
    await assert.rejects(
      workspace(session).containers.execJsonLines(
        containerId,
        3,
        { command: ['test', '--reporter=jsonl'], maxLineBytes: 4096 },
        (value) => values.push(value),
      ),
      (error) =>
        error instanceof ExecutionOperationError &&
        error.executionId === executionId &&
        error.phase === 'output' &&
        error.cause instanceof JsonLineParseError &&
        error.cause.line === 2 &&
        error.cause.cause instanceof SyntaxError,
    );
    assert.deepEqual(values, [{ ok: true }]);
    assert.deepEqual(requests, ['container_exec', 'execution_output', 'execution_cancel']);
    assert.equal((await workspace(session).info()).name, 'tests');
    assert.deepEqual(requests, [
      'container_exec',
      'execution_output',
      'execution_cancel',
      'workspace_info',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix structured execution validates result schemas before delivery and preserves recovery', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-json-schema-'));
  const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64);
  const executionId = 'e'.repeat(32);
  const requests = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        requests.push(frame.payload.call);
        const payload =
          frame.payload.call === 'container_exec'
            ? { reply: 'identity', with: executionId }
            : frame.payload.call === 'execution_output'
              ? {
                  reply: 'execution_output',
                  with: {
                    entries: [
                      {
                        sequence: 1,
                        timestamp_ms: 1,
                        stream: 'stdout',
                        bytes: [
                          ...Buffer.from(
                            '{"kind":"case","durationMs":12}\n{"kind":"case","durationMs":"slow"}\n',
                          ),
                        ],
                      },
                    ],
                    next: 1,
                    more: false,
                    eof: true,
                    gap: false,
                  },
                }
              : frame.payload.call === 'workspace_info'
                ? {
                    reply: 'workspace',
                    with: { name: 'tests', image: 'toolbox', architecture: 'amd64' },
                  }
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
          peer: 'typed-test-runner',
          granted: [
            'containers:read',
            'containers:execute',
            'containers:lifecycle',
            'workspaces:read',
          ],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    await assert.rejects(
      containers.execJsonLines(
        containerId,
        3,
        { command: ['test', '--reporter=jsonl'], maxLineBytes: 4096, decode: 'invalid' },
        () => {},
      ),
      /decode must be a function/,
    );
    assert.deepEqual(requests, [], 'invalid decoders must not start an execution');

    const values = [];
    const decode = (value) => {
      if (
        typeof value !== 'object' ||
        value === null ||
        value.kind !== 'case' ||
        typeof value.durationMs !== 'number'
      ) {
        throw new TypeError('case duration must be numeric');
      }
      return { durationMs: value.durationMs };
    };
    await assert.rejects(
      containers.execJsonLines(
        containerId,
        3,
        { command: ['test', '--reporter=jsonl'], maxLineBytes: 4096, decode },
        (value) => values.push(value),
      ),
      (error) =>
        error instanceof ExecutionOperationError &&
        error.executionId === executionId &&
        error.phase === 'output' &&
        error.cause instanceof JsonLineDecodeError &&
        error.cause.line === 2 &&
        error.cause.cause instanceof TypeError &&
        /duration must be numeric/.test(error.cause.cause.message),
    );
    assert.deepEqual(values, [{ durationMs: 12 }]);
    assert.deepEqual(requests, ['container_exec', 'execution_output', 'execution_cancel']);
    assert.equal((await workspace(session).info()).name, 'tests');
    assert.deepEqual(requests, [
      'container_exec',
      'execution_output',
      'execution_cancel',
      'workspace_info',
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix bounded query cancels before delivering an excess row and keeps the session reusable', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-query-row-limit-'));
  const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64);
  const executionId = 'e'.repeat(32);
  const requests = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        requests.push(frame.payload.call);
        const payload =
          frame.payload.call === 'container_exec'
            ? { reply: 'identity', with: executionId }
            : frame.payload.call === 'execution_output'
              ? {
                  reply: 'execution_output',
                  with: {
                    entries: [
                      {
                        sequence: 1,
                        timestamp_ms: 1,
                        stream: 'stdout',
                        bytes: [...Buffer.from('{"id":1}\n{"id":2}\n')],
                      },
                    ],
                    next: 1,
                    more: false,
                    eof: true,
                    gap: false,
                  },
                }
              : frame.payload.call === 'workspace_info'
                ? {
                    reply: 'workspace',
                    with: { name: 'database', image: 'postgres', architecture: 'amd64' },
                  }
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
          peer: 'postgres-query-grid',
          granted: [
            'containers:read',
            'containers:execute',
            'containers:lifecycle',
            'workspaces:read',
          ],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    await assert.rejects(
      containers.execJsonLines(
        containerId,
        2,
        { command: ['psql'], maxLineBytes: 4096, maxLines: 0 },
        () => {},
      ),
      /maxLines.*1.*1000000/,
    );
    assert.deepEqual(requests, [], 'invalid aggregate bounds must not start a query');
    const rows = [];
    await assert.rejects(
      containers.execJsonLines(
        containerId,
        2,
        { command: ['psql'], maxLineBytes: 4096, maxLines: 1 },
        (row) => rows.push(row),
      ),
      (error) =>
        error instanceof ExecutionOperationError &&
        error.executionId === executionId &&
        error.phase === 'output' &&
        error.cause instanceof RangeError &&
        /1 line limit/.test(error.cause.message),
    );
    assert.deepEqual(rows, [{ id: 1 }]);
    assert.deepEqual(requests, ['container_exec', 'execution_output', 'execution_cancel']);
    assert.equal((await workspace(session).info()).name, 'database');
    assert.deepEqual(requests, [
      'container_exec',
      'execution_output',
      'execution_cancel',
      'workspace_info',
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
    assert.deepEqual(
      requests.map(({ call }) => call),
      [
        'container_exec',
        'execution_output',
        'execution_cancel',
        'execution_inspect',
        'execution_remove',
        'container_list',
      ],
    );
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
    const firstEntry = (await walk.next()).value;
    assert.equal(firstEntry.path, 'src/a.ts');
    firstEntry.path = 'src/redirected';
    firstEntry.directory = true;
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

    const stable = host.files.readChunks('src/a.ts', { chunkBytes: 2 });
    const firstRange = (await stable.next()).value;
    const chunks = [...firstRange.contents];
    firstRange.contents.length = 0;
    firstRange.eof = true;
    const secondRange = (await stable.next()).value;
    chunks.push(...secondRange.contents);
    assert.deepEqual(chunks, [65, 66, 67, 68]);
    assert.equal((await stable.next()).done, true);
    assert.equal(
      requests.some(
        ({ call, with: value }) =>
          call === 'filesystem_list_page' && value.path === 'src/redirected',
      ),
      false,
      'mutating a yielded entry cannot redirect recursive traversal',
    );
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
        } else if (requested === 'backward') {
          entries = [
            {
              path: frame.payload.with.after === null ? 'backward/b' : 'backward/a',
              directory: false,
              size: 1,
            },
          ];
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
                identity:
                  requested === 'backward' ? 'directory-backward' : `directory-${listCalls}`,
                next: entries.at(-1)?.path ?? null,
                more: requested === 'backward' && frame.payload.with.after === null,
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
    await assert.rejects(
      duplicate.next(),
      /host returned an inconsistent filesystem directory page/,
    );
    const backward = files.walk('backward', { pageSize: 1 });
    assert.equal((await backward.next()).value.path, 'backward/b');
    await assert.rejects(
      backward.next(),
      /host returned an inconsistent filesystem directory page/,
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

test('real Unix output calls reject oversized pages, sequence gaps, and unknown streams', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-output-integrity-'));
  const socketPath = path.join(directory, 'host.sock');
  const executionId = 'd'.repeat(32);
  const connections = new Set();
  let outputCalls = 0;
  const entry = (sequence) => ({
    sequence,
    timestamp_ms: sequence,
    stream: 'stdout',
    bytes: [sequence],
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        if (frame.payload.call === 'execution_output') {
          outputCalls += 1;
          assert.deepEqual(frame.payload.with, { id: executionId, after: 0, limit: 1 });
          const entries =
            outputCalls === 1
              ? [entry(1), entry(2)]
              : outputCalls === 2
                ? [entry(2)]
                : [{ ...entry(1), stream: 'database' }];
          const reply = encode({
              channel: 2,
              kind: KIND.response,
              payload: {
                reply: 'execution_output',
                with: {
                  entries,
                  next: entries.at(-1).sequence,
                  more: false,
                  eof: false,
                  gap: false,
                },
              },
            });
          for (const byte of reply) socket.write(Uint8Array.of(byte));
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
          peer: 'output-integrity',
          granted: ['containers:read', 'workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const host = workspace(session);
    await assert.rejects(
      host.containers.executionOutputPages(executionId, { limit: 1 }).next(),
      /exceeded its requested entry limit/,
    );
    await assert.rejects(host.containers.executionOutput(executionId, { limit: 1 }), (error) => {
      assert(error instanceof ExecutionOutputProtocolError);
      assert.deepEqual([error.executionId, error.after, error.next], [executionId, 0, 2]);
      assert.match(error.message, /entry sequence is not contiguous/);
      return true;
    });
    await assert.rejects(
      host.containers.executionOutputPages(executionId, { limit: 1 }).next(),
      /unknown stream/,
    );
    assert.equal((await host.info()).name, 'demo', 'semantic rejection leaves the session usable');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix credential execution rejects oversized and colliding bindings before framing', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-debug-credentials-'));
  const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64);
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload);
        const payload =
          frame.payload.call === 'workspace_info'
            ? {
                reply: 'workspace',
                with: { name: 'debug', image: 'toolbox', architecture: 'amd64' },
              }
            : { reply: 'identity', with: 'e'.repeat(32) };
        socket.write(encode({ channel: 2, kind: KIND.response, payload }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'fixture',
          granted: ['containers:execute', 'credentials:inject', 'workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    const oversized = Array.from({ length: 65 }, (_, index) => [
      `DEBUG_TOKEN_${index}`,
      `debug.token.${index}`,
    ]);
    await assert.rejects(
      containers.execWithCredentials(containerId, 3, {
        command: ['debug-helper'],
        credentials: oversized,
      }),
      /at most 64/,
    );
    await assert.rejects(
      containers.execWithCredentials(containerId, 3, {
        command: ['debug-helper'],
        environment: [['DEBUG_TOKEN', 'public']],
        credentials: [['DEBUG_TOKEN', 'debug.token']],
      }),
      /environment names must be unique/,
    );
    assert.equal(calls.length, 0, 'invalid secret bindings never reach Unix framing');
    assert.equal((await workspace(session).info()).name, 'debug');
    assert.deepEqual(
      calls.map(({ call }) => call),
      ['workspace_info'],
    );
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix streaming abort interrupts a stalled consumer and cancels before another output page', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-stream-abort-'));
  const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64);
  const executionId = 'e'.repeat(32);
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        const payload =
          frame.payload.call === 'container_exec'
            ? { reply: 'identity', with: executionId }
            : frame.payload.call === 'execution_output'
              ? {
                  reply: 'execution_output',
                  with: {
                    entries: [
                      { sequence: 1, timestamp_ms: 1, stream: 'stdout', bytes: [111, 107] },
                    ],
                    next: 1,
                    more: true,
                    eof: false,
                    gap: false,
                  },
                }
              : frame.payload.call === 'execution_cancel'
                ? { reply: 'done' }
                : {
                    reply: 'workspace',
                    with: { name: 'tests', image: 'alpine', architecture: 'amd64' },
                  };
        socket.write(encode({ channel: 2, kind: KIND.response, payload }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'fixture',
          granted: ['containers:execute', 'containers:read', 'workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const host = workspace(session);
    const controller = new AbortController();
    let entered;
    const consuming = new Promise((resolve) => (entered = resolve));
    const running = host.containers.execStreaming(
      containerId,
      4,
      { command: ['npm', 'test'], signal: controller.signal },
      async () => {
        entered();
        await new Promise(() => {});
      },
    );
    await consuming;
    controller.abort('source changed');
    await assert.rejects(running, (error) => {
      assert(error instanceof ExecutionOperationError);
      assert.equal(error.executionId, executionId);
      assert.equal(error.phase, 'output');
      assert.equal(error.cause.name, 'AbortError');
      return true;
    });
    assert.deepEqual(calls.slice(0, 3), ['container_exec', 'execution_output', 'execution_cancel']);
    assert.equal(
      calls.filter((call) => call === 'execution_output').length,
      1,
      'abort cannot prefetch behind stalled consumer backpressure',
    );
    assert.equal((await host.info()).name, 'tests');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix helper EOF releases an idle dynamic input iterator without cancellation', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-lsp-eof-'));
  const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64);
  const executionId = 'e'.repeat(32);
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        const payload =
          frame.payload.call === 'container_exec'
            ? { reply: 'identity', with: executionId }
            : frame.payload.call === 'execution_output'
              ? {
                  reply: 'execution_output',
                  with: { entries: [], next: 0, more: false, eof: true, gap: false },
                }
              : frame.payload.call === 'execution_inspect'
                ? {
                    reply: 'execution',
                    with: {
                      id: executionId,
                      container_id: containerId,
                      running: false,
                      exit_code: 0,
                      pid: 22,
                      command: ['language-helper'],
                      user: 'developer',
                    },
                  }
                : {
                    reply: 'workspace',
                    with: { name: 'language', image: 'toolbox', architecture: 'amd64' },
                  };
        socket.write(encode({ channel: 2, kind: KIND.response, payload }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'fixture',
          granted: ['containers:execute', 'containers:input', 'containers:read', 'workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const host = workspace(session);
    async function* idleMessages() {
      await new Promise(() => {});
    }
    const result = await Promise.race([
      host.containers.execStreaming(
        containerId,
        3,
        { command: ['language-helper'], input: idleMessages() },
        async () => {},
      ),
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error('helper EOF left stdin lifecycle stuck')), 250),
      ),
    ]);
    assert.equal(result.execution.exit_code, 0);
    assert.deepEqual(calls, ['container_exec', 'execution_output', 'execution_inspect']);
    assert.equal(calls.includes('execution_close_input'), false);
    assert.equal(calls.includes('execution_cancel'), false);
    assert.equal((await host.info()).name, 'language');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix empty credential bindings retain least-privilege execution and session health', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-empty-credentials-'));
  const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64);
  const executionId = 'e'.repeat(32);
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        const payload =
          frame.payload.call === 'container_exec'
            ? { reply: 'identity', with: executionId }
            : frame.payload.call === 'execution_output'
              ? {
                  reply: 'execution_output',
                  with: { entries: [], next: 0, more: false, eof: true, gap: false },
                }
              : frame.payload.call === 'execution_inspect'
                ? {
                    reply: 'execution',
                    with: {
                      id: executionId,
                      container_id: containerId,
                      running: false,
                      exit_code: 0,
                      pid: 3,
                      command: ['psql'],
                      user: 'postgres',
                    },
                  }
                : {
                    reply: 'workspace',
                    with: { name: 'database', image: 'postgres:17', architecture: 'amd64' },
                  };
        socket.write(encode({ channel: 2, kind: KIND.response, payload }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'fixture',
          granted: ['containers:execute', 'containers:read', 'workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const host = workspace(session);
    const result = await host.containers.execStreaming(
      containerId,
      2,
      { command: ['psql'], credentials: [] },
      async () => {},
    );
    assert.equal(result.execution.exit_code, 0);
    assert.deepEqual(calls, ['container_exec', 'execution_output', 'execution_inspect']);
    assert.equal(calls.includes('container_exec_credential'), false);
    assert.equal((await host.info()).name, 'database');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix resumed execution commits output cursor only after consumer acknowledgement', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-execution-resume-'));
  const socketPath = path.join(directory, 'host.sock');
  const executionId = 'e'.repeat(32);
  const containerId = 'c'.repeat(64);
  const requests = [];
  const connections = new Set();
  const execution = {
    id: executionId,
    container_id: containerId,
    running: false,
    exit_code: 7,
    pid: 42,
    command: ['test', '--reporter=jsonl'],
    user: 'runner',
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        requests.push(frame.payload);
        const after = frame.payload.with?.after;
        const payload =
          frame.payload.call === 'execution_output'
            ? {
                reply: 'execution_output',
                with:
                  after === 0
                    ? {
                        entries: [
                          {
                            sequence: 1,
                            timestamp_ms: 1,
                            stream: 'stdout',
                            bytes: [...Buffer.from('{"case":"first"}\n')],
                          },
                        ],
                        next: 1,
                        more: true,
                        eof: false,
                        gap: false,
                      }
                    : {
                        entries: [
                          {
                            sequence: 2,
                            timestamp_ms: 2,
                            stream: 'stdout',
                            bytes: [...Buffer.from('{"case":"second"}\n')],
                          },
                        ],
                        next: 2,
                        more: false,
                        eof: true,
                        gap: false,
                      },
              }
            : frame.payload.call === 'execution_inspect'
              ? { reply: 'execution', with: execution }
              : {
                  reply: 'workspace',
                  with: { name: 'tests', image: 'toolbox', architecture: 'amd64' },
                };
        const response = encode({ channel: frame.channel, kind: KIND.response, payload });
        socket.write(response.subarray(0, 5));
        socket.write(response.subarray(5));
      }
    });
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'test-runner-resume',
        granted: ['containers:read', 'workspaces:read'],
      },
    });
    socket.write(greeting.subarray(0, 3));
    socket.write(greeting.subarray(3));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const containers = workspace(session).containers;
    await assert.rejects(
      containers.resumeExecutionStreaming(executionId, { after: -1 }, () => {}),
      /nonnegative safe integer/,
    );
    await assert.rejects(
      containers.resumeExecutionStreaming(executionId, { maxPages: 0 }, () => {}),
      /maxPages.*1.*1000000/,
    );
    const stopped = new AbortController();
    stopped.abort('query view closed');
    await assert.rejects(
      containers.resumeExecutionStreaming(
        executionId,
        { maxPages: 1, signal: stopped.signal },
        () => {},
      ),
      (error) => error.name === 'AbortError' && error.cause === stopped.signal.reason,
    );
    assert.deepEqual(requests, [], 'invalid resume cursors must not emit a request frame');

    const windowPages = [];
    const window = await containers.resumeExecutionStreaming(
      executionId,
      { after: 0, pageLimit: 1, maxPages: 1, pollIntervalMs: 10 },
      (page) => windowPages.push(page.next),
    );
    assert.deepEqual(windowPages, [1]);
    assert.deepEqual(window, {
      executionId,
      next: 1,
      pages: 1,
      complete: false,
    });
    assert.equal(
      requests.some(({ call }) => call === 'execution_inspect'),
      false,
      'a bounded output window must not claim a still-running execution is complete',
    );

    const firstAttempt = [];
    let resumeAfter;
    await assert.rejects(
      containers.resumeExecutionStreaming(
        executionId,
        { after: window.next, pageLimit: 1, pollIntervalMs: 10 },
        async (page) => {
          firstAttempt.push(page.next);
          await Promise.resolve();
          if (page.next === 2) throw new Error('result store unavailable');
        },
      ),
      (error) => {
        assert(error instanceof ExecutionOperationError);
        assert.equal(error.executionId, executionId);
        assert.equal(error.phase, 'output');
        assert.equal(error.after, 1);
        resumeAfter = error.after;
        return true;
      },
    );
    assert.deepEqual(firstAttempt, [2]);
    assert.equal(
      requests.some(({ call }) => call === 'execution_cancel'),
      false,
      'a resumed observer must not cancel an execution it did not create',
    );

    const retried = [];
    const result = await containers.resumeExecutionStreaming(
      executionId,
      { after: resumeAfter, pageLimit: 1, pollIntervalMs: 10 },
      (page) => retried.push(page.next),
    );
    assert.deepEqual(retried, [2]);
    assert.deepEqual(result, { executionId, execution, next: 2, pages: 1, complete: true });
    assert.deepEqual(
      requests
        .filter(({ call }) => call === 'execution_output')
        .map(({ with: parameters }) => parameters.after),
      [0, 1, 1],
    );
    assert.equal((await workspace(session).info()).name, 'tests');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix output iteration rejects a stalled continuation without looping or poisoning the session', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-output-stall-'));
  const socketPath = path.join(directory, 'host.sock');
  const executionId = 'd'.repeat(32);
  const connections = new Set();
  let outputCalls = 0;
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        const payload =
          frame.payload.call === 'execution_output'
            ? (() => {
                outputCalls += 1;
                return {
                  reply: 'execution_output',
                  with: { entries: [], next: 0, more: true, eof: false, gap: false },
                };
              })()
            : {
                reply: 'workspace',
                with: { name: 'database', image: 'postgres:17', architecture: 'amd64' },
              };
        socket.write(encode({ channel: 2, kind: KIND.response, payload }));
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
    const pages = host.containers.executionOutputPages(executionId, { pollIntervalMs: 10 });
    await assert.rejects(pages.next(), (error) => {
      assert(error instanceof ExecutionOutputProtocolError);
      assert.deepEqual([error.executionId, error.after, error.next], [executionId, 0, 0]);
      assert.match(error.message, /continued page did not advance/);
      return true;
    });
    assert.equal(outputCalls, 1, 'an invalid continuation cannot drive another output request');
    assert.equal((await host.info()).name, 'database', 'the ordered session remains usable');
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
  let replacement = false;
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
                  generation: replacement ? 3 : 2,
                  revision: replacement ? 1 : 4,
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
                      generation: replacement ? 3 : 2,
                      revision: replacement ? 1 : 4,
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
                  generation: replacement ? 3 : 2,
                  revision: replacement ? 1 : 4,
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
    calls.length = 0;
    replacement = true;
    await assert.rejects(
      workspace(session).terminal.actAndWait('settings', {
        generation: 2,
        revision: 4,
        node: 7,
        action: 'invoke',
      }),
      /semantic action pane was replaced before its result could be verified/,
    );
    assert.equal(calls.at(-1), 'event_unsubscribe');
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
                  state: 'inspecting',
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

test('real Unix acquisition cancellation proves exact terminal status across fragmented frames', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-acquisition-cancel-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  let cancelled = false;
  const fragmented = (socket, value) => {
    const frame = encode(value);
    socket.write(frame.subarray(0, 3));
    socket.write(frame.subarray(3, 11));
    socket.write(frame.subarray(11));
  };
  const status = () => ({
    job: 'job-cancel',
    reference: 'registry.example/tool:1',
    revision: cancelled ? 5 : 4,
    state: cancelled ? 'cancelled' : 'inspecting',
    progress: null,
    candidate: null,
    error: null,
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload.call);
        if (frame.payload.call === 'extension_acquisition_status') {
          fragmented(socket, {
            channel: frame.channel,
            kind: KIND.response,
            payload: { reply: 'extension_acquisition', with: status() },
          });
        } else if (frame.payload.call === 'extension_acquisition_cancel') {
          assert.deepEqual(frame.payload.with, { job: 'job-cancel', revision: 4 });
          cancelled = true;
          fragmented(socket, {
            channel: 21,
            kind: KIND.event,
            payload: {
              snapshot: 'extension_acquisitions',
              of: { job: 'job-cancel', revision: 5, state: 'cancelled', coalesced: 0 },
            },
          });
          fragmented(socket, {
            channel: frame.channel,
            kind: KIND.response,
            payload: { reply: 'done' },
          });
        } else {
          fragmented(socket, {
            channel: frame.channel,
            kind: KIND.response,
            payload: { reply: 'done' },
          });
        }
      }
    });
    fragmented(socket, {
      channel: CONTROL,
      kind: KIND.open,
      payload: { protocol: 1, peer: 'acquisition-cancel', granted: ['extensions:install'] },
    });
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const extensions = workspace(session).extensions;
    const result = await extensions.cancelAcquisitionAndWait('job-cancel', 4, {
      timeoutMs: 100,
    });
    assert.equal(result.changed, true);
    assert.equal(result.status.state, 'cancelled');
    assert.equal(result.status.revision, 5);
    assert.deepEqual(calls, [
      'event_subscribe',
      'extension_acquisition_status',
      'extension_acquisition_cancel',
      'extension_acquisition_status',
      'event_unsubscribe',
    ]);
    assert.deepEqual(await extensions.acquisition('job-cancel'), status());
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix acquisition rejects impossible progress without poisoning the session', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-acquisition-progress-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        const payload =
          frame.payload.call === 'extension_acquisition_status'
            ? {
                reply: 'extension_acquisition',
                with: {
                  job: 'job-9',
                  reference: 'registry.example/tool:1',
                  revision: 3,
                  state: 'pulling',
                  progress: { status: 'downloading', id: 'layer', current: 101, total: 100 },
                  candidate: null,
                  error: null,
                },
              }
            : { reply: 'extension_acquisition_job', with: { job: 'job-10' } };
        socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'catalogue', granted: ['extensions:install'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const extensions = workspace(session).extensions;
    await assert.rejects(extensions.acquisition('job-9'), /inconsistent extension acquisition/);
    assert.equal((await extensions.startAcquisition('registry.example/tool:2')).job, 'job-10');
    assert.deepEqual(
      calls.map(({ call }) => call),
      ['extension_acquisition_status', 'extension_acquisition_start'],
    );
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

test('throwing AbortSignal cleanup cannot strand ordered Unix calls before a trickled late reply', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-abort-cleanup-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  let observedTwo;
  const twoCalls = new Promise((resolve) => {
    observedTwo = resolve;
  });
  let peerClosed;
  const closed = new Promise((resolve) => {
    peerClosed = resolve;
  });
  const late = encode({
    channel: 2,
    kind: KIND.response,
    payload: {
      reply: 'workspace',
      with: { name: 'late', image: 'alpine', architecture: 'amd64' },
    },
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('error', () => {});
    socket.on('close', () => {
      connections.delete(socket);
      peerClosed();
    });
    const reader = new Reader();
    let calls = 0;
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2 || frame.kind !== KIND.request) continue;
        calls += 1;
        if (calls === 2) observedTwo();
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          peer: 'abort_cleanup',
          granted: ['workspaces:read'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));

  class ThrowingCleanupSignal {
    aborted = false;
    reason;
    listener;

    addEventListener(_name, listener) {
      this.listener = listener;
    }

    removeEventListener() {
      throw new Error('hostile AbortSignal cleanup');
    }

    abort(reason) {
      this.aborted = true;
      this.reason = reason;
      this.listener();
    }
  }

  try {
    const replies = [];
    const session = await connect({
      path: socketPath,
      timeout: 1_000,
      onReply: (reply) => replies.push(reply),
    });
    const signal = new ThrowingCleanupSignal();
    const first = session.call('workspace_info', undefined, { signal });
    const second = session.call('workspace_list');
    await twoCalls;

    assert.doesNotThrow(() => signal.abort('cancel the first ordered slot'));
    const firstRejected = assert.rejects(first, { name: 'AbortError' });
    const secondRejected = assert.rejects(second, { name: 'AbortError' });
    const peer = [...connections][0];
    peer.write(late.subarray(0, 7));
    setImmediate(() => peer.write(late.subarray(7)));

    await Promise.all([firstRejected, secondRejected]);
    await Promise.race([
      closed,
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error('cancelled ordered socket stayed open')), 200),
      ),
    ]);
    assert.deepEqual(replies, [], 'the late abandoned reply has no live caller or GUI callback');
    assert.match((await session.closed).message, /aborted/);
  } finally {
    for (const connection of connections) connection.destroy();
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

test('a coalesced Unix close revokes later GUI and row frames in the same read', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-close-boundary-'));
  const socketPath = path.join(directory, 'host.sock');
  let peer;
  const returned = [];
  const server = net.createServer((socket) => {
    peer = socket;
    socket.on('error', () => {});
    const reader = new Reader();
    socket.on('data', (chunk) => returned.push(...reader.take(chunk)));
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'close_boundary', granted: [] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  const rows = [];
  const events = [];
  let session;
  try {
    session = await connect({
      path: socketPath,
      timeout: 500,
      onRows: (request, channel) => rows.push({ request, channel }),
      onEvent: (event) => events.push(event),
    });
    peer.write(
      Buffer.concat([
        encode({ channel: CONTROL, kind: KIND.close, payload: Buffer.alloc(0) }),
        encode({
          channel: 19,
          kind: KIND.event,
          payload: { pane_provider: 'database', slot: 'retired-pane' },
        }),
        encode({
          channel: 23,
          kind: KIND.event,
          payload: {
            id: 7,
            source: 3,
            version: 11,
            range: { start: 0, count: 20 },
          },
        }),
      ]),
    );

    assert.match((await session.closed).message, /host closed the session/);
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(events, [], 'a retired GUI session delivers no coalesced interaction');
    assert.deepEqual(rows, [], 'a retired GUI session grants no row-provider authority');
    assert.equal(
      returned.some((frame) => frame.channel === 19 || frame.channel === 23),
      false,
      'post-close frames receive neither credit nor a row response',
    );
  } finally {
    await session?.close();
    peer?.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('an asynchronous row listener failure closes its real Unix request generation', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-row-listener-error-'));
  const socketPath = path.join(directory, 'host.sock');
  let peer;
  let reportPeerClosed;
  const peerClosed = new Promise((resolve) => {
    reportPeerClosed = resolve;
  });
  const returned = [];
  const server = net.createServer((socket) => {
    peer = socket;
    socket.on('error', () => {});
    socket.on('close', reportPeerClosed);
    const reader = new Reader();
    socket.on('data', (chunk) => returned.push(...reader.take(chunk)));
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'row_listener_error', granted: [] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  let session;
  try {
    session = await connect({
      path: socketPath,
      timeout: 500,
      onRows: async () => {
        await new Promise((resolve) => setImmediate(resolve));
        throw new Error('row provider failed asynchronously');
      },
    });
    peer.write(
      encode({
        channel: 41,
        kind: KIND.event,
        payload: {
          id: 13,
          source: 5,
          version: 8,
          range: { start: 20, count: 10 },
        },
      }),
    );

    const reason = await Promise.race([
      session.closed,
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error('failed row generation stayed live')), 200),
      ),
    ]);
    assert.match(reason.message, /row provider failed asynchronously/);
    await Promise.race([
      peerClosed,
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error('failed row socket stayed open')), 200),
      ),
    ]);
    assert.equal(
      returned.some((frame) => frame.channel === 41),
      false,
      'failed row work emits neither a reply nor authority-restoring credit',
    );
    await assert.rejects(session.call('workspace_info'), /session is closed/);
  } finally {
    await session?.close();
    peer?.destroy();
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
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
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
    await assert.rejects(
      session.call('state_write', { observed: 'absent', contents: [] }),
      /closed/,
    );
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
    assert.equal(
      events.length,
      delivered,
      'closed sessions deliver no later subscription callbacks',
    );
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
    const stopSecond = await host.watchContainers((snapshot) =>
      delivered.push(['second', snapshot]),
    );

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

test('a pre-greeting Unix event flood reaches no callbacks and permits fresh reconnect', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-pregreeting-events-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  let generation = 0;
  const event = {
    interaction: 'key',
    trigger: 'Key',
    node: 7,
    id: '7:Key',
    slot: 'pane-1',
    key: 'a',
    keycode: 38,
    modifiers: 0,
    pressed: true,
  };
  const server = net.createServer((socket) => {
    generation += 1;
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: { protocol: 1, peer: `generation-${generation}`, granted: [] },
    });
    if (generation === 1) {
      const flood = Array.from({ length: 4 }, (_, index) =>
        encode({ channel: 80 + index, kind: KIND.event, payload: event }),
      );
      socket.write(Buffer.concat([...flood, greeting]));
    } else {
      socket.write(greeting);
    }
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const delivered = [];
    const closed = [];
    await assert.rejects(
      connect({
        path: socketPath,
        connectTimeout: 1_000,
        pendingLimit: 2,
        onEvent: (received) => delivered.push(received),
        onClose: (error) => closed.push(error.message),
      }),
      /non-control frame before the greeting/,
    );
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(delivered, [], 'unnegotiated bytes never reach GUI callbacks');
    assert.deepEqual(closed, ['extension host sent a non-control frame before the greeting']);

    const second = await connect({ path: socketPath, connectTimeout: 1_000 });
    assert.deepEqual(second.granted, []);
    await second.close();
    assert.equal(generation, 2, 'the same listener accepts a clean negotiated generation');
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
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
  const entry = () =>
    new Promise((resolve) => {
      reportEntered = resolve;
    });
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
    peer.write(
      Buffer.concat([
        encode({
          channel: 19,
          kind: KIND.event,
          payload: { pane_provider: 'one', slot: 'pane-1' },
        }),
        encode({
          channel: 19,
          kind: KIND.event,
          payload: { pane_provider: 'two', slot: 'pane-2' },
        }),
      ]),
    );
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
    for (
      let attempt = 0;
      attempt < 20 && returned.filter((frame) => frame.kind === KIND.credit).length < 2;
      attempt += 1
    )
      await new Promise((resolve) => setTimeout(resolve, 5));
    assert.equal(returned.filter((frame) => frame.kind === KIND.credit).length, 2);
  } finally {
    await session.close();
    peer?.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('a coalesced channel close revokes queued GUI delivery and credit', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-event-channel-close-'));
  const socketPath = path.join(directory, 'host.sock');
  let peer;
  const returned = [];
  const server = net.createServer((socket) => {
    peer = socket;
    socket.on('error', () => {});
    const reader = new Reader();
    socket.on('data', (chunk) => returned.push(...reader.take(chunk)));
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'event_channel_close', granted: [] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  const delivered = [];
  let session;
  try {
    session = await connect({
      path: socketPath,
      timeout: 500,
      onEvent: (event) => delivered.push(`primary:${event.slot}`),
    });
    session.onEvent((event) => delivered.push(`secondary:${event.slot}`));
    peer.write(
      Buffer.concat([
        encode({
          channel: 37,
          kind: KIND.event,
          payload: { pane_provider: 'database', slot: 'retired-pane' },
        }),
        encode({ channel: 37, kind: KIND.close, payload: Buffer.alloc(0) }),
      ]),
    );

    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(delivered, [], 'a closed event generation reaches no GUI listener');
    assert.equal(
      returned.some((frame) => frame.channel === 37 && frame.kind === KIND.credit),
      false,
      'a retired channel receives no credit that could authorize more stale events',
    );

    peer.write(
      encode({
        channel: 37,
        kind: KIND.event,
        payload: { pane_provider: 'database', slot: 'replacement-pane' },
      }),
    );
    for (let attempt = 0; attempt < 20 && delivered.length < 2; attempt += 1)
      await new Promise((resolve) => setTimeout(resolve, 5));
    assert.deepEqual(delivered, ['primary:replacement-pane', 'secondary:replacement-pane']);
    assert.equal(
      returned.filter((frame) => frame.channel === 37 && frame.kind === KIND.credit).length,
      1,
      'only the live replacement generation returns credit',
    );
  } finally {
    await session?.close();
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
  const firstEntered = new Promise((resolve) => {
    reportEntered = resolve;
  });
  const dispose = session.onEvent(async (event) => {
    entered.push(event.slot);
    reportEntered();
    await new Promise((resolve) => {
      release = resolve;
    });
  });
  try {
    peer.write(
      Buffer.concat([
        encode({
          channel: 23,
          kind: KIND.event,
          payload: { pane_provider: 'one', slot: 'pane-1' },
        }),
        encode({
          channel: 23,
          kind: KIND.event,
          payload: { pane_provider: 'two', slot: 'pane-2' },
        }),
      ]),
    );
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
    required: [],
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
          assert.deepEqual(frame.payload.with.containers, {
            selectors: [{ name: 'postgres' }],
            create: false,
          });
          assert.deepEqual(frame.payload.with.filesystem, {
            read: [{ exact: 'schema.sql' }],
            write: [],
            create: [],
            delete: [],
            rename: [],
          });
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
    const result = await workspace(session).extensions.installAndWait('job-1', 7, {
      capabilities: ['extensions:read'],
      containers: { selectors: [{ name: 'postgres' }], create: false },
      filesystem: {
        read: [{ exact: 'schema.sql' }],
        write: [],
        create: [],
        delete: [],
        rename: [],
      },
    });
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

test('real Unix install wait rejects broader published authority and preserves the session', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-install-authority-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const digest = `sha256:${'b'.repeat(64)}`;
  const candidate = {
    name: 'reviewed',
    version: '2',
    image_digest: digest,
    requested: ['extensions:read'],
    required: [],
    installed_image_digest: null,
  };
  const committed = {
    name: 'reviewed',
    image_digest: digest,
    version: '2',
    status: 'standby',
    enabled: false,
    pane_providers: [],
    granted: ['extensions:read'],
  };
  const broadened = { ...committed, granted: ['extensions:read', 'extensions:install'] };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (frame.payload.call === 'extension_acquisition_status') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: {
                reply: 'extension_acquisition',
                with: {
                  job: 'job-authority',
                  reference: 'reviewed:2',
                  revision: 4,
                  state: 'ready',
                  progress: null,
                  candidate,
                  error: null,
                },
              },
            }),
          );
        } else if (frame.payload.call === 'extension_install') {
          socket.write(
            encode({
              channel: 41,
              kind: KIND.event,
              payload: { snapshot: 'extensions', of: [broadened] },
            }),
          );
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'extension', with: committed },
            }),
          );
        } else if (frame.payload.call === 'extension_list') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'extensions', with: [committed] },
            }),
          );
        } else {
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
          peer: 'install-authority',
          granted: ['extensions:read', 'extensions:install'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const extensions = workspace(session).extensions;
    await assert.rejects(
      extensions.installAndWait('job-authority', 4, {
        capabilities: ['extensions:read'],
      }),
      /replaced or disappeared after install/,
    );
    assert.deepEqual(await extensions.list(), [committed]);
    assert.deepEqual(calls, [
      'extension_acquisition_status',
      'event_subscribe',
      'extension_install',
      'event_unsubscribe',
      'extension_list',
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
          granted: [
            'containers:read',
            'containers:create',
            'containers:execute',
            'containers:lifecycle',
            'containers:remove',
          ],
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
          granted: [
            'containers:read',
            'containers:create',
            'containers:execute',
            'containers:lifecycle',
            'containers:remove',
          ],
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
          granted: [
            'containers:read',
            'containers:create',
            'containers:execute',
            'containers:lifecycle',
            'containers:remove',
          ],
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
          granted: [
            'containers:read',
            'containers:create',
            'containers:execute',
            'containers:lifecycle',
            'containers:remove',
          ],
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

test('real Unix focusAndWait uses the least-privilege focus grant and verifies exact identity', async () => {
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
          granted: ['panes:observe', 'terminals:focus'],
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
  let delayFirstAdvance = true;
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
          const stale = delayFirstAdvance ? reads <= 2 : reads === 1;
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: {
                reply: 'text',
                with: screen(stale ? 7 : 8, stale ? ['$ '] : ['$ ^C']),
              },
            }),
          );
          if (writes === 1 && reads === 2) {
            delayFirstAdvance = false;
            const advanced = encode({
              channel: 100,
              kind: KIND.event,
              payload: {
                snapshot: 'pane_changes',
                of: { slot, kind: 'terminal', generation: 4, revision: 8, coalesced: 0 },
              },
            });
            socket.write(advanced.subarray(0, 5));
            setImmediate(() => socket.write(advanced.subarray(5)));
          }
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
    assert.equal(
      calls.at(-1),
      'terminal_read_pane',
      'the session remains usable after cancellation',
    );
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix quiet terminal wait does not mistake local echo for an agent response', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-terminal-quiet-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const slot = 'agent-pane';
  let revision = 7;
  let subscriptions = 0;
  let writes = 0;
  let outputTimer;
  const screen = () => ({
    slot,
    generation: 4,
    revision,
    columns: 80,
    rows: 24,
    lines:
      revision === 7
        ? ['$ ']
        : revision === 8
          ? ['$ explain this']
          : ['$ explain this', 'The command response arrived.'],
    cursor_column: 0,
    cursor_row: revision === 9 ? 2 : 1,
    truncated: false,
  });
  const changed = (socket) =>
    socket.write(
      encode({
        channel: 100,
        kind: KIND.event,
        payload: {
          snapshot: 'pane_changes',
          of: { slot, kind: 'terminal', generation: 4, revision, coalesced: 0 },
        },
      }),
    );
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (frame.payload.call === 'event_subscribe') {
          subscriptions += 1;
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
          if (subscriptions === 2) {
            outputTimer = setTimeout(() => {
              revision = 9;
              changed(socket);
            }, 5);
          }
        } else if (frame.payload.call === 'event_unsubscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'terminal_read_pane') {
          socket.write(
            encode({ channel: 2, kind: KIND.response, payload: { reply: 'text', with: screen() } }),
          );
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
                      slot,
                      generation: 4,
                      revision,
                      kind: 'terminal',
                      provider: null,
                      tab: null,
                      title: 'Agent',
                      focused: true,
                    },
                  ],
                  truncated: false,
                },
              },
            }),
          );
        } else if (frame.payload.call === 'terminal_write_pane') {
          writes += 1;
          assert.deepEqual(frame.payload.with, {
            slot,
            generation: 4,
            revision: writes === 1 ? 7 : 9,
            contents: [
              ...new TextEncoder().encode(writes === 1 ? 'explain this\n' : 'still running\n'),
            ],
          });
          if (writes === 1) {
            revision = 8;
            changed(socket);
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
          peer: 'terminal-quiet-agent',
          granted: ['panes:observe', 'terminals:output', 'terminals:input'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const terminal = workspace(session).terminal;
    const before = screen();
    await assert.rejects(
      terminal.writeObservedAndWaitForQuietText(before, 'explain this\n', { quietMs: 0 }),
      /quiet window/,
    );
    assert.deepEqual(calls, [], 'invalid settling policy must send no request');
    const result = await terminal.writeObservedAndWaitForQuietText(before, 'explain this\n', {
      lines: 40,
      quietMs: 20,
      timeoutMs: 1_000,
    });
    assert.equal(result.changed, true);
    assert.equal(result.settled, true);
    assert.equal(result.after.snapshot.revision, 9);
    assert.match(result.after.text, /command response arrived/);
    assert.ok(
      calls.filter((call) => call === 'terminal_read_pane').length >= 4,
      'the helper reads beyond the first echoed revision',
    );
    const cancellation = new AbortController();
    const cancelled = terminal.writeObservedAndWaitForQuietText(
      result.after.snapshot,
      'still running\n',
      {
        quietMs: 20,
        timeoutMs: 1_000,
        signal: cancellation.signal,
      },
    );
    setTimeout(() => cancellation.abort('agent stopped'), 5);
    await assert.rejects(cancelled, (error) => error?.name === 'AbortError');
    assert.equal((await terminal.read(slot)).revision, 9, 'the ordered session remains usable');
    await session.close();
  } finally {
    clearTimeout(outputTimer);
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix writeAndWait refuses to attribute a replacement pane screen to sent input', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-write-replacement-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  let reads = 0;
  const screen = (generation, revision, line) => ({
    slot: 'pane-input',
    generation,
    revision,
    columns: 80,
    rows: 24,
    lines: [line],
    cursor_column: 0,
    cursor_row: 0,
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
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: {
                reply: 'text',
                with: reads === 1 ? screen(4, 7, '$ ') : screen(5, 1, 'replacement'),
              },
            }),
          );
        } else if (frame.payload.call === 'terminal_write_pane') {
          assert.deepEqual(frame.payload.with, {
            slot: 'pane-input',
            generation: 4,
            revision: 7,
            contents: [3],
          });
          socket.write(
            encode({
              channel: 100,
              kind: KIND.event,
              payload: {
                snapshot: 'pane_changes',
                of: {
                  slot: 'pane-input',
                  kind: 'terminal',
                  generation: 5,
                  revision: 1,
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
          peer: 'write-replacement',
          granted: ['panes:observe', 'terminals:output', 'terminals:input'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const terminal = workspace(session).terminal;
    await assert.rejects(
      terminal.writeAndWait('pane-input', 4, 7, [3]),
      /pane was replaced before input result could be verified/,
    );
    assert.deepEqual(calls, [
      'event_subscribe',
      'terminal_read_pane',
      'terminal_write_pane',
      'terminal_read_pane',
      'event_unsubscribe',
    ]);
    assert.equal((await terminal.read('pane-input')).generation, 5);
    assert.equal(calls.at(-1), 'terminal_read_pane', 'the ordered session remains usable');
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
          granted: [
            'containers:read',
            'containers:create',
            'containers:execute',
            'containers:lifecycle',
            'containers:remove',
          ],
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
          granted: [
            'containers:read',
            'containers:create',
            'containers:execute',
            'containers:lifecycle',
            'containers:remove',
          ],
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

test('real Unix pinTabAndWait ignores stale inventory, verifies exact state, and preserves the session', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-pin-tab-wait-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  let pinCalls = 0;
  const unpinned = { id: 'tab-agent', title: 'Agent', pinned: false, panes: [] };
  const pinned = { ...unpinned, pinned: true };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload);
        if (frame.payload.call === 'event_subscribe') {
          assert.deepEqual(frame.payload.with, { topic: 'terminal' });
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
          socket.write(
            encode({
              channel: 131,
              kind: KIND.event,
              payload: { snapshot: 'terminal', of: [unpinned] },
            }),
          );
        } else if (frame.payload.call === 'terminal_pin_tab') {
          pinCalls += 1;
          assert.deepEqual(frame.payload.with, {
            tab: 'tab-agent',
            pinned: pinCalls === 1,
          });
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
          socket.write(
            encode({
              channel: 131,
              kind: KIND.event,
              payload: { snapshot: 'terminal', of: pinCalls === 1 ? [pinned] : [] },
            }),
          );
        } else if (frame.payload.call === 'event_unsubscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'terminal_tabs') {
          socket.write(
            encode({
              channel: 2,
              kind: KIND.response,
              payload: { reply: 'tabs', with: [pinned] },
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
          peer: 'pin-tab-wait',
          granted: ['terminals:read', 'terminals:layout-control'],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const terminal = workspace(session).terminal;
    await assert.rejects(terminal.pinTabAndWait('', true), /tab identity/);
    await assert.rejects(terminal.pinTabAndWait('tab-agent', 'yes'), /state must be boolean/);
    await assert.rejects(terminal.pinTabAndWait('tab-agent', true, { timeoutMs: 0 }), /timeout/);
    assert.deepEqual(calls, [], 'invalid authority must emit no request');
    assert.deepEqual(await terminal.pinTabAndWait('tab-agent'), { changed: true, tab: pinned });
    await assert.rejects(
      terminal.pinTabAndWait('tab-agent', false),
      /tab tab-agent disappeared while pinning/,
    );
    assert.deepEqual(await terminal.tabs(), [pinned]);
    assert.deepEqual(
      calls.map(({ call }) => call),
      [
        'event_subscribe',
        'terminal_pin_tab',
        'event_unsubscribe',
        'event_subscribe',
        'terminal_pin_tab',
        'event_unsubscribe',
        'terminal_tabs',
      ],
    );
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

function semanticTree(slot, revision = 4, generation = 2) {
  return {
    slot,
    generation,
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
          label: 'Apply',
          value: null,
          disabled: false,
          destructive: false,
          actions: ['invoke'],
          children: [],
        },
      ],
    },
  };
}

async function withPaneIdentityHost(granted, respond, exercise) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-pane-identity-'));
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
        respond(frame.payload, socket);
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'pane-identity', granted },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    await exercise(session, calls);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
}

test('real Unix inspectAndAct rejects wrong pre-action pane identity before mutation', async () => {
  await withPaneIdentityHost(
    ['panes:observe', 'panes:semantic-read', 'panes:semantic-control', 'terminals:read'],
    (request, socket) => {
      let payload;
      if (request.call === 'pane_semantic_read') {
        payload = { reply: 'semantics', with: semanticTree('pane-b') };
      } else if (request.call === 'pane_list') {
        payload = { reply: 'panes', with: { panes: [], truncated: false } };
      } else {
        payload = { reply: 'done' };
      }
      socket.write(encode({ channel: 2, kind: KIND.response, payload }));
    },
    async (session, calls) => {
      const terminal = workspace(session).terminal;
      await assert.rejects(
        terminal.inspectAndAct('pane-a', { node: 7, action: 'invoke' }),
        /pane semantics for pane pane-b, expected pane-a; no pane state was assumed/,
      );
      assert.equal(
        calls.some(({ call }) => call === 'pane_semantic_action'),
        false,
        'mismatched inspected authority must never mutate the requested pane',
      );
      assert.deepEqual(await terminal.panes(), { panes: [], truncated: false });
    },
  );
});

test('real Unix inspectAndAct rejects a replacement generation after mutation', async () => {
  let reads = 0;
  await withPaneIdentityHost(
    ['panes:observe', 'panes:semantic-read', 'panes:semantic-control'],
    (request, socket) => {
      if (request.call === 'pane_semantic_read') {
        reads += 1;
        socket.write(
          encode({
            channel: 2,
            kind: KIND.response,
            payload: {
              reply: 'semantics',
              with: semanticTree('pane-a', reads === 1 ? 4 : 1, reads === 1 ? 2 : 3),
            },
          }),
        );
      } else if (request.call === 'pane_semantic_action') {
        socket.write(
          encode({
            channel: 91,
            kind: KIND.event,
            payload: {
              snapshot: 'pane_changes',
              of: {
                slot: 'pane-a',
                kind: 'native',
                generation: 2,
                revision: 5,
                coalesced: 0,
              },
            },
          }),
        );
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      } else {
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      }
    },
    async (session, calls) => {
      await assert.rejects(
        workspace(session).terminal.inspectAndAct('pane-a', { node: 7, action: 'invoke' }),
        /inspected semantic pane was replaced before action verification/,
      );
      assert.equal(calls.filter(({ call }) => call === 'pane_semantic_action').length, 1);
      assert.equal((await workspace(session).terminal.semantics('pane-a')).generation, 3);
    },
  );
});

test('real Unix semantic action waits cancel, release observation, and preserve the session', async () => {
  let cancel;
  await withPaneIdentityHost(
    ['panes:observe', 'panes:semantic-read', 'panes:semantic-control'],
    (request, socket) => {
      const payload =
        request.call === 'pane_semantic_read'
          ? { reply: 'semantics', with: semanticTree('pane-a') }
          : { reply: 'done' };
      socket.write(encode({ channel: 2, kind: KIND.response, payload }));
      if (request.call === 'pane_semantic_action') {
        setTimeout(() => cancel.abort('agent request superseded'), 10);
      }
    },
    async (session, calls) => {
      const terminal = workspace(session).terminal;
      const alreadyCancelled = new AbortController();
      alreadyCancelled.abort('agent stopped');
      await assert.rejects(
        terminal.inspectAndAct(
          'pane-a',
          { node: 7, action: 'invoke' },
          { signal: alreadyCancelled.signal },
        ),
        (error) => error.name === 'AbortError' && error.cause === 'agent stopped',
      );
      assert.deepEqual(calls, [], 'pre-cancelled semantic work must not subscribe or mutate');

      cancel = new AbortController();
      await assert.rejects(
        terminal.actAndWait(
          'pane-a',
          { generation: 2, revision: 4, node: 7, action: 'invoke' },
          { timeoutMs: 1_000, signal: cancel.signal },
        ),
        (error) => error.name === 'AbortError' && error.cause === 'agent request superseded',
      );
      assert.deepEqual(calls, [
        { call: 'event_subscribe', with: { topic: 'pane-changes' } },
        {
          call: 'pane_semantic_action',
          with: {
            slot: 'pane-a',
            action: {
              generation: 2,
              revision: 4,
              node: 7,
              action: 'invoke',
            },
          },
        },
        { call: 'event_unsubscribe', with: { topic: 'pane-changes' } },
      ]);

      calls.length = 0;
      cancel = new AbortController();
      await assert.rejects(
        terminal.inspectAndAct(
          'pane-a',
          { node: 7, action: 'invoke' },
          { timeoutMs: 1_000, signal: cancel.signal },
        ),
        (error) => error.name === 'AbortError' && error.cause === 'agent request superseded',
      );
      assert.deepEqual(
        calls.map(({ call }) => call),
        [
          'event_subscribe',
          'pane_semantic_read',
          'pane_semantic_action',
          'event_unsubscribe',
        ],
      );

      calls.length = 0;
      assert.equal((await terminal.semantics('pane-a')).revision, 4);
      assert.deepEqual(calls.map(({ call }) => call), ['pane_semantic_read']);
    },
  );
});

test('real Unix terminal-to-text rejects text carrying another pane identity', async () => {
  await withPaneIdentityHost(
    ['panes:observe', 'terminals:output'],
    (request, socket) => {
      const payload =
        request.call === 'pane_list'
          ? {
              reply: 'panes',
              with: {
                panes: [
                  {
                    slot: 'pane-a',
                    generation: 3,
                    revision: 8,
                    kind: 'terminal',
                    provider: null,
                    tab: null,
                    title: 'Shell',
                    focused: true,
                  },
                ],
                truncated: false,
              },
            }
          : {
              reply: 'text',
              with: {
                slot: 'pane-b',
                generation: 3,
                revision: 8,
                columns: 80,
                rows: 24,
                lines: ['$ unsafe'],
                cursor_column: 8,
                cursor_row: 0,
                truncated: false,
              },
            };
      socket.write(encode({ channel: 2, kind: KIND.response, payload }));
    },
    async (session) => {
      await assert.rejects(
        workspace(session).terminal.toText('pane-a'),
        /terminal text for pane pane-b, expected pane-a; no pane state was assumed/,
      );
    },
  );
});

test('real Unix pane inventory rejects duplicate slots and preserves session health', async () => {
  let inventories = 0;
  await withPaneIdentityHost(
    ['panes:observe'],
    (request, socket) => {
      inventories += 1;
      const pane = {
        slot: 'pane-a',
        generation: 3,
        revision: 8,
        kind: 'terminal',
        provider: null,
        tab: null,
        title: 'Shell',
        focused: true,
      };
      socket.write(
        encode({
          channel: 2,
          kind: KIND.response,
          payload: {
            reply: 'panes',
            with: {
              panes: inventories === 1 ? [pane, { ...pane, kind: 'native' }] : [pane],
              truncated: false,
            },
          },
        }),
      );
    },
    async (session, calls) => {
      const terminal = workspace(session).terminal;
      await assert.rejects(terminal.panes(), /duplicate pane slot identities/);
      assert.deepEqual(
        calls.map(({ call }) => call),
        ['pane_list'],
      );
      assert.deepEqual(await terminal.panes(), {
        panes: [
          {
            slot: 'pane-a',
            generation: 3,
            revision: 8,
            kind: 'terminal',
            provider: null,
            tab: null,
            title: 'Shell',
            focused: true,
          },
        ],
        truncated: false,
      });
      assert.deepEqual(
        calls.map(({ call }) => call),
        ['pane_list', 'pane_list'],
      );
    },
  );
});

test('real Unix tab inventory rejects duplicate identities and preserves session health', async () => {
  let inventories = 0;
  await withPaneIdentityHost(
    ['terminals:read'],
    (_request, socket) => {
      inventories += 1;
      const tab = { id: 'tab-a', title: 'Shell', pinned: false, panes: [] };
      socket.write(
        encode({
          channel: 2,
          kind: KIND.response,
          payload: {
            reply: 'tabs',
            with: inventories === 1 ? [tab, { ...tab, title: 'Replacement' }] : [tab],
          },
        }),
      );
    },
    async (session, calls) => {
      const terminal = workspace(session).terminal;
      await assert.rejects(terminal.tabs(), /duplicate tab identities/);
      assert.deepEqual(
        calls.map(({ call }) => call),
        ['terminal_tabs'],
      );
      assert.deepEqual(await terminal.tabs(), [
        { id: 'tab-a', title: 'Shell', pinned: false, panes: [] },
      ]);
      assert.deepEqual(
        calls.map(({ call }) => call),
        ['terminal_tabs', 'terminal_tabs'],
      );
    },
  );
});

test('real Unix topology rejects duplicate pane slots and preserves session health', async () => {
  let inventories = 0;
  await withPaneIdentityHost(
    ['terminals:read'],
    (_request, socket) => {
      inventories += 1;
      const leaf = (slot) => ({
        kind: 'pane',
        pane: {
          slot,
          working_directory: null,
          command: null,
          occupant: 'terminal',
          provider: null,
        },
        grid: { columns: 80, rows: 24 },
        focused: slot === 'pane-a',
      });
      const topology = {
        active_tab: 'tab-a',
        tabs: [
          {
            id: 'tab-a',
            title: 'Shell',
            pinned: false,
            root: {
              kind: 'split',
              division: 'beside',
              ratio_per_mille: 500,
              first: leaf('pane-a'),
              second: leaf(inventories === 1 ? 'pane-a' : 'pane-b'),
            },
          },
        ],
      };
      socket.write(
        encode({
          channel: 2,
          kind: KIND.response,
          payload: { reply: 'topology', with: topology },
        }),
      );
    },
    async (session, calls) => {
      const terminal = workspace(session).terminal;
      await assert.rejects(terminal.topology(), /duplicate pane slots in terminal topology/);
      assert.deepEqual(
        calls.map(({ call }) => call),
        ['terminal_topology'],
      );
      assert.equal((await terminal.topology()).tabs[0].id, 'tab-a');
      assert.deepEqual(
        calls.map(({ call }) => call),
        ['terminal_topology', 'terminal_topology'],
      );
    },
  );
});

test('real Unix terminal-to-text rejects same-slot replacement and preserves session health', async () => {
  let reads = 0;
  await withPaneIdentityHost(
    ['panes:observe', 'terminals:output'],
    (request, socket) => {
      if (request.call === 'terminal_read_pane') reads += 1;
      const payload =
        request.call === 'pane_list'
          ? {
              reply: 'panes',
              with: {
                panes: [
                  {
                    slot: 'pane-a',
                    generation: 3,
                    revision: 8,
                    kind: 'terminal',
                    provider: null,
                    tab: null,
                    title: 'Shell',
                    focused: true,
                  },
                ],
                truncated: false,
              },
            }
          : {
              reply: 'text',
              with: {
                slot: 'pane-a',
                generation: reads === 1 ? 4 : 5,
                revision: 1,
                columns: 80,
                rows: 24,
                lines: ['replacement'],
                cursor_column: 0,
                cursor_row: 0,
                truncated: false,
              },
            };
      socket.write(encode({ channel: 2, kind: KIND.response, payload }));
    },
    async (session, calls) => {
      await assert.rejects(
        workspace(session).terminal.toText('pane-a'),
        /pane pane-a changed during bounded text conversion/,
      );
      assert.deepEqual(
        calls.map(({ call }) => call),
        ['pane_list', 'terminal_read_pane'],
      );
      assert.equal((await workspace(session).terminal.read('pane-a')).generation, 5);
      assert.equal(calls.at(-1).call, 'terminal_read_pane');
    },
  );
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
          granted: [
            'containers:read',
            'containers:create',
            'containers:execute',
            'containers:lifecycle',
            'containers:remove',
          ],
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
          granted: [
            'containers:read',
            'containers:create',
            'containers:execute',
            'containers:lifecycle',
            'containers:remove',
          ],
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

test('real Unix stable pane inventory retries a layout race and keeps the session reusable', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-pane-snapshot-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  const calls = [];
  let lists = 0;
  const pane = (revision) => ({
    slot: 'shell', generation: 7, revision, kind: 'terminal', provider: null,
    tab: 'tab-1', title: 'Shell', focused: true,
  });
  const screen = (revision) => ({
    slot: 'shell', generation: 7, revision, columns: 80, rows: 24,
    lines: [`revision ${revision}`], cursor_column: 0, cursor_row: 1, truncated: false,
  });
  const fragmented = (socket, frame) => {
    socket.write(frame.subarray(0, 5));
    setImmediate(() => socket.write(frame.subarray(5)));
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        if (frame.payload.call === 'pane_list') {
          lists += 1;
          const revisions = [10, 11, 12, 13, 13, 13];
          fragmented(socket, encode({
            channel: 2, kind: KIND.response,
            payload: { reply: 'panes', with: { panes: [pane(revisions[lists - 1])], truncated: false } },
          }));
        } else if (frame.payload.call === 'terminal_read_pane') {
          assert.deepEqual(frame.payload.with, { slot: 'shell', lines: 40 });
          fragmented(socket, encode({
            channel: 2, kind: KIND.response,
            payload: { reply: 'text', with: screen(lists === 3 ? 12 : lists >= 5 ? 13 : 10) },
          }));
        }
      }
    });
    const greeting = encode({
      channel: CONTROL, kind: KIND.open,
      payload: { protocol: 1, peer: 'stable-pane-inventory', granted: ['panes:observe', 'terminals:output'] },
    });
    socket.write(greeting.subarray(0, 3));
    setImmediate(() => socket.write(greeting.subarray(3)));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const terminal = workspace(session).terminal;
    const cancelled = new AbortController();
    cancelled.abort('agent stopped');
    await assert.rejects(
      terminal.readAllStable({ signal: cancelled.signal }),
      (error) => error?.name === 'AbortError',
    );
    await assert.rejects(terminal.readAllStable({ attempts: 0 }), /within 1\.\.=16/);
    assert.deepEqual(calls, [], 'invalid and pre-cancelled snapshots send no frames');

    await assert.rejects(terminal.readAllStable({ lines: 40, attempts: 1 }), (error) => {
      assert(error instanceof PaneInventoryChangedError);
      assert.equal(error.attempts, 1);
      assert.equal(error.before[0].revision, 10);
      assert.equal(error.after[0].revision, 11);
      return true;
    });
    const snapshot = await terminal.readAllStable({ lines: 40, attempts: 2 });
    assert.equal(snapshot.complete, true);
    assert.equal(snapshot.panes[0].pane.revision, 13);
    assert.equal(snapshot.panes[0].readable.text, 'revision 13');
    assert.deepEqual(calls.map(({ call }) => call), [
      'pane_list', 'terminal_read_pane', 'pane_list',
      'pane_list', 'terminal_read_pane', 'pane_list',
      'pane_list', 'terminal_read_pane', 'pane_list',
    ]);
    assert.equal((await terminal.read('shell', 40)).revision, 13);
    assert.equal(calls.at(-1).call, 'terminal_read_pane', 'the ordered session remains reusable');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix semantic inventory discloses client projection truncation and preserves identity', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-semantic-projection-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  const calls = [];
  const node = (id) => ({
    id,
    role: 'button',
    label: 'label'.repeat(80),
    value: null,
    disabled: false,
    destructive: false,
    actions: ['invoke'],
    children: [],
  });
  const tree = {
    slot: 'surface-1',
    generation: 8,
    revision: 14,
    truncated: false,
    root: { ...node(0), children: Array.from({ length: 255 }, (_, index) => node(index + 1)) },
  };
  const pane = {
    slot: 'surface-1',
    generation: 8,
    revision: 14,
    kind: 'surface',
    provider: { extension: 'database', provider: 'browser' },
    tab: 'tab-4',
    title: 'Database',
    focused: true,
  };
  const fragmented = (socket, value) => {
    const frame = encode(value);
    socket.write(frame.subarray(0, 7));
    setImmediate(() => socket.write(frame.subarray(7)));
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload.call);
        const payload =
          frame.payload.call === 'pane_list'
            ? { reply: 'panes', with: { panes: [pane], truncated: false } }
            : { reply: 'semantics', with: tree };
        fragmented(socket, { channel: 2, kind: KIND.response, payload });
      }
    });
    fragmented(socket, {
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'semantic-projection',
        granted: ['panes:observe', 'panes:semantic-read'],
      },
    });
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const terminal = workspace(session).terminal;
    const cancelled = new AbortController();
    cancelled.abort('agent stopped');
    await assert.rejects(
      terminal.readAllStable({ signal: cancelled.signal }),
      (error) => error?.name === 'AbortError',
    );
    assert.deepEqual(calls, [], 'pre-cancelled semantic inspection emits no request');

    const inventory = await terminal.readAllStable({ attempts: 2 });
    const [{ pane: identity, readable }] = inventory.panes;
    assert.deepEqual(identity.provider, { extension: 'database', provider: 'browser' });
    assert.equal(readable.kind, 'ui');
    assert.equal(readable.complete, false);
    assert.equal(readable.sourceTruncated, false);
    assert.equal(readable.projectionTruncated, true);
    assert.match(readable.text, /<truncated\/>/);
    assert.deepEqual(calls, ['pane_list', 'pane_semantic_read', 'pane_list']);
    assert.equal((await terminal.semantics('surface-1')).revision, 14);
    assert.equal(calls.at(-1), 'pane_semantic_read', 'the ordered session remains reusable');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix final JSON record remains cancellable after execution EOF', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-final-json-cancel-'));
  const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64);
  const executionId = 'e'.repeat(32);
  const calls = [];
  const connections = new Set();
  const execution = {
    id: executionId,
    container_id: containerId,
    running: false,
    exit_code: 0,
    pid: 42,
    command: ['test', '--jsonl'],
    user: 'runner',
  };
  const fragmented = (socket, payload) => {
    const frame = encode({ channel: 2, kind: KIND.response, payload });
    socket.write(frame.subarray(0, 5));
    setImmediate(() => socket.write(frame.subarray(5)));
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload.call);
        const payload =
          frame.payload.call === 'container_exec'
            ? { reply: 'identity', with: executionId }
            : frame.payload.call === 'execution_output'
              ? {
                  reply: 'execution_output',
                  with: {
                    entries: [{
                      sequence: 1,
                      timestamp_ms: 1,
                      stream: 'stdout',
                      bytes: [...Buffer.from('{"case":1}')],
                    }],
                    next: 1,
                    more: false,
                    eof: true,
                    gap: false,
                  },
                }
              : frame.payload.call === 'execution_inspect'
                ? { reply: 'execution', with: execution }
                : {
                    reply: 'workspace',
                    with: { name: 'tests', image: 'toolbox', architecture: 'amd64' },
                  };
        fragmented(socket, payload);
      }
    });
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'final-json-cancel',
        granted: ['containers:read', 'containers:execute', 'workspaces:read'],
      },
    });
    socket.write(greeting.subarray(0, 3));
    setImmediate(() => socket.write(greeting.subarray(3)));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const abort = new AbortController();
    let delivered = false;
    const pending = workspace(session).containers.execJsonLines(
      containerId,
      3,
      { command: ['test', '--jsonl'], maxLineBytes: 4096, signal: abort.signal },
      () => {
        delivered = true;
        return new Promise(() => {});
      },
    );
    while (!delivered) await new Promise((resolve) => setImmediate(resolve));
    abort.abort('test run superseded');
    await assert.rejects(pending, (error) => {
      assert(error instanceof ExecutionOperationError);
      assert.equal(error.executionId, executionId);
      assert.equal(error.phase, 'output');
      assert.deepEqual(error.execution, execution);
      assert.equal(error.cause?.name, 'AbortError');
      return true;
    });
    assert.deepEqual(calls, ['container_exec', 'execution_output', 'execution_inspect']);
    assert.equal((await workspace(session).info()).name, 'tests');
    assert.equal(calls.at(-1), 'workspace_info', 'the ordered session remains reusable');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix text EOF decoding preserves completed execution identity and session reuse', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-text-eof-'));
  const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64);
  const executionId = 'e'.repeat(32);
  const calls = [];
  const connections = new Set();
  const execution = {
    id: executionId, container_id: containerId, running: false, exit_code: 0,
    pid: 42, command: ['psql'], user: 'postgres',
  };
  const fragmented = (socket, payload) => {
    const frame = encode({ channel: 2, kind: KIND.response, payload });
    socket.write(frame.subarray(0, 5));
    setImmediate(() => socket.write(frame.subarray(5)));
  };
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload.call);
        const payload = frame.payload.call === 'container_exec'
          ? { reply: 'identity', with: executionId }
          : frame.payload.call === 'execution_output'
            ? { reply: 'execution_output', with: {
                entries: [{ sequence: 1, timestamp_ms: 1, stream: 'stdout', bytes: [0xc3] }],
                next: 1, more: false, eof: true, gap: false,
              } }
            : frame.payload.call === 'execution_inspect'
              ? { reply: 'execution', with: execution }
              : { reply: 'workspace', with: {
                  name: 'database', image: 'postgres:17', architecture: 'amd64',
                } };
        fragmented(socket, payload);
      }
    });
    const greeting = encode({ channel: CONTROL, kind: KIND.open, payload: {
      protocol: 1, peer: 'text-eof-recovery',
      granted: ['containers:read', 'containers:execute', 'workspaces:read'],
    } });
    socket.write(greeting.subarray(0, 3));
    setImmediate(() => socket.write(greeting.subarray(3)));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    await assert.rejects(
      workspace(session).containers.execText(containerId, 3, {
        command: ['psql'], maxBytes: 4096,
      }),
      (error) => {
        assert(error instanceof ExecutionOperationError);
        assert.equal(error.executionId, executionId);
        assert.equal(error.phase, 'output');
        assert.deepEqual(error.execution, execution);
        assert(error.cause instanceof TypeError);
        return true;
      },
    );
    assert.deepEqual(calls, ['container_exec', 'execution_output', 'execution_inspect']);
    assert.equal((await workspace(session).info()).name, 'database');
    assert.equal(calls.at(-1), 'workspace_info', 'the ordered session remains reusable');
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
          granted: [
            'containers:read',
            'containers:create',
            'containers:execute',
            'containers:lifecycle',
            'containers:remove',
          ],
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
