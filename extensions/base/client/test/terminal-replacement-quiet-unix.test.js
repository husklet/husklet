import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';

import { connect, workspace } from '../dist/index.js';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

test('quiet terminal input discloses replacement without settling unrelated output', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-quiet-replacement-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  let reads = 0;
  const screen = (generation, revision, line) => ({
    slot: 'agent',
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
    const send = (frame) => {
      const bytes = encode(frame);
      for (let offset = 0; offset < bytes.length; offset += 2)
        socket.write(bytes.subarray(offset, offset + 2));
    };
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload.call);
        const reply = (payload) => send({ channel: frame.channel, kind: KIND.response, payload });
        if (frame.payload.call === 'event_subscribe' || frame.payload.call === 'event_unsubscribe')
          reply({ reply: 'done' });
        else if (frame.payload.call === 'terminal_read_pane') {
          reads += 1;
          reply({
            reply: 'text',
            with: reads === 1 ? screen(4, 7, '$ ') : screen(5, 1, 'new shell'),
          });
        } else if (frame.payload.call === 'pane_list')
          reply({
            reply: 'panes',
            with: {
              panes: [
                {
                  slot: 'agent',
                  generation: 5,
                  revision: 1,
                  kind: 'terminal',
                  provider: null,
                  tab: null,
                  title: 'New shell',
                  focused: true,
                },
              ],
              truncated: false,
            },
          });
        else if (frame.payload.call === 'terminal_write_pane') {
          send({
            channel: 100,
            kind: KIND.event,
            payload: {
              snapshot: 'pane_changes',
              of: { slot: 'agent', kind: 'terminal', generation: 5, revision: 1, coalesced: 0 },
            },
          });
          reply({ reply: 'done' });
        }
      }
    });
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'quiet-replacement',
        granted: ['panes:observe', 'terminals:output', 'terminals:input'],
      },
    });
    for (const byte of greeting) socket.write(Uint8Array.of(byte));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const terminal = workspace(session).terminal;
    const result = await terminal.writeObservedAndWaitForQuietText(screen(4, 7, '$ '), 'whoami\n', {
      quietMs: 20,
      timeoutMs: 1_000,
    });
    assert.equal(result.changed, true);
    assert.equal(result.replaced, true);
    assert.equal(result.settled, false);
    assert.equal(result.after.snapshot.generation, 5);
    assert.equal(
      calls.filter((call) => call === 'event_subscribe').length,
      1,
      'replacement is not followed as command output',
    );
    assert.equal(
      (await terminal.read('agent')).generation,
      5,
      'the fragmented session remains reusable',
    );
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
