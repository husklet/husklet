import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';

import { TerminalOperationError, connect, workspace } from '../dist/index.js';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

test('fragmented Unix session discloses completed input authority when observation is cancelled', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-terminal-cancel-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  let revision = 7;
  let reads = 0;
  let written;
  const screen = () => ({
    slot: 'agent',
    generation: 4,
    revision,
    columns: 80,
    rows: 24,
    lines: [revision === 7 ? '$ ' : '$ deploy\ndeploy started'],
    cursor_column: 0,
    cursor_row: revision === 7 ? 0 : 1,
    truncated: false,
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const send = (payload) => {
      const bytes = encode(payload);
      for (let offset = 0; offset < bytes.length; offset += 2) {
        socket.write(bytes.subarray(offset, offset + 2));
      }
    };
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        const reply = (payload) => send({ channel: frame.channel, kind: KIND.response, payload });
        if (frame.payload.call === 'event_subscribe' || frame.payload.call === 'event_unsubscribe') {
          reply({ reply: 'done' });
        } else if (frame.payload.call === 'terminal_read_pane') {
          reads += 1;
          reply({ reply: 'text', with: screen() });
        } else if (frame.payload.call === 'pane_list') {
          reply({
            reply: 'panes',
            with: {
              panes: [
                {
                  slot: 'agent',
                  generation: 4,
                  revision,
                  kind: 'terminal',
                  provider: null,
                  tab: null,
                  title: 'Agent',
                  focused: true,
                },
              ],
              truncated: false,
            },
          });
        } else if (frame.payload.call === 'terminal_write_pane') {
          written = frame.payload.with.contents;
          revision = 8;
          send({
            channel: 100,
            kind: KIND.event,
            payload: {
              snapshot: 'pane_changes',
              of: { slot: 'agent', kind: 'terminal', generation: 4, revision, coalesced: 0 },
            },
          });
          reply({ reply: 'done' });
        }
      }
    });
    send({
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'terminal-cancellation',
        granted: ['panes:observe', 'terminals:output', 'terminals:input'],
      },
    });
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const terminal = workspace(session).terminal;
    const cancellation = new AbortController();
    const operation = terminal.writeObservedAndWaitForQuietText(screen(), [0x03, 0x0a], {
      quietMs: 1_000,
      timeoutMs: 5_000,
      signal: cancellation.signal,
    });
    const rejection = assert.rejects(operation, (error) => {
      assert(error instanceof TerminalOperationError);
      assert.equal(error.operation, 'write-input');
      assert.deepEqual(error.result, {
        slot: 'agent',
        generation: 4,
        revision: 7,
        written: true,
        after: { kind: 'terminal', generation: 4, revision: 8 },
      });
      assert.equal(error.cause?.name, 'AbortError');
      return true;
    });
    while (revision === 7 || reads < 2) await new Promise((resolve) => setTimeout(resolve, 1));
    await new Promise((resolve) => setTimeout(resolve, 10));
    cancellation.abort('agent deadline');
    await rejection;
    assert.deepEqual(written, [0x03, 0x0a], 'control bytes arrive exactly once');
    await session.close();
    const resumed = await connect({ path: socketPath });
    assert.equal(
      (await workspace(resumed).terminal.read('agent')).revision,
      8,
      'authoritative state can be reconciled on a fresh ordered session',
    );
    await resumed.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
