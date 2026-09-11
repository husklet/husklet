import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';

import { connect, IncompletePaneInventoryError, workspace } from '../dist/index.js';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

test('stable pane reads reject truncated discovery over fragmented Unix frames', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-incomplete-panes-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  const calls = [];
  const pane = {
    slot: 'shell',
    generation: 4,
    revision: 9,
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
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload.call);
        const payload =
          frame.payload.call === 'pane_list'
            ? { reply: 'panes', with: { panes: [pane], truncated: true } }
            : frame.payload.call === 'terminal_read_pane'
              ? {
                  reply: 'text',
                  with: {
                    slot: 'shell',
                    generation: 4,
                    revision: 9,
                    columns: 80,
                    rows: 24,
                    lines: ['$'],
                    cursor_column: 1,
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
        peer: 'incomplete-panes',
        granted: ['panes:observe', 'terminals:output', 'workspaces:read'],
      },
    });
    socket.write(greeting.subarray(0, 4));
    socket.write(greeting.subarray(4));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const host = workspace(session);
    await assert.rejects(host.terminal.readAllStable(), (error) => {
      assert(error instanceof IncompletePaneInventoryError);
      assert.deepEqual(error.panes, [{ slot: 'shell', generation: 4, revision: 9 }]);
      assert(Object.isFrozen(error.panes));
      assert(Object.isFrozen(error.panes[0]));
      return true;
    });
    assert.deepEqual(calls, ['pane_list', 'terminal_read_pane']);
    assert.equal((await host.info()).name, 'agent');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
