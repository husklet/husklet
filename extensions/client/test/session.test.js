import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';
import { connect, workspace } from '../src/index.js';
import { CONTROL, KIND, Reader, encode } from '../src/wire.js';

test('real Unix stream negotiates grants, correlates a call, and returns event credit', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-client-'));
  const socketPath = path.join(directory, 'host.sock');
  const observed = [];
  let creditSeen;
  const credit = new Promise((resolve) => { creditSeen = resolve; });
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        observed.push(frame);
        if (frame.channel === 7 && frame.kind === KIND.credit) creditSeen();
        if (frame.channel === CONTROL && frame.kind === KIND.response) continue;
        if (frame.channel === 2 && frame.payload.call === 'event_subscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        }
        if (frame.channel === 2 && frame.payload.call === 'workspace_info') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'workspace', with: { name: 'demo', image: 'alpine', architecture: 'amd64' } } }));
          socket.write(encode({ channel: 7, kind: KIND.event, payload: { snapshot: 'containers', of: [] } }));
        }
      }
    });
    socket.write(encode({ channel: CONTROL, kind: KIND.request, payload: { protocol: 1, peer: 'fixture', granted: ['workspace-read'] } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    let pushed;
    const session = await connect({ path: socketPath, onEvent: (event) => { pushed = event; } });
    assert.deepEqual(session.granted, ['workspace-read']);
    await session.call('event_subscribe', { topic: 'containers' });
    assert.equal((await workspace(session).info()).name, 'demo');
    await credit;
    assert.equal(pushed.snapshot, 'containers');
    assert(observed.some((frame) => frame.channel === CONTROL && frame.kind === KIND.response));
    assert(observed.some((frame) => frame.channel === 7 && frame.kind === KIND.credit && frame.payload === 1));
    session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
