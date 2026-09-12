import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';
import { connect, ExtensionError, TerminalCommandOperationError, workspace } from '../dist/index.js';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

const id = 'e'.repeat(32);
const owner = 'a'.repeat(32);
const pane = { slot: 'term-1', generation: 3, revision: 9 };
const running = {
  id,
  owner,
  ...pane,
  running: true,
  exit_code: 0,
  pid: 41,
  command: ['sh', '-lc', 'printf ready; exit 17'],
};

function fragmented(socket, frame) {
  const bytes = encode(frame);
  for (let offset = 0; offset < bytes.length; offset += 2) {
    socket.write(bytes.subarray(offset, offset + 2));
  }
}

test('supervised terminal command is authoritative over fragmented real Unix framing', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-terminal-command-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  let output = 0;
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload);
        const call = frame.payload.call;
        let payload;
        if (call === 'terminal_command_start') {
          assert.deepEqual(frame.payload.with, {
            ...pane,
            command: running.command,
            stdin: true,
          });
          payload = { reply: 'terminal_command', with: running };
        } else if (call === 'terminal_command_write') {
          assert.deepEqual(frame.payload.with, { id, owner, ...pane, contents: [113, 10] });
          payload = { reply: 'terminal_command_input', with: { id, committed: 2 } };
        } else if (call === 'terminal_command_close_input') {
          payload = { reply: 'done' };
        } else if (call === 'terminal_command_output') {
          output += 1;
          payload = {
            reply: 'terminal_command_output',
            with: {
              id,
              owner,
              ...pane,
              output: {
                entries:
                  output === 1
                    ? [{ sequence: 1, timestamp_ms: 1, stream: 'stdout', bytes: [114, 101] }]
                    : [{ sequence: 2, timestamp_ms: 2, stream: 'stdout', bytes: [97, 100, 121, 10] }],
                next: output,
                more: false,
                eof: output === 2,
                gap: false,
              },
            },
          };
        } else if (call === 'terminal_command_wait') {
          payload = {
            reply: 'terminal_command',
            with: { ...running, running: false, exit_code: 17, pid: 0 },
          };
        } else {
          throw new Error(`unexpected ${call}`);
        }
        fragmented(socket, { channel: 2, kind: KIND.response, payload });
      }
    });
    fragmented(socket, {
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'terminal-command-test',
        granted: ['terminals:process-control', 'terminals:output', 'terminals:input'],
      },
    });
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).terminal.commandText(pane, {
      command: running.command,
      input: 'q\n',
      maxBytes: 64,
      pollIntervalMs: 10,
    });
    assert.equal(result.stdout, 'ready\n');
    assert.equal(result.stderr, '');
    assert.equal(result.command.exit_code, 17);
    assert.equal(result.command.running, false);
    assert.deepEqual(
      calls.map(({ call }) => call),
      [
        'terminal_command_start',
        'terminal_command_write',
        'terminal_command_close_input',
        'terminal_command_output',
        'terminal_command_output',
        'terminal_command_wait',
      ],
    );
    await session.close();
  } finally {
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('aborting idle command polling immediately cancels the exact supervised command', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-terminal-command-abort-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        const call = frame.payload.call;
        const payload =
          call === 'terminal_command_start'
            ? { reply: 'terminal_command', with: running }
            : call === 'terminal_command_output'
              ? {
                  reply: 'terminal_command_output',
                  with: {
                    id,
                    owner,
                    ...pane,
                    output: { entries: [], next: 0, more: false, eof: false, gap: false },
                  },
                }
              : call === 'terminal_command_cancel'
                ? {
                    reply: 'terminal_command',
                    with: { ...running, running: false, exit_code: 130, pid: 0 },
                  }
                : (() => {
                    throw new Error(`unexpected ${call}`);
                  })();
        fragmented(socket, { channel: frame.channel, kind: KIND.response, payload });
      }
    });
    fragmented(socket, {
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'terminal-command-abort',
        granted: ['terminals:process-control', 'terminals:output'],
      },
    });
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const controller = new AbortController();
    const started = Date.now();
    const operation = workspace(session).terminal.commandText(pane, {
      command: running.command,
      maxBytes: 64,
      pollIntervalMs: 60_000,
      signal: controller.signal,
      cancelSignal: 'SIGINT',
      cancelTimeoutMs: 1_000,
    });
    while (!calls.some(({ call }) => call === 'terminal_command_output'))
      await new Promise((resolve) => setImmediate(resolve));
    controller.abort('agent deadline');
    await assert.rejects(operation, TerminalCommandOperationError);
    assert(Date.now() - started < 1_000, 'abort must not wait for the 60 second poll interval');
    assert.deepEqual(calls.at(-1), {
      call: 'terminal_command_cancel',
      with: { id, owner, ...pane, signal: 'SIGINT', timeout_ms: 1_000 },
    });
    await session.close();
  } finally {
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('supervised command output survives reconnect and originating pane replacement', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-terminal-command-resume-'));
  const socketPath = path.join(directory, 'host.sock');
  const requests = [];
  const connections = new Set();
  let accepted = 0;
  const server = net.createServer((socket) => {
    accepted += 1;
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        requests.push(frame.payload);
        const payload =
          frame.payload.call === 'terminal_command_start'
            ? { reply: 'terminal_command', with: running }
            : {
                reply: 'terminal_command_output',
                with: {
                  id,
                  owner,
                  ...pane,
                  output: {
                    entries: [
                      { sequence: 8, timestamp_ms: 8, stream: 'stdout', bytes: [100, 111, 110, 101, 10] },
                    ],
                    next: 8,
                    more: false,
                    eof: true,
                    gap: false,
                  },
                },
              };
        fragmented(socket, { channel: frame.channel, kind: KIND.response, payload });
      }
    });
    fragmented(socket, {
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: `terminal-command-resume-${accepted}`,
        granted: ['terminals:process-control', 'terminals:output'],
      },
    });
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const first = await connect({ path: socketPath });
    const command = await workspace(first).terminal.commandStart(pane, running.command);
    await first.close();

    // The UI may now contain another occupant at term-1. The creation snapshot
    // remains part of the command capability and is intentionally replayed.
    const resumed = await connect({ path: socketPath });
    const page = await workspace(resumed).terminal.commandOutput(command, { after: 7, limit: 1 });
    assert.equal(page.output.next, 8);
    assert.equal(new TextDecoder().decode(Uint8Array.from(page.output.entries[0].bytes)), 'done\n');
    assert.deepEqual(requests[1], {
      call: 'terminal_command_output',
      with: { id, owner, ...pane, after: 7, limit: 1 },
    });
    assert.equal(accepted, 2);
    await resumed.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('a command from a replaced installation is denied over fragmented framing without poisoning the session', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-terminal-command-incarnation-'));
  const socketPath = path.join(directory, 'host.sock');
  const requests = [];
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        requests.push(frame.payload);
        const payload =
          frame.payload.call === 'terminal_command_output'
            ? {
                error: 'denied',
                capability: 'terminals:output',
                detail: 'terminal command belongs to another extension installation',
              }
            : {
                reply: 'workspace',
                with: { name: 'dev', architecture: 'arm64', image: 'alpine' },
              };
        fragmented(socket, {
          channel: frame.channel,
          kind: KIND.response,
          ...(payload.error ? { flags: 3 } : {}),
          payload,
        });
      }
    });
    fragmented(socket, {
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'replacement-installation',
        granted: ['terminals:output', 'workspaces:read'],
      },
    });
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    await assert.rejects(
      workspace(session).terminal.commandOutput(running, { after: 0, limit: 1 }),
      (error) =>
        error instanceof ExtensionError &&
        error.kind === 'denied' &&
        error.message.includes('another extension installation'),
    );
    assert.equal((await workspace(session).info()).name, 'dev');
    assert.deepEqual(requests[0], {
      call: 'terminal_command_output',
      with: { id, owner, ...pane, after: 0, limit: 1 },
    });
    assert.equal(requests[1].call, 'workspace_info');
    await session.close();
  } finally {
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('large Git review output exposes an exact reconnect cursor after fragmented disconnect', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-git-review-resume-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  const requests = [];
  let accepted = 0;
  let firstOutput = true;
  const gitCommand = ['git', 'diff', '--no-ext-diff', '--src-prefix=a/', '--dst-prefix=b/'];
  const server = net.createServer((socket) => {
    accepted += 1;
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const connection = accepted;
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        requests.push(frame.payload);
        if (frame.payload.call === 'terminal_command_start') {
          fragmented(socket, {
            channel: frame.channel,
            kind: KIND.response,
            payload: { reply: 'terminal_command', with: { ...running, command: gitCommand } },
          });
        } else if (frame.payload.call === 'terminal_command_output' && connection === 1) {
          if (firstOutput) {
            firstOutput = false;
            fragmented(socket, {
              channel: frame.channel,
              kind: KIND.response,
              payload: {
                reply: 'terminal_command_output',
                with: {
                  id,
                  owner,
                  ...pane,
                  output: {
                    entries: [
                      {
                        sequence: 1,
                        timestamp_ms: 1,
                        stream: 'stdout',
                        bytes: Array.from(new TextEncoder().encode('diff --git a/a b/a\n')),
                      },
                    ],
                    next: 1,
                    more: false,
                    eof: false,
                    gap: false,
                  },
                },
              },
            });
          } else {
            socket.destroy();
          }
        } else if (frame.payload.call === 'terminal_command_cancel' && connection === 1) {
          socket.destroy();
        } else if (frame.payload.call === 'terminal_command_output' && connection === 2) {
          fragmented(socket, {
            channel: frame.channel,
            kind: KIND.response,
            payload: {
              reply: 'terminal_command_output',
              with: {
                id,
                owner,
                ...pane,
                output: {
                  entries: [
                    {
                      sequence: 2,
                      timestamp_ms: 2,
                      stream: 'stderr',
                      bytes: Array.from(new TextEncoder().encode('warning: recovered\n')),
                    },
                  ],
                  next: 2,
                  more: false,
                  eof: true,
                  gap: false,
                },
              },
            },
          });
        }
      }
    });
    fragmented(socket, {
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: `git-review-${connection}`,
        granted: ['terminals:process-control', 'terminals:output'],
      },
    });
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const first = await connect({ path: socketPath });
    let failure;
    try {
      await workspace(first).terminal.commandText(pane, {
        command: gitCommand,
        maxBytes: 1024 * 1024,
        pageLimit: 1,
        pollIntervalMs: 10,
      });
    } catch (error) {
      failure = error;
    }
    assert(failure instanceof TerminalCommandOperationError, `${failure?.constructor?.name}: ${failure}`);
    assert.equal(failure.phase, 'output');
    assert.equal(failure.after, 1);
    assert.equal(failure.command.id, id);
    assert.deepEqual(failure.command.command, gitCommand);
    assert(Object.isFrozen(failure.command));
    assert(Object.isFrozen(failure.command.command));

    const resumed = await connect({ path: socketPath });
    const remainder = await workspace(resumed).terminal.commandOutput(failure.command, {
      after: failure.after,
      limit: 1,
    });
    assert.equal(remainder.output.next, 2);
    assert.equal(remainder.output.eof, true);
    assert.deepEqual(requests.at(-1), {
      call: 'terminal_command_output',
      with: { id, owner, ...pane, after: 1, limit: 1 },
    });
    await resumed.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
