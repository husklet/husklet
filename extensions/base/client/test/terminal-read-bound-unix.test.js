import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';

import { connect, TerminalReadLimitError, workspace } from '../dist/index.js';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

test('terminal text bounds fail before Unix framing and a fragmented valid read preserves the session', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-terminal-bound-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
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
          frame.payload.call === 'terminal_read_pane'
            ? {
                reply: 'text',
                with: {
                  slot: 'shell',
                  generation: 3,
                  revision: 8,
                  columns: 80,
                  rows: 24,
                  lines: ['$ ready'],
                  cursor_column: 7,
                  cursor_row: 0,
                  truncated: false,
                },
              }
            : {
                reply: 'workspace',
                with: { name: 'agent', image: 'alpine', architecture: 'amd64' },
              };
        const reply = encode({ channel: frame.channel, kind: KIND.response, payload });
        for (const byte of reply) socket.write(Uint8Array.of(byte));
      }
    });
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'terminal-bound',
        granted: ['terminals:output', 'workspaces:read'],
      },
    });
    socket.write(greeting.subarray(0, 3));
    socket.write(greeting.subarray(3));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const host = workspace(session);
    for (const lines of [0, 2001, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
      await assert.rejects(host.terminal.read('shell', lines), (error) => {
        assert(error instanceof TerminalReadLimitError);
        assert.equal(error.requested, lines);
        assert.equal(error.maximum, 2000);
        return true;
      });
    }
    for (const read of [
      () => host.terminal.toText('shell', { lines: 2001 }),
      () => host.terminal.readAll({ lines: 2001 }),
    ]) {
      await assert.rejects(read(), (error) => {
        assert(error instanceof TerminalReadLimitError);
        assert.equal(error.requested, 2001);
        return true;
      });
    }
    assert.equal(calls.length, 0, 'invalid read bounds emit no request frame');
    assert.equal((await host.terminal.read('shell', 2000)).revision, 8);
    assert.deepEqual(calls[0].with, { slot: 'shell', lines: 2000 });
    assert.equal((await host.info()).name, 'agent', 'a rejected bound leaves the session reusable');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
