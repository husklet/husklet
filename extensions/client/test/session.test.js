import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';
import { connect, Session, workspace } from '../src/index.js';
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
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: { protocol: 1, peer: 'fixture', granted: ['workspace-read'] } }));
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

test('real Unix control frames ping both directions and close every pending operation', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-control-'));
  const socketPath = path.join(directory, 'host.sock');
  const frames = [];
  let connected;
  const server = net.createServer((socket) => {
    connected = socket;
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        frames.push(frame);
        if (frame.kind === KIND.ping) socket.write(encode({ channel: frame.channel, kind: KIND.pong, payload: frame.payload }));
      }
    });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: { protocol: 1, peer: 'control', granted: [] } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const closed = [];
    const session = await connect({ path: socketPath, timeout: 40, onClose: (error) => closed.push(error.message) });
    connected.write(encode({ channel: 17, kind: KIND.ping, payload: Buffer.from([0, 255, 4]) }));
    await session.ping();
    await new Promise((resolve) => setImmediate(resolve));
    const pong = frames.find((frame) => frame.kind === KIND.pong);
    assert.deepEqual(pong?.payload, Buffer.from([0, 255, 4]));
    const pending = session.call('workspace_info');
    connected.write(encode({ channel: CONTROL, kind: KIND.close, payload: Buffer.alloc(0) }));
    await assert.rejects(pending, /host closed the session/);
    await new Promise((resolve) => setTimeout(resolve, 60));
    assert.equal(closed.length, 1);
  } finally {
    connected?.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix partial EOF and illegal headers fail closed without sending credit', async () => {
  for (const malformed of ['partial', 'flags']) {
    const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-malformed-'));
    const socketPath = path.join(directory, 'host.sock');
    const replies = [];
    const server = net.createServer((socket) => {
      socket.on('data', (chunk) => replies.push(chunk));
      if (malformed === 'partial') {
        const frame = encode({ channel: CONTROL, kind: KIND.open, payload: { protocol: 1, peer: 'bad', granted: [] } });
        socket.end(frame.subarray(0, frame.length - 2));
      } else {
        const frame = encode({ channel: CONTROL, kind: KIND.open, payload: { protocol: 1, peer: 'bad', granted: [] } });
        frame[9] = 0x80;
        socket.end(frame);
      }
    });
    await new Promise((resolve) => server.listen(socketPath, resolve));
    try {
      await assert.rejects(connect({ path: socketPath, connectTimeout: 1_000 }), malformed === 'partial' ? /unfinished frame/ : /unknown flags/);
      assert.equal(Buffer.concat(replies).length, 0, 'a malformed peer receives neither greeting nor event credit');
    } finally {
      await new Promise((resolve) => server.close(resolve));
      await rm(directory, { recursive: true, force: true });
    }
  }
});

test('real socket write backpressure admits no further calls until drain', async () => {
  const bounded = (promise, label) => Promise.race([
    promise,
    new Promise((_, reject) => setTimeout(() => reject(new Error(`${label} timed out`)), 1_000)),
  ]);
  const server = net.createServer();
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const accepted = new Promise((resolve) => server.once('connection', resolve));
  const client = net.createConnection(server.address().port, '127.0.0.1');
  await new Promise((resolve, reject) => { client.once('connect', resolve); client.once('error', reject); });
  const host = await accepted;
  const baselineListeners = Object.fromEntries(['data', 'end', 'drain'].map((event) => [event, client.listenerCount(event)]));
  host.write(encode({ channel: CONTROL, kind: KIND.open, payload: { protocol: 1, peer: 'pressure', granted: [] } }));
  const session = new Session(client, { timeout: 1_000 });
  await bounded(session.ready, 'greeting');
  const write = client.write.bind(client);
  client.write = (bytes) => { write(bytes); return false; };
  const first = session.call('workspace_info');
  await assert.rejects(session.call('workspace_info'), /write backpressure/);
  host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'workspace', with: { name: 'demo', image: 'alpine', architecture: 'amd64' } } }));
  await bounded(first, 'first call');
  client.write = write;
  client.emit('drain');
  const second = session.call('workspace_info');
  host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'workspace', with: { name: 'demo', image: 'alpine', architecture: 'amd64' } } }));
  await bounded(second, 'second call');
  await bounded(session.close(), 'close');
  for (const [event, count] of Object.entries(baselineListeners)) assert.equal(client.listenerCount(event), count);
  host.destroy();
  await new Promise((resolve) => server.close(resolve));
});

test('a real Unix reply on an uncorrelated channel fails the ordered session closed', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-channel-'));
  const socketPath = path.join(directory, 'host.sock');
  let peer;
  const server = net.createServer((socket) => {
    peer = socket;
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: { protocol: 1, peer: 'channel', granted: [] } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath, timeout: 200 });
    const pending = session.call('workspace_info');
    peer.write(encode({ channel: 4, kind: KIND.response, payload: { reply: 'workspace', with: { name: 'wrong', image: 'alpine', architecture: 'amd64' } } }));
    await assert.rejects(pending, /unexpected 2 frame on channel 4/);
    await session.close();
  } finally {
    peer?.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
