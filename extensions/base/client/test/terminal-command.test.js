import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';
import { connect, workspace } from '../dist/index.js';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

const id = 'e'.repeat(32);
const pane = { slot: 'term-1', generation: 3, revision: 9 };
const running = {
  id,
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
          assert.deepEqual(frame.payload.with, { id, ...pane, contents: [113, 10] });
          payload = { reply: 'terminal_command_input', with: { id, committed: 2 } };
        } else if (call === 'terminal_command_close_input') {
          payload = { reply: 'done' };
        } else if (call === 'terminal_command_output') {
          output += 1;
          payload = {
            reply: 'terminal_command_output',
            with: {
              id,
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
      with: { id, ...pane, after: 7, limit: 1 },
    });
    assert.equal(accepted, 2);
    await resumed.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
