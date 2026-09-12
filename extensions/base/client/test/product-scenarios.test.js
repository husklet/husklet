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
const FILE_JOURNAL = '0123456789abcdef0123456789abcdef';
const capabilities = [
  'panes:observe',
  'panes:semantic-read',
  'terminals:output',
  'terminals:input',
  'terminals:layout-control',
  'terminals:process-control',
  'filesystem:read',
  'filesystem:write',
  'state:read',
  'state:write',
  'containers:read',
  'containers:create',
  'containers:execute',
  'containers:input',
  'credentials:inject',
  'containers:lifecycle',
  'containers:remove',
  'networks:read',
  'interface:render',
];

async function scenario(
  name,
  configuration,
  reply,
  expectedCode = 0,
  granted = capabilities,
  onResponse,
) {
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
        if (frame.kind === KIND.response) {
          if (onResponse) onResponse(socket, frame);
          continue;
        }
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        reply(socket, frame);
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: {
          protocol: 1,
          extension: 'product-scenario',
          granted,
          filesystem: {
            read: granted.includes('filesystem:read') ? [{ subtree: 'src' }] : [],
            write: granted.includes('filesystem:write') ? [{ subtree: 'src' }] : [],
            create: [],
            delete: [],
            rename: [],
          },
        },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  const example =
    name === 'postgres-inspector.ts' ||
    name === 'embeddings-workbench.ts' ||
    name === 'git-review-workbench.ts'
      ? path.join(reactExamples, name)
      : path.join(examples, name);
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
    assert.equal(code, expectedCode, stderr);
    return { result: stdout ? JSON.parse(stdout) : null, calls, stderr };
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

test('LLM terminal agent runs a supervised command without parsing a prompt', async () => {
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
  const uiPane = {
    slot: 'dashboard',
    generation: 2,
    revision: 3,
    kind: 'surface',
    provider: null,
    tab: 'tab',
    title: 'Dashboard',
    focused: false,
  };
  const run = await scenario(
    'llm-terminal-agent.ts',
    { slot: 'term', prompt: 'explain status' },
    (socket, frame) => {
      const call = frame.payload.call;
      if (call === 'pane_list')
        respond(socket, frame, {
          reply: 'panes',
          with: {
            panes: [pane, uiPane],
            truncated: false,
          },
        });
      else if (call === 'event_subscribe' || call === 'event_unsubscribe')
        respond(socket, frame, { reply: 'done' });
      else if (call === 'terminal_read_pane') {
        respond(socket, frame, {
          reply: 'text',
          with: {
            slot: 'term',
            generation: 4,
            revision: 8,
            columns: 80,
            rows: 24,
            lines: ['$ ', ''],
            cursor_column: 0,
            cursor_row: 1,
            truncated: false,
          },
        });
      } else if (call === 'pane_semantic_read') {
        const target = frame.payload.with.slot;
        respond(socket, frame, {
          reply: 'semantics',
          with: {
            slot: target,
            generation: target === 'term' ? 5 : 2,
            revision: target === 'term' ? 1 : 3,
            root: {
              id: 0,
              role: 'group',
              label: target === 'term' ? 'Agent result healthy' : 'Deployment healthy',
              value: null,
              disabled: false,
              destructive: false,
              actions: [],
              children: [],
            },
            truncated: false,
          },
        });
      } else if (call === 'terminal_command_start') {
        assert.deepEqual(frame.payload.with, {
          slot: 'term',
          generation: 4,
          revision: 8,
          command: ['sh', '-lc', 'explain status'],
          stdin: false,
        });
        respond(socket, frame, {
          reply: 'terminal_command',
          with: { id: 'e'.repeat(32), slot: 'term', generation: 4, revision: 8, running: true, exit_code: 0, pid: 19, command: ['sh', '-lc', 'explain status'] },
        });
      } else if (call === 'terminal_command_output') {
        respond(socket, frame, {
          reply: 'terminal_command_output',
          with: { id: 'e'.repeat(32), slot: 'term', generation: 4, revision: 8, output: { entries: [{ sequence: 1, timestamp_ms: 1, stream: 'stdout', bytes: Array.from(new TextEncoder().encode('healthy\n')) }], next: 1, more: false, eof: true, gap: false } },
        });
      } else if (call === 'terminal_command_wait') {
        respond(socket, frame, {
          reply: 'terminal_command',
          with: { id: 'e'.repeat(32), slot: 'term', generation: 4, revision: 8, running: false, exit_code: 17, pid: 0, command: ['sh', '-lc', 'explain status'] },
        });
      }
    },
  );
  assert.equal(run.result.selected.kind, 'terminal');
  assert.equal(run.result.selected.before, '$ \n');
  assert.equal(run.result.selected.stdout, 'healthy\n');
  assert.equal(run.result.selected.stderr, '');
  assert.equal(run.result.selected.exitCode, 17);
  assert.equal(run.result.selected.completed, true);
  assert.equal(run.result.selected.command, 'e'.repeat(32));
  assert.deepEqual(run.result.selected.pane, { slot: 'term', generation: 4, revision: 8 });
  assert.equal(run.result.incomplete, false);
  assert.match(run.result.context.find(({ kind }) => kind === 'ui').text, /Deployment healthy/);
});

test('LLM terminal agent reads a selected semantic surface without terminal input', async () => {
  const pane = {
    slot: 'dashboard',
    generation: 2,
    revision: 3,
    kind: 'surface',
    provider: null,
    tab: 'tab',
    title: 'Dashboard',
    focused: true,
  };
  const run = await scenario(
    'llm-terminal-agent.ts',
    { slot: 'dashboard', prompt: 'explain status' },
    (socket, frame) => {
      const call = frame.payload.call;
      if (call === 'pane_list')
        respond(socket, frame, { reply: 'panes', with: { panes: [pane], truncated: false } });
      else if (call === 'pane_semantic_read')
        respond(socket, frame, {
          reply: 'semantics',
          with: {
            slot: 'dashboard',
            generation: 2,
            revision: 3,
            root: {
              id: 0,
              role: 'status',
              label: 'Deployment healthy',
              value: null,
              disabled: false,
              destructive: false,
              actions: [],
              children: [],
            },
            truncated: false,
          },
        });
    },
  );
  assert.equal(run.result.selected.kind, 'ui');
  assert.match(run.result.selected.text, /Deployment healthy/);
  assert.equal(run.calls.includes('terminal_write_pane'), false);
});

test('embeddings indexer reconciles, recursively discovers, streams, and checkpoints', async () => {
  const document = new TextEncoder().encode('alpha beta gamma');
  const run = await scenario(
    'embeddings-indexer.ts',
    { roots: ['src'], suffixes: ['.md'], chunkBytes: 6, once: true },
    (socket, frame) => {
      const { call } = frame.payload;
      if (call === 'state_read')
        respond(socket, frame, { reply: 'state', with: { identity: 'absent', contents: [] } });
      else if (call === 'filesystem_inventory')
        respond(socket, frame, {
          reply: 'file_inventory',
          with: {
            entries: [
              { path: 'src/a.md', directory: false, size: document.length, identity: 'doc-v1' },
            ],
            complete: true,
            coalesced: 0,
            revision: 7,
            journal: FILE_JOURNAL,
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
      } else if (call === 'filesystem_changes')
        respond(socket, frame, {
          reply: 'file_changes',
          with: {
            journal: FILE_JOURNAL,
            changes: [],
            next: 7,
            current: 7,
            more: false,
            truncated: false,
          },
        });
      else if (call === 'state_write') {
        assert.equal(frame.payload.with.observed, 'absent');
        assert.match(
          new TextDecoder().decode(Uint8Array.from(frame.payload.with.contents)),
          /"src\/a.md":\{"identity":"doc-v1","digest":"[0-9a-f]{64}","bytes":16\}/,
        );
        respond(socket, frame, { reply: 'identity', with: `sha256:${'d'.repeat(64)}` });
      }
    },
  );
  assert.equal(run.result.path, 'src/a.md');
  assert.equal(run.result.identity, 'doc-v1');
  assert.equal(run.calls.find(({ call }) => call === 'filesystem_changes').with.after, 7);
  const ranges = run.calls.filter(({ call }) => call === 'filesystem_read_range');
  assert.deepEqual(
    ranges.map(({ with: value }) => [value.offset, value.observed]),
    [
      [0, 'doc-v1'],
      [6, 'doc-v1'],
      [12, 'doc-v1'],
    ],
  );
});

test('embeddings indexer refuses publication after journal invalidation', async () => {
  const document = new TextEncoder().encode('changed underneath');
  const run = await scenario(
    'embeddings-indexer.ts',
    { roots: ['src'], suffixes: ['.md'], once: true },
    (socket, frame) => {
      const { call } = frame.payload;
      if (call === 'state_read')
        respond(socket, frame, { reply: 'state', with: { identity: 'absent', contents: [] } });
      else if (call === 'filesystem_inventory')
        respond(socket, frame, {
          reply: 'file_inventory',
          with: {
            entries: [
              { path: 'src/a.md', directory: false, size: document.length, identity: 'doc-v1' },
            ],
            complete: true,
            coalesced: 0,
            revision: 20,
            journal: FILE_JOURNAL,
          },
        });
      else if (call === 'filesystem_list_page')
        respond(socket, frame, {
          reply: 'directory_page',
          with: {
            entries: [
              { path: 'src/a.md', directory: false, size: document.length, identity: 'doc-v1' },
            ],
            identity: 'src-v1',
            next: 'src/a.md',
            more: false,
          },
        });
      else if (call === 'filesystem_read_range')
        respond(socket, frame, {
          reply: 'file_range',
          with: {
            path: 'src/a.md',
            identity: 'doc-v1',
            offset: 0,
            total: document.length,
            contents: [...document],
            eof: true,
            truncated: false,
          },
        });
      else if (call === 'filesystem_changes')
        respond(socket, frame, {
          reply: 'file_changes',
          with: {
            journal: FILE_JOURNAL,
            changes: [{ revision: 21, kind: 'invalidate', path: 'src/a.md', entry: null }],
            next: 21,
            current: 21,
            more: false,
            truncated: false,
          },
        });
    },
    1,
  );
  assert.match(run.stderr, /document changed after inventory/);
  assert.equal(
    run.calls.some(({ call }) => call === 'state_write'),
    false,
  );
});

test('embeddings workbench resumes, indexes changed ranges with an opaque credential, and renders progress', async () => {
  const container = 'c'.repeat(64);
  const execution = 'e'.repeat(32);
  const document = Buffer.from('export const answer = 42;');
  const previous = {
    version: 1,
    revision: 5,
    documents: { 'src/old.ts': { identity: 'old-v1', embedding: '[0.1]' } },
  };
  const run = await scenario(
    'embeddings-workbench.ts',
    { root: 'src', model: { container, generation: 7, credential: 'embeddings.api-key' } },
    (socket, frame) => {
      const { call, with: value } = frame.payload;
      if (call === 'state_read')
        respond(socket, frame, {
          reply: 'state',
          with: {
            identity: `sha256:${'a'.repeat(64)}`,
            contents: [...Buffer.from(JSON.stringify(previous))],
          },
        });
      else if (call === 'filesystem_inventory')
        respond(socket, frame, {
          reply: 'file_inventory',
          with: { journal: FILE_JOURNAL, entries: [], complete: true, coalesced: 0, revision: 6 },
        });
      else if (call === 'filesystem_changes')
        respond(socket, frame, {
          reply: 'file_changes',
          with: {
            journal: FILE_JOURNAL,
            changes: [
              {
                revision: 6,
                kind: 'modify',
                path: 'src/new.ts',
                entry: {
                  path: 'src/new.ts',
                  directory: false,
                  size: document.length,
                  identity: 'new-v2',
                },
              },
            ],
            next: 6,
            current: 6,
            more: false,
            truncated: false,
          },
        });
      else if (call === 'filesystem_list_page')
        respond(socket, frame, {
          reply: 'directory_page',
          with: {
            entries: [
              { path: 'src/new.ts', directory: false, size: document.length, identity: 'new-v2' },
              { path: 'src/old.ts', directory: false, size: 1, identity: 'old-v1' },
            ],
            identity: 'directory-v2',
            next: 'src/old.ts',
            more: false,
          },
        });
      else if (call === 'filesystem_read_range') {
        const bytes = document.subarray(value.offset, value.offset + value.limit);
        respond(socket, frame, {
          reply: 'file_range',
          with: {
            path: 'src/new.ts',
            identity: 'new-v2',
            offset: value.offset,
            total: document.length,
            contents: [...bytes],
            eof: value.offset + bytes.length === document.length,
            truncated: value.offset + bytes.length !== document.length,
          },
        });
      } else if (call === 'container_exec_credential')
        respond(socket, frame, { reply: 'identity', with: execution });
      else if (call === 'execution_output')
        respond(socket, frame, {
          reply: 'execution_output',
          with: {
            entries:
              value.after === 0
                ? [
                    {
                      sequence: 1,
                      timestamp_ms: 7,
                      stream: 'stdout',
                      bytes: [...Buffer.from('[0.2]\n')],
                    },
                  ]
                : [],
            next: 1,
            more: false,
            eof: true,
            gap: false,
          },
        });
      else if (call === 'execution_inspect')
        respond(socket, frame, {
          reply: 'execution',
          with: {
            id: execution,
            container_id: container,
            running: false,
            exit_code: 0,
            pid: 9,
            command: ['embed', 'src/new.ts'],
            user: 'indexer',
          },
        });
      else if (call === 'state_write')
        respond(socket, frame, { reply: 'identity', with: `sha256:${'b'.repeat(64)}` });
      else if (call === 'interface_open_tab')
        respond(socket, frame, { reply: 'identity', with: 'index-pane' });
      else respond(socket, frame, { reply: 'done' });
    },
  );
  assert.deepEqual(run.result, {
    indexed: 1,
    revision: 6,
    checkpoint: `sha256:${'b'.repeat(64)}`,
  });
  assert.equal(run.calls.find(({ call }) => call === 'filesystem_changes').with.after, 5);
  assert.deepEqual(
    run.calls
      .filter(({ call }) => call === 'filesystem_read_range')
      .map(({ with: value }) => [value.offset, value.limit, value.observed]),
    [
      [0, 8, 'new-v2'],
      [8, 8, 'new-v2'],
      [16, 8, 'new-v2'],
      [24, 8, 'new-v2'],
    ],
  );
  const credentialExec = run.calls.find(({ call }) => call === 'container_exec_credential');
  assert.deepEqual(credentialExec.with.credentials, [['EMBEDDINGS_API_KEY', 'embeddings.api-key']]);
  assert.equal(
    run.calls.some(({ call }) => call === 'credential_read'),
    false,
  );
  assert(
    run.calls.some(({ call }) => call === 'interface_render' || call === 'interface_render_at'),
    'progress UI was not rendered',
  );
  assert.match(
    new TextDecoder().decode(
      Uint8Array.from(run.calls.find(({ call }) => call === 'state_write').with.contents),
    ),
    /"revision":6.*"src\/old.ts".*"src\/new.ts"/,
  );
});

test('Git review resumes bounded inspection and applies one identity-observed file over Unix framing', async () => {
  const container = 'c'.repeat(64);
  const statusExecution = 'a'.repeat(32);
  const diffExecution = 'b'.repeat(32);
  const head = 'd'.repeat(40);
  const source = Buffer.from('export function enabled() {\n  return false;\n}\n');
  let executions = 0;
  const granted = [
    'containers:read',
    'containers:execute',
    'filesystem:read',
    'filesystem:write',
    'state:read',
    'state:write',
    'panes:observe',
    'terminals:output',
    'interface:render',
  ];
  const run = await scenario(
    'git-review-workbench.ts',
    {
      repository: '/workspace',
      container,
      generation: 7,
      file: 'src/flag.ts',
      terminal: 'review-shell',
    },
    (socket, frame) => {
      const { call, with: value } = frame.payload;
      if (call === 'state_read')
        respond(socket, frame, {
          reply: 'state',
          with: {
            identity: `sha256:${'1'.repeat(64)}`,
            contents: [
              ...Buffer.from(JSON.stringify({ version: 1, head: 'c'.repeat(40), reviewed: {} })),
            ],
          },
        });
      else if (call === 'container_exec') {
        const id = executions++ === 0 ? statusExecution : diffExecution;
        respond(socket, frame, { reply: 'identity', with: id });
      } else if (call === 'execution_output') {
        const output =
          value.id === statusExecution
            ? `# branch.oid ${head}\u0000 1 .M N... src/flag.ts\u0000`
            : '@@ -1,3 +1,3 @@\n-  return false;\n+  return true;\n';
        respond(socket, frame, {
          reply: 'execution_output',
          with: {
            entries:
              value.after === 0
                ? [
                    {
                      sequence: 1,
                      timestamp_ms: 1,
                      stream: 'stdout',
                      bytes: [...Buffer.from(output)],
                    },
                  ]
                : [],
            next: 1,
            more: false,
            eof: true,
            gap: false,
          },
        });
      } else if (call === 'execution_inspect')
        respond(socket, frame, {
          reply: 'execution',
          with: {
            id: value.id,
            container_id: container,
            running: false,
            exit_code: 0,
            pid: 8,
            command: ['git'],
            user: 'reviewer',
          },
        });
      else if (call === 'filesystem_stat')
        respond(socket, frame, {
          reply: 'entry',
          with: { path: 'src/flag.ts', directory: false, size: source.length, identity: 'file-v1' },
        });
      else if (call === 'filesystem_read_range') {
        const bytes = source.subarray(value.offset, value.offset + value.limit);
        respond(socket, frame, {
          reply: 'file_range',
          with: {
            path: value.path,
            identity: 'file-v1',
            offset: value.offset,
            total: source.length,
            contents: [...bytes],
            eof: value.offset + bytes.length === source.length,
            truncated: value.offset + bytes.length !== source.length,
          },
        });
      } else if (call === 'pane_list')
        respond(socket, frame, {
          reply: 'panes',
          with: {
            panes: [
              {
                slot: 'review-shell',
                generation: 2,
                revision: 4,
                kind: 'terminal',
                provider: null,
                tab: 'review',
                title: 'Review shell',
                focused: true,
              },
            ],
            truncated: false,
          },
        });
      else if (call === 'terminal_read_pane')
        respond(socket, frame, {
          reply: 'text',
          with: {
            slot: 'review-shell',
            generation: 2,
            revision: 4,
            columns: 80,
            rows: 24,
            lines: ['$ git status', 'modified: src/flag.ts'],
            cursor_column: 0,
            cursor_row: 1,
            truncated: false,
          },
        });
      else if (call === 'filesystem_write_observed')
        respond(socket, frame, { reply: 'identity', with: 'file-v2' });
      else if (call === 'state_write')
        respond(socket, frame, { reply: 'identity', with: `sha256:${'2'.repeat(64)}` });
      else if (call === 'interface_open_tab')
        respond(socket, frame, { reply: 'identity', with: 'review-pane' });
      else respond(socket, frame, { reply: 'done' });
    },
    0,
    granted,
  );
  assert.deepEqual(run.result, {
    head,
    written: 'file-v2',
    persisted: `sha256:${'2'.repeat(64)}`,
    resumed: 'c'.repeat(40),
  });
  assert.deepEqual(
    run.calls
      .filter(({ call }) => call === 'container_exec')
      .map(({ with: value }) => value.command),
    [
      ['git', '-C', '/workspace', 'status', '--porcelain=v2', '--branch', '-z'],
      ['git', '-C', '/workspace', 'diff', '--no-ext-diff', '--unified=3', '--', 'src/flag.ts'],
    ],
  );
  assert(run.calls.filter(({ call }) => call === 'filesystem_read_range').length >= 3);
  const write = run.calls.find(({ call }) => call === 'filesystem_write_observed');
  assert.equal(write.with.path, 'src/flag.ts');
  assert.equal(write.with.observed, 'file-v1');
  assert.match(new TextDecoder().decode(Uint8Array.from(write.with.contents)), /return true/);
  assert(run.calls.some(({ call }) => call === 'terminal_read_pane'));
  assert(
    run.calls.some(({ call }) => call === 'interface_render' || call === 'interface_render_at'),
  );
});

test('Postgres GUI stays live and serves a scrolled database window before host shutdown', async () => {
  const id = 'c'.repeat(64);
  const credentialKey = 'postgres.password';
  const executionId = 'e'.repeat(32);
  let outputCalls = 0;
  const run = await scenario(
    'postgres-inspector.ts',
    {
      container: id,
      credentialKey,
      query: 'select id,name from widgets',
    },
    (socket, frame) => {
      const { call } = frame.payload;
      if (call === 'container_list')
        respond(socket, frame, {
          reply: 'containers',
          with: [
            {
              id,
              name: 'postgres',
              image: 'postgres:18',
              state: 'running',
              created: 1,
              generation: 2,
            },
          ],
        });
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
            snapshot: 'a'.repeat(64),
            next: null,
            more: false,
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
            networks: [
              { id: 'n1', name: 'backend', driver: 'bridge', scope: 'local', kind: 'custom' },
            ],
            truncated: false,
          },
        });
      else if (call === 'container_exec_credential')
        respond(socket, frame, { reply: 'identity', with: executionId });
      else if (call === 'execution_output') {
        outputCalls += 1;
        const bytes =
          outputCalls === 1
            ? Buffer.from('rows\n1000000\n')
            : Buffer.from('id,name\n1,alpha\n2,beta\n');
        respond(socket, frame, {
          reply: 'execution_output',
          with: {
            entries: [{ sequence: 1, timestamp_ms: 1, stream: 'stdout', bytes: [...bytes] }],
            next: 1,
            more: false,
            eof: true,
            gap: false,
          },
        });
      } else if (call === 'execution_inspect')
        respond(socket, frame, {
          reply: 'execution',
          with: {
            id: executionId,
            container_id: id,
            running: false,
            exit_code: 0,
            pid: 42,
            command: ['psql', '--csv'],
            user: 'postgres',
          },
        });
      else if (call === 'interface_open_tab')
        respond(socket, frame, { reply: 'identity', with: 'postgres-pane' });
      else if (call === 'source_resize_at') {
        respond(socket, frame, { reply: 'done' });
        if (frame.payload.with.mutation.Length) {
          socket.write(
            encode({
              channel: 17,
              kind: KIND.event,
              payload: {
                id: 41,
                slot: 'postgres-pane',
                source: 1,
                version: 1,
                range: { start: 999999, count: 1 },
                sort: null,
                filter: null,
              },
            }),
          );
        } else if (frame.payload.with.mutation.Window) {
          socket.end();
        }
      } else respond(socket, frame, { reply: 'done' });
    },
    0,
    capabilities,
    (socket, frame) => {
      if (frame.channel === CONTROL) return;
      assert.equal(frame.channel, 17);
      assert.deepEqual(frame.payload, {
        source: 1,
        version: 1,
        request: 41,
        range: { start: 999999, count: 1 },
        rows: [{ key: 999999, cells: [{ Text: '1' }, { Text: 'alpha' }] }],
      });
      socket.end();
    },
  );
  assert.deepEqual(run.result, {
    container: id,
    processes: 2,
    queryRows: 1000000,
    networks: 1,
    slot: 'postgres-pane',
  });
  const rendered = run.calls.find(({ call }) => call === 'interface_render_at');
  assert(rendered, 'React tree was not rendered');
  const mutations = run.calls
    .filter(({ call }) => call === 'source_resize_at')
    .map(({ with: value }) => value.mutation);
  assert.equal(mutations.length, 2);
  assert.equal(mutations[1].Length.rows, 1000000);
  const execs = run.calls.filter(({ call }) => call === 'container_exec_credential');
  assert.equal(execs.length, 3);
  assert.equal(run.calls.filter(({ call }) => call === 'execution_inspect').length, 3);
  assert.equal(run.calls.filter(({ call }) => call === 'execution_remove').length, 3);
  assert.deepEqual(execs[1].with.command.slice(0, 4), [
    'psql',
    '--csv',
    '--no-psqlrc',
    '--command',
  ]);
  assert.match(execs[1].with.command[4], /LIMIT 128 OFFSET 0$/);
  assert.match(execs[2].with.command[4], /LIMIT 1 OFFSET 999999$/);
  assert.deepEqual(execs[1].with.credentials, [['PGPASSWORD', credentialKey]]);
  assert.equal(
    run.calls.some(({ call }) => call === 'filesystem_read'),
    false,
  );
  assert.equal(
    run.calls.some(({ call }) => call === 'credential_read'),
    false,
  );
});
