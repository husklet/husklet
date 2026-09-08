import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

const examples = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../examples');
const reactExamples = path.resolve(examples, '../../react/examples');
const capabilities = [
  'panes:observe',
  'terminals:output',
  'terminals:control',
  'filesystem:read',
  'filesystem:write',
  'containers:read',
  'containers:control',
  'networks:read',
  'interface:render',
];

async function scenario(name, configuration, reply) {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-product-scenario-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const peers = new Set();
  const server = net.createServer((socket) => {
    peers.add(socket);
    socket.on('close', () => peers.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        reply(socket, frame);
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, extension: 'product-scenario', granted: capabilities },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  const example =
    name === 'postgres-inspector.ts' ? path.join(reactExamples, name) : path.join(examples, name);
  const child = spawn(
    process.execPath,
    ['--experimental-strip-types', example, JSON.stringify({ path: socketPath, ...configuration })],
    { cwd: path.resolve(examples, '../../..'), stdio: ['ignore', 'pipe', 'pipe'] },
  );
  let stdout = '';
  let stderr = '';
  child.stdout.setEncoding('utf8');
  child.stderr.setEncoding('utf8');
  child.stdout.on('data', (part) => {
    stdout += part;
  });
  child.stderr.on('data', (part) => {
    stderr += part;
  });
  let timeout;
  try {
    const code = await Promise.race([
      new Promise((resolve) => child.once('exit', resolve)),
      new Promise((_, reject) => {
        timeout = setTimeout(() => reject(new Error('scenario timed out')), 5_000);
      }),
    ]);
    assert.equal(code, 0, stderr);
    return { result: JSON.parse(stdout), calls };
  } finally {
    clearTimeout(timeout);
    if (child.exitCode === null) child.kill('SIGKILL');
    for (const peer of peers) peer.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
}

function respond(socket, frame, payload) {
  socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
}

test('LLM terminal agent observes, writes, and waits over the extension socket', async () => {
  let reads = 0;
  const pane = {
    slot: 'term',
    generation: 4,
    revision: 8,
    kind: 'terminal',
    provider: null,
    tab: 'tab',
    title: 'Shell',
    focused: true,
  };
  const run = await scenario(
    'llm-terminal-agent.ts',
    { slot: 'term', prompt: 'explain status' },
    (socket, frame) => {
      const call = frame.payload.call;
      if (call === 'pane_list')
        respond(socket, frame, { reply: 'panes', with: { panes: [pane], truncated: false } });
      else if (call === 'event_subscribe' || call === 'event_unsubscribe')
        respond(socket, frame, { reply: 'done' });
      else if (call === 'terminal_read_pane') {
        reads += 1;
        const revision = reads < 3 ? 8 : 9;
        respond(socket, frame, {
          reply: 'text',
          with: {
            slot: 'term',
            generation: 4,
            revision,
            columns: 80,
            rows: 24,
            lines: [revision === 8 ? '$ ' : '$ explain status', revision === 8 ? '' : 'healthy'],
            cursor_column: 0,
            cursor_row: 1,
            truncated: false,
          },
        });
      } else if (call === 'terminal_write_pane') {
        assert.deepEqual(frame.payload.with, {
          slot: 'term',
          generation: 4,
          revision: 8,
          contents: Array.from(new TextEncoder().encode('explain status\n')),
        });
        socket.write(
          encode({
            channel: 40,
            kind: KIND.event,
            payload: {
              snapshot: 'pane_changes',
              of: { slot: 'term', kind: 'terminal', generation: 4, revision: 9, coalesced: 0 },
            },
          }),
        );
        respond(socket, frame, { reply: 'done' });
      }
    },
  );
  assert.equal(run.result.before, '$ \n');
  assert.match(run.result.after, /healthy/);
});

test('embeddings indexer reconciles, recursively discovers, streams, and CAS-updates', async () => {
  const document = new TextEncoder().encode('alpha beta gamma');
  const run = await scenario(
    'embeddings-indexer.ts',
    { root: 'src', document: 'src/a.md', index: '.husklet/index.json', chunkBytes: 6 },
    (socket, frame) => {
      const { call } = frame.payload;
      if (call === 'filesystem_inventory')
        respond(socket, frame, {
          reply: 'file_inventory',
          with: {
            entries: [
              { path: 'src/a.md', directory: false, size: document.length, identity: 'doc-v1' },
            ],
            complete: true,
            coalesced: 0,
          },
        });
      else if (call === 'filesystem_list_page')
        respond(socket, frame, {
          reply: 'directory_page',
          with: {
            entries: [
              {
                path: 'src/a.md',
                directory: false,
                size: document.length,
                identity: 'doc-v1',
              },
            ],
            identity: 'src-v1',
            next: 'src/a.md',
            more: false,
          },
        });
      else if (call === 'filesystem_read_range') {
        const offset = frame.payload.with.offset;
        const contents = Array.from(document.slice(offset, offset + 6));
        respond(socket, frame, {
          reply: 'file_range',
          with: {
            path: 'src/a.md',
            identity: 'doc-v1',
            offset,
            total: document.length,
            contents,
            eof: offset + contents.length === document.length,
            truncated: offset + contents.length !== document.length,
          },
        });
      } else if (call === 'filesystem_stat')
        respond(socket, frame, {
          reply: 'entry',
          with: { path: '.husklet/index.json', directory: false, size: 2, identity: 'index-v1' },
        });
      else if (call === 'filesystem_write_observed') {
        assert.equal(frame.payload.with.observed, 'index-v1');
        assert.match(
          new TextDecoder().decode(Uint8Array.from(frame.payload.with.contents)),
          /"identity":"doc-v1"/,
        );
        respond(socket, frame, { reply: 'identity', with: 'index-v2' });
      }
    },
  );
  assert.equal(run.result.bytes, document.length);
  assert.equal(run.result.indexIdentity, 'index-v2');
  const ranges = run.calls.filter(({ call }) => call === 'filesystem_read_range');
  assert.deepEqual(
    ranges.map(({ with: value }) => [value.offset, value.observed]),
    [
      [0, null],
      [6, 'doc-v1'],
      [12, 'doc-v1'],
    ],
  );
});

test('Postgres GUI stays live and serves a scrolled database window before host shutdown', async () => {
  const id = 'c'.repeat(64);
  const password = 'sentinel-password-never-in-replies';
  const executionId = 'e'.repeat(32);
  let outputCalls = 0;
  const run = await scenario('postgres-inspector.ts', {
    container: id,
    credentialPath: 'secrets/postgres.password',
    query: 'select id,name from widgets',
  }, (socket, frame) => {
    const { call } = frame.payload;
    if (call === 'container_list')
      respond(socket, frame, { reply: 'containers', with: [{
        id, name: 'postgres', image: 'postgres:18', state: 'running', created: 1, generation: 2,
      }] });
    else if (call === 'container_inspect')
      respond(socket, frame, {
        reply: 'container',
        with: {
          id,
          name: 'postgres',
          image: 'postgres:18',
          state: 'running',
          created: 1,
          generation: 2,
        },
      });
    else if (call === 'container_processes')
      respond(socket, frame, {
        reply: 'processes',
        with: {
          container_id: id,
          titles: ['PID', 'COMMAND'],
          processes: [
            ['1', 'postgres'],
            ['7', 'walwriter'],
          ],
          observed_at_ms: 1,
          scope: 'namespace',
          pid_identity: 'snapshot',
          truncated: false,
        },
      });
    else if (call === 'container_logs')
      respond(socket, frame, {
        reply: 'logs',
        with: {
          stdout: [114, 101, 97, 100, 121],
          stderr: [],
          truncated: false,
          stdout_truncated: false,
          stderr_truncated: false,
          eof: true,
        },
      });
    else if (call === 'network_list')
      respond(socket, frame, {
        reply: 'networks',
        with: {
          networks: [{ id: 'n1', name: 'backend', driver: 'bridge', scope: 'local', kind: 'custom' }],
          truncated: false,
        },
      });
    else if (call === 'filesystem_read')
      respond(socket, frame, { reply: 'contents', with: [...Buffer.from(`${password}\n`)] });
    else if (call === 'container_exec')
      respond(socket, frame, { reply: 'identity', with: executionId });
    else if (call === 'execution_output') {
      outputCalls += 1;
      const bytes = outputCalls === 1 ? Buffer.from('rows\n2\n') : Buffer.from('id,name\n1,alpha\n2,beta\n');
      respond(socket, frame, { reply: 'execution_output', with: {
        entries: [{ sequence: 1, timestamp_ms: 1, stream: 'stdout', bytes: [...bytes] }],
        next: 1, more: false, eof: true, gap: false,
      } });
    } else if (call === 'execution_inspect')
      respond(socket, frame, {
        reply: 'execution',
        with: {
          id: executionId, container_id: id, running: false, exit_code: 0, pid: 42,
          command: ['psql', '--csv'], user: 'postgres',
        },
      });
    else if (call === 'interface_open_tab')
      respond(socket, frame, { reply: 'identity', with: 'postgres-pane' });
    else if (call === 'source_resize_at') {
      respond(socket, frame, { reply: 'done' });
      if (frame.payload.with.mutation.Length) {
        socket.write(encode({
          channel: 17,
          kind: KIND.event,
          payload: {
            id: 41,
            slot: 'postgres-pane',
            source: 1,
            version: 1,
            range: { start: 1, count: 1 },
            sort: null,
            filter: null,
          },
        }));
      } else if (frame.payload.with.mutation.Window) {
        socket.end();
      }
    } else respond(socket, frame, { reply: 'done' });
  });
  assert.deepEqual(run.result, {
    container: id,
    processes: 2,
    queryRows: 2,
    networks: 1,
    slot: 'postgres-pane',
  });
  const rendered = run.calls.find(({ call }) => call === 'interface_render_at');
  assert(rendered, 'React tree was not rendered');
  const mutations = run.calls
    .filter(({ call }) => call === 'source_resize_at')
    .map(({ with: value }) => value.mutation);
  assert.equal(mutations.length, 3);
  assert.equal(mutations[1].Length.rows, 2);
  assert.deepEqual(mutations[2].Window, {
    source: 1,
    version: 1,
    request: 41,
    range: { start: 1, count: 1 },
    rows: [{ key: 1, cells: [{ Text: '1' }, { Text: 'alpha' }] }],
  });
  const execs = run.calls.filter(({ call }) => call === 'container_exec');
  assert.equal(execs.length, 3);
  assert.deepEqual(execs[1].with.command.slice(0, 4), ['psql', '--csv', '--no-psqlrc', '--command']);
  assert.match(execs[1].with.command[4], /LIMIT 128 OFFSET 0$/);
  assert.match(execs[2].with.command[4], /LIMIT 1 OFFSET 1$/);
  assert.deepEqual(execs[1].with.environment, [['PGPASSWORD', password]]);
  for (const call of run.calls.filter(({ call }) => call !== 'container_exec'))
    assert(!JSON.stringify(call).includes(password), `credential leaked through ${call.call}`);
});
