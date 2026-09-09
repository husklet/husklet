import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';
import { connect, workspace } from '../dist/index.js';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

test('Postgres GUI uses bounded observation, opaque credentials, pane text, and one exact file grant', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-postgres-gui-'));
  const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64);
  const executionId = 'e'.repeat(32);
  const hostOnlyPassword = 'sentinel-plaintext-password';
  const settingsPath = '.husklet/postgres/connections.json';
  const calls = [];
  const connections = new Set();
  const allowedWrites = new Set([settingsPath]);
  let outputPage = 0;

  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        const { call, with: value = {} } = frame.payload;
        let payload;
        if (call === 'container_list') {
          payload = {
            reply: 'containers',
            with: [
              {
                id: containerId,
                name: 'postgres',
                image: 'postgres:17',
                state: 'running',
                created: 1,
                generation: 7,
              },
            ],
          };
        } else if (call === 'container_processes') {
          const first = value.after === 0;
          payload =
            value.snapshot && value.snapshot !== 'a'.repeat(64)
              ? {
                  error: 'conflict',
                  detail: 'container process snapshot changed; restart pagination',
                }
              : {
                  reply: 'processes',
                  with: {
                    container_id: containerId,
                    titles: ['PID', 'USER', 'COMMAND'],
                    processes: first
                      ? [['1', 'postgres', 'postgres -D /var/lib/postgresql/data']]
                      : [['8', 'postgres', 'postgres: app app 10.0.0.2 idle']],
                    snapshot: 'a'.repeat(64),
                    next: first ? 1 : null,
                    more: first,
                    observed_at_ms: 42,
                    scope: 'namespace',
                    pid_identity: 'snapshot',
                    truncated: false,
                  },
                };
        } else if (call === 'container_exec_credential') {
          assert.equal(value.credentials[0][1], 'postgres.password');
          assert.equal(hostOnlyPassword.length > 0, true, 'fixture host owns the credential value');
          payload = { reply: 'identity', with: executionId };
        } else if (call === 'execution_output') {
          outputPage += 1;
          payload = {
            reply: 'execution_output',
            with:
              outputPage === 1
                ? {
                    entries: [
                      {
                        sequence: 0,
                        timestamp_ms: 50,
                        stream: 'stdout',
                        bytes: [...Buffer.from('{"database":"app"}\n')],
                      },
                    ],
                    next: 1,
                    more: true,
                    eof: false,
                    gap: false,
                  }
                : { entries: [], next: 1, more: false, eof: true, gap: false },
          };
        } else if (call === 'execution_inspect') {
          payload = {
            reply: 'execution',
            with: {
              id: executionId,
              container_id: containerId,
              running: false,
              exit_code: 0,
              pid: 19,
              command: ['psql'],
              user: 'postgres',
            },
          };
        } else if (call === 'pane_list') {
          payload = {
            reply: 'panes',
            with: {
              panes: [
                {
                  slot: 'database-shell',
                  generation: 3,
                  revision: 9,
                  kind: 'terminal',
                  provider: null,
                  tab: 'database',
                  title: 'Postgres shell',
                  focused: true,
                },
              ],
              truncated: false,
            },
          };
        } else if (call === 'terminal_read_pane') {
          payload = {
            reply: 'text',
            with: {
              slot: 'database-shell',
              generation: 3,
              revision: 9,
              columns: 100,
              rows: 30,
              lines: ['app=# select current_database();', ' current_database', ' app'],
              cursor_column: 5,
              cursor_row: 2,
              truncated: false,
            },
          };
        } else if (call === 'filesystem_write_observed') {
          payload = allowedWrites.has(value.path)
            ? { reply: 'identity', with: 'v1:2:3:4:5:6:7:9' }
            : {
                error: 'denied',
                capability: 'filesystem:write',
                detail: `path is outside the reviewed exact selector: ${value.path}`,
              };
        } else {
          payload = { error: 'unsupported', detail: `unexpected Postgres workflow call: ${call}` };
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
          extension: 'postgres-gui',
          granted: [
            'containers:read',
            'containers:execute',
            'credentials:inject',
            'panes:observe',
            'terminals:output',
            'filesystem:write',
          ],
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));

  try {
    const session = await connect({ path: socketPath });
    const host = workspace(session);
    const [container] = await host.containers.list();
    const processes = [];
    for await (const page of host.containers.processPages(container.id, { limit: 1 })) {
      processes.push(...page.processes);
    }
    assert.equal(processes.length, 2, 'the GUI retrieves the remainder of one immutable snapshot');
    await assert.rejects(
      host.containers.processes(container.id, {
        snapshot: 'b'.repeat(64),
        after: 1,
        limit: 1,
      }),
      /snapshot changed; restart pagination/,
    );
    const beforeMalformed = calls.length;
    await assert.rejects(
      host.containers.processes(container.id, { snapshot: 'malformed', after: 1, limit: 1 }),
      /64 hexadecimal characters/,
    );
    await assert.rejects(
      host.containers.processes(container.id, { after: 1, limit: 1 }),
      /requires its snapshot identity/,
    );
    assert.equal(calls.length, beforeMalformed, 'malformed cursors never reach the Unix socket');

    const rows = [];
    await host.containers.execJsonLines(
      container.id,
      container.generation,
      {
        command: ['psql', '--no-psqlrc', '--tuples-only'],
        credentials: [['PGPASSWORD', 'postgres.password']],
        maxLineBytes: 4096,
        pageLimit: 1,
      },
      (row) => rows.push(row),
    );
    assert.deepEqual(rows, [{ database: 'app' }]);

    const terminal = await host.terminal.toText('database-shell', { lines: 30 });
    assert.equal(terminal.kind, 'terminal');
    assert.match(terminal.text, /current_database/);

    const settings = Buffer.from('{"container":"postgres","database":"app"}\n');
    assert.equal(
      await host.files.writeObserved(settingsPath, 'v1:2:3:4:5:6:7:8', settings),
      'v1:2:3:4:5:6:7:9',
    );
    await assert.rejects(
      host.files.writeObserved('.env', 'v1:2:3:4:5:6:7:8', Buffer.from('PGPASSWORD=stolen')),
      /outside the reviewed exact selector/,
    );
    await assert.rejects(host.credentials.read('postgres.password'), /credentials:read/);

    const credentialCall = calls.find(({ call }) => call === 'container_exec_credential');
    assert.deepEqual(credentialCall.with.credentials, [['PGPASSWORD', 'postgres.password']]);
    assert.equal(JSON.stringify(calls).includes('sentinel-plaintext-password'), false);
    assert.equal(
      calls.some(({ call }) => call === 'credential_read'),
      false,
      'plaintext credential reads are rejected before reaching the socket',
    );
    assert.deepEqual(
      calls.filter(({ call }) => call === 'execution_output').map(({ with: value }) => value.limit),
      [1, 1],
      'output is fetched through bounded pages',
    );
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
