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
  const executionId = 'e'.repeat(32);
  const requests = [];
  const connections = new Set();
  let outputPage = 0;
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
        if (frame.payload.call === 'container_exec_credential') {
          payload = { reply: 'identity', with: executionId };
        } else if (frame.payload.call === 'execution_output') {
          outputPage += 1;
          payload = {
            reply: 'execution_output',
            with:
              outputPage === 1
                ? {
                    entries: [
                      {
                        sequence: 0,
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
                        sequence: 1,
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
      }
    });
    writeFragmented({
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'postgres-fixture',
        granted: ['containers:read', 'containers:execute', 'containers:input', 'credentials:inject'],
      },
    });
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const rows = [];
    let liveExecution;
    const result = await workspace(session).containers.execJsonLines(
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
    );
    assert.deepEqual(rows, [{ id: 1 }, { id: 2 }]);
    assert.equal(result.lines, 2);
    assert.equal(result.executionId, executionId);
    assert.equal(result.execution.exit_code, 0);
    assert.equal(liveExecution.id, executionId);
    assert.deepEqual(requests, [
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
      {
        call: 'execution_write',
        with: {
          id: executionId,
          contents: [...Buffer.from('select row_to_json(query) from (select 1 as id) query;\n')],
        },
      },
      { call: 'execution_close_input', with: { id: executionId } },
      { call: 'execution_output', with: { id: executionId, after: 0, limit: 1 } },
      { call: 'execution_output', with: { id: executionId, after: 1, limit: 1 } },
      { call: 'execution_inspect', with: { id: executionId } },
    ]);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
