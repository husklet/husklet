import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';
import { connect, workspace } from '../dist/index.js';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

test('Postgres browser streams credential-backed rows over real Unix framing', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-postgres-browser-'));
  const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64);
  const networkId = 'a'.repeat(32);
  const executionId = 'e'.repeat(32);
  const requests = [];
  const connections = new Set();
  let outputPage = 0;
  let outputRequested = false;
  const pendingInput = [];
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    let writes = Promise.resolve();
    const writeFragmented = (frame) => {
      const bytes = encode(frame);
      writes = writes.then(
        () =>
          new Promise((resolve, reject) => {
            socket.write(bytes.subarray(0, 3), (error) => {
              if (error) reject(error);
              else setImmediate(() => socket.write(bytes.subarray(3), resolve));
            });
          }),
      );
    };
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        requests.push(frame.payload);
        let payload;
        if (frame.payload.call === 'network_inspect') {
          payload = {
            reply: 'network',
            with: {
              id: networkId,
              name: 'database',
              driver: 'bridge',
              scope: 'local',
              kind: 'custom',
              endpoints: { containers: [], truncated: false },
            },
          };
        } else if (frame.payload.call === 'container_exec_credential') {
          payload = { reply: 'identity', with: executionId };
        } else if (frame.payload.call === 'execution_write' && !outputRequested) {
          // Model a bidirectional process whose stdin writer cannot advance until
          // the client drains output. Sending all input first deadlocks here.
          pendingInput.push(frame);
          continue;
        } else if (frame.payload.call === 'execution_output') {
          outputRequested = true;
          outputPage += 1;
          payload = {
            reply: 'execution_output',
            with:
              outputPage === 1
                ? {
                    entries: [
                      {
                        sequence: 1,
                        timestamp_ms: 1,
                        stream: 'stdout',
                        bytes: [...Buffer.from('{"id":1')],
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
                        bytes: [...Buffer.from('}\n{"id":2}\n')],
                      },
                    ],
                    next: 2,
                    more: false,
                    eof: true,
                    gap: false,
                  },
          };
        } else if (frame.payload.call === 'execution_inspect') {
          payload = {
            reply: 'execution',
            with: {
              id: executionId,
              container_id: containerId,
              running: false,
              exit_code: 0,
              pid: 41,
              command: ['psql'],
              user: 'postgres',
            },
          };
        } else {
          payload = { reply: 'done' };
        }
        writeFragmented({ channel: frame.channel, kind: KIND.response, payload });
        while (pendingInput.length > 0) {
          const input = pendingInput.shift();
          writeFragmented({
            channel: input.channel,
            kind: KIND.response,
            payload: { reply: 'done' },
          });
        }
      }
    });
    writeFragmented({
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'postgres-fixture',
        granted: [
          'containers:read',
          'containers:execute',
          'containers:input',
          'credentials:inject',
          'networks:read',
          'networks:connect',
          'networks:disconnect',
        ],
      },
    });
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const rows = [];
    let liveExecution;
    let deadlockTimer;
    const result = await Promise.race([
      workspace(session).networks.withTemporaryConnection(
        networkId,
        containerId,
        () => workspace(session).containers.execJsonLines(
          containerId,
          7,
          {
            command: ['psql', '--command', 'select 1'],
            environment: [['PGDATABASE', 'app']],
            credentials: [['PGPASSWORD', 'postgres.password']],
            input: ['select row_to_json(query) from (select 1 as id) query;\n'],
            pageLimit: 1,
            maxLineBytes: 1024,
            onStarted: async (id) => {
              liveExecution = await workspace(session).containers.execution(id);
            },
          },
          async (value) => {
            rows.push(value);
            await Promise.resolve();
          },
        ),
        { aliases: ['postgres-inspector'] },
      )
        .finally(() => clearTimeout(deadlockTimer)),
      new Promise(
        (_, reject) =>
          (deadlockTimer = setTimeout(
            () => reject(new Error('bidirectional execution deadlocked')),
            1_000,
          )),
      ),
    ]);
    assert.deepEqual(rows, [{ id: 1 }, { id: 2 }]);
    assert.equal(result.lines, 2);
    assert.equal(result.executionId, executionId);
    assert.equal(result.execution.exit_code, 0);
    assert.equal(liveExecution.id, executionId);
    assert.deepEqual(requests.slice(0, 4), [
      { call: 'network_inspect', with: { reference: networkId } },
      {
        call: 'network_connect',
        with: { reference: networkId, container: containerId, aliases: ['postgres-inspector'] },
      },
      {
        call: 'container_exec_credential',
        with: {
          id: containerId,
          generation: 7,
          command: ['psql', '--command', 'select 1'],
          environment: [['PGDATABASE', 'app']],
          credentials: [['PGPASSWORD', 'postgres.password']],
          user: null,
          working_directory: null,
          stdin: true,
        },
      },
      { call: 'execution_inspect', with: { id: executionId } },
    ]);
    assert.deepEqual(
      requests.slice(4).map((request) => request.call),
      [
        'execution_output',
        'execution_write',
        'execution_output',
        'execution_close_input',
        'execution_inspect',
        'network_disconnect',
      ],
    );
    assert.deepEqual(
      requests.find((request) => request.call === 'execution_write'),
      {
        call: 'execution_write',
        with: {
          id: executionId,
          contents: [...Buffer.from('select row_to_json(query) from (select 1 as id) query;\n')],
        },
      },
    );
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('Postgres output cancellation interrupts a stalled Unix call and reconnects from the acknowledged cursor', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-postgres-cancel-'));
  const socketPath = path.join(directory, 'host.sock');
  const executionId = 'e'.repeat(32);
  const connections = new Set();
  let accepted = 0;
  let stalledRequests = 0;
  let observeStalledRequest;
  const stalledRequest = new Promise((resolve) => {
    observeStalledRequest = resolve;
  });
  const server = net.createServer((socket) => {
    const connection = ++accepted;
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        if (connection === 1 && frame.payload.call === 'execution_output') {
          stalledRequests += 1;
          observeStalledRequest();
          continue;
        }
        const response = encode({
          channel: frame.channel,
          kind: KIND.response,
          payload: {
            reply: 'workspace',
            with: { name: 'database', image: 'postgres:17', architecture: 'amd64' },
          },
        });
        socket.write(response.subarray(0, 4));
        socket.write(response.subarray(4));
      }
    });
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: `postgres-cancel-${connection}`,
        granted: ['containers:read', 'workspaces:read'],
      },
    });
    socket.write(greeting.subarray(0, 2));
    socket.write(greeting.subarray(2));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath, timeout: 5_000 });
    const abort = new AbortController();
    const pages = workspace(session).containers.executionOutputPages(executionId, {
      after: 17,
      signal: abort.signal,
    });
    const pending = pages.next();
    await stalledRequest;
    abort.abort('query view closed');
    await assert.rejects(
      pending,
      (error) => error?.name === 'AbortError' && error.cause === abort.signal.reason,
    );
    assert.equal(stalledRequests, 1);
    await assert.rejects(workspace(session).info(), /closed|query view closed/);

    const resumed = await connect({ path: socketPath });
    assert.equal((await workspace(resumed).info()).name, 'database');
    assert.equal(accepted, 2);
    await resumed.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
