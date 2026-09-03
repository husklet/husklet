import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';
import { connect, Session, workspace } from '../src/index.js';
import { CONTROL, KIND, Reader, encode } from '../src/wire.js';

test('real Unix stream drives a typed inventory watcher and returns event credit', async () => {
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
        if (frame.channel === 2 && ['event_subscribe', 'event_unsubscribe'].includes(frame.payload.call)) {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        }
        if (frame.channel === 2 && frame.payload.call === 'workspace_info') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'workspace', with: { name: 'demo', image: 'alpine', architecture: 'amd64' } } }));
          socket.write(encode({ channel: 7, kind: KIND.event, payload: { snapshot: 'images', of: [] } }));
        }
      }
    });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: { protocol: 1, peer: 'fixture', granted: ['workspace-read', 'image-read'] } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const pushed = [];
    const session = await connect({ path: socketPath });
    assert.deepEqual(session.granted, ['workspace-read', 'image-read']);
    const stop = await workspace(session).watchImages((images) => pushed.push(images));
    assert.equal((await workspace(session).info()).name, 'demo');
    await credit;
    assert.deepEqual(pushed, [[]]);
    assert(observed.some((frame) => frame.channel === CONTROL && frame.kind === KIND.response));
    assert(observed.some((frame) => frame.channel === 7 && frame.kind === KIND.credit && frame.payload === 1));
    await stop();
    assert(observed.some((frame) => frame.payload?.call === 'event_subscribe' && frame.payload.with.topic === 'images'));
    assert(observed.some((frame) => frame.payload?.call === 'event_unsubscribe' && frame.payload.with.topic === 'images'));
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix semantic action wait arms before authority and disposes after changed text', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-text-wait-'));
  const socketPath = path.join(directory, 'host.sock'); const calls = []; const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket); socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (frame.payload.call === 'event_subscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'pane_semantic_action') {
          socket.write(encode({ channel: 11, kind: KIND.event, payload: { snapshot: 'pane_changes', of: {
            slot: 'settings', kind: 'native', generation: 2, revision: 4, coalesced: 0,
          } } }));
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'pane_list') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'panes', with: { panes: [{
            slot: 'settings', generation: 2, revision: 4, kind: 'native', provider: null,
            tab: null, title: 'Settings', focused: true,
          }], truncated: false } } }));
        } else if (frame.payload.call === 'pane_semantic_read') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'semantics', with: {
            slot: 'settings', generation: 2, revision: 4, truncated: false, root: {
              id: 0, role: 'page', label: 'Done', value: null, disabled: false, destructive: false,
              actions: [], children: [],
            },
          } } }));
        } else if (frame.payload.call === 'event_unsubscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        }
      }
    });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: {
      protocol: 1, peer: 'action-wait', granted: ['pane-observe', 'pane-semantic-read', 'pane-semantic-control'],
    } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).terminal.actAndWait('settings', {
      generation: 2, revision: 3, node: 7, action: 'invoke',
    });
    assert.equal(result.changed, true); assert.match(result.readable.text, /<label>Done<\/label>/);
    assert.deepEqual(calls, ['event_subscribe', 'pane_semantic_action', 'pane_list', 'pane_semantic_read', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix acquisition wait filters its cursor and disposes after authoritative status', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-acquisition-wait-'));
  const socketPath = path.join(directory, 'host.sock'); const calls = []; const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket); socket.on('close', () => connections.delete(socket)); const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue; calls.push(frame.payload.call);
        if (frame.payload.call === 'event_subscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
          socket.write(encode({ channel: 12, kind: KIND.event, payload: { snapshot: 'extension_acquisitions', of: {
            job: 'job-7', revision: 4, state: 'pulling', coalesced: 0,
          } } }));
          socket.write(encode({ channel: 12, kind: KIND.event, payload: { snapshot: 'extension_acquisitions', of: {
            job: 'job-7', revision: 5, state: 'ready', coalesced: 1,
          } } }));
        } else if (frame.payload.call === 'extension_acquisition_status') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'extension_acquisition', with: {
            job: 'job-7', reference: 'registry/demo:1', revision: 5, state: 'ready',
            progress: null, candidate: null, error: null,
          } } }));
        } else if (frame.payload.call === 'event_unsubscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        }
      }
    });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: {
      protocol: 1, peer: 'acquisition-wait', granted: ['extension-install'],
    } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).extensions.waitForAcquisition('job-7', 4);
    assert.equal(result.changed, true); assert.equal(result.status.revision, 5);
    assert.deepEqual(calls, ['event_subscribe', 'extension_acquisition_status', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix occupant switch arms before CAS and verifies provider inventory', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-switch-wait-'));
  const socketPath = path.join(directory, 'host.sock'); const calls = []; const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket); socket.on('close', () => connections.delete(socket)); const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue; calls.push(frame.payload.call);
        if (frame.payload.call === 'event_subscribe' || frame.payload.call === 'event_unsubscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'terminal_switch_occupant_observed') {
          socket.write(encode({ channel: 13, kind: KIND.event, payload: { snapshot: 'pane_changes', of: {
            slot: 'pane-1', kind: 'surface', generation: 8, revision: 12, coalesced: 0,
          } } }));
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        } else if (frame.payload.call === 'pane_list') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'panes', with: { panes: [{
            slot: 'pane-1', generation: 8, revision: 12, kind: 'surface',
            provider: { extension: 'manager', provider: 'main' }, tab: 'tab', title: 'Manager', focused: true,
          }], truncated: false } } }));
        }
      }
    });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: {
      protocol: 1, peer: 'switch-wait', granted: ['pane-observe', 'terminal-control'],
    } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).terminal.switchOccupantAndWait('pane-1', 7, 11, {
      kind: 'surface', extension: 'manager', provider: 'main',
    });
    assert.equal(result.changed, true); assert.equal(result.pane.provider.extension, 'manager');
    assert.deepEqual(calls, ['event_subscribe', 'terminal_switch_occupant_observed', 'pane_list', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix extension enable arms inventory before digest-bound authority', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-enable-wait-'));
  const socketPath = path.join(directory, 'host.sock'); const calls = []; const connections = new Set();
  const digest = `sha256:${'b'.repeat(64)}`;
  const server = net.createServer((socket) => {
    connections.add(socket); socket.on('close', () => connections.delete(socket)); const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue; calls.push(frame.payload.call);
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        if (frame.payload.call === 'extension_enable') socket.write(encode({ channel: 14, kind: KIND.event, payload: {
          snapshot: 'extensions', of: [{ name: 'manager', image_digest: digest, version: '1', status: 'duty', enabled: true, pane_providers: [] }],
        } }));
      }
    });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: {
      protocol: 1, peer: 'enable-wait', granted: ['extension-read', 'extension-control'],
    } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).extensions.enableAndWait('manager', digest);
    assert.equal(result.changed, true); assert.deepEqual(calls, ['event_subscribe', 'extension_enable', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve)); await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix extension disable arms inventory before digest-bound authority', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-disable-wait-'));
  const socketPath = path.join(directory, 'host.sock'); const calls = []; const connections = new Set();
  const digest = `sha256:${'d'.repeat(64)}`;
  const server = net.createServer((socket) => {
    connections.add(socket); socket.on('close', () => connections.delete(socket)); const reader = new Reader();
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.channel !== 2) continue; calls.push(frame.payload.call);
      socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      if (frame.payload.call === 'extension_disable') socket.write(encode({ channel: 15, kind: KIND.event, payload: {
        snapshot: 'extensions', of: [{ name: 'manager', image_digest: digest, version: '1', status: 'standby', enabled: false, pane_providers: [] }],
      } }));
    } });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: {
      protocol: 1, peer: 'disable-wait', granted: ['extension-read', 'extension-control'],
    } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).extensions.disableAndWait('manager', digest);
    assert.equal(result.changed, true); assert.deepEqual(calls, ['event_subscribe', 'extension_disable', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve)); await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix extension remove arms inventory before authority and observes absence', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-remove-wait-'));
  const socketPath = path.join(directory, 'host.sock'); const calls = []; const connections = new Set();
  const digest = `sha256:${'e'.repeat(64)}`;
  const server = net.createServer((socket) => {
    connections.add(socket); socket.on('close', () => connections.delete(socket)); const reader = new Reader();
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.channel !== 2) continue; calls.push(frame.payload.call);
      socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      if (frame.payload.call === 'extension_remove') socket.write(encode({ channel: 16, kind: KIND.event, payload: {
        snapshot: 'extensions', of: [],
      } }));
    } });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: {
      protocol: 1, peer: 'remove-wait', granted: ['extension-read', 'extension-control'],
    } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).extensions.removeAndWait('manager', digest);
    assert.deepEqual(result, { changed: true, removed: { name: 'manager', image_digest: digest }, replacement: null });
    assert.deepEqual(calls, ['event_subscribe', 'extension_remove', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve)); await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix extension retry arms inventory before digest-bound authority', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-retry-wait-'));
  const socketPath = path.join(directory, 'host.sock'); const calls = []; const connections = new Set();
  const digest = `sha256:${'f'.repeat(64)}`;
  const server = net.createServer((socket) => {
    connections.add(socket); socket.on('close', () => connections.delete(socket)); const reader = new Reader();
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.channel !== 2) continue; calls.push(frame.payload.call);
      socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      if (frame.payload.call === 'extension_retry') socket.write(encode({ channel: 17, kind: KIND.event, payload: {
        snapshot: 'extensions', of: [{ name: 'manager', image_digest: digest, version: '1', status: 'duty', enabled: true, pane_providers: [] }],
      } }));
    } });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: {
      protocol: 1, peer: 'retry-wait', granted: ['extension-read', 'extension-control'],
    } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).extensions.retryAndWait('manager', digest);
    assert.equal(result.changed, true); assert.deepEqual(calls, ['event_subscribe', 'extension_retry', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve)); await rm(directory, { recursive: true, force: true });
  }
});

test('negotiated grants are immutable and deny calls and topics before any socket write', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-grants-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket); socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2) continue;
        calls.push(frame.payload);
        socket.write(encode({ channel: 2, kind: KIND.response, payload: {
          reply: 'workspace', with: { name: 'demo', image: 'alpine', architecture: 'amd64' },
        } }));
      }
    });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: {
      protocol: 1, peer: 'grants', granted: ['workspace-read'],
    } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    assert.deepEqual(session.grantedCapabilities, ['workspace-read']);
    assert(Object.isFrozen(session.grantedCapabilities));
    assert.throws(() => session.grantedCapabilities.push('container-read'), TypeError);
    await assert.rejects(session.call('container_list'), (error) => error instanceof Error
      && error.name === 'ExtensionError' && error.kind === 'denied' && error.capability === 'container-read');
    await assert.rejects(session.call('event_subscribe', { topic: 'containers' }), /container-read/);
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(calls, [], 'locally denied authority writes no request frame');
    assert.equal((await session.call('workspace_info')).reply, 'workspace');
    assert.deepEqual(calls, [{ call: 'workspace_info' }], 'a negotiated authority still reaches the host');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('AbortSignal writes nothing before a call and closes ordered Unix calls after write', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-abort-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const delivered = [];
  let peerClosed;
  let peer;
  let observedTwo;
  const closed = new Promise((resolve) => { peerClosed = resolve; });
  const twoCalls = new Promise((resolve) => { observedTwo = resolve; });
  const server = net.createServer((socket) => {
    peer = socket;
    socket.on('error', () => {});
    socket.on('close', peerClosed);
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel === 2) {
          calls.push(frame.payload);
          if (calls.length === 2) observedTwo();
        }
      }
    });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: {
      protocol: 1, peer: 'abort', granted: ['workspace-read'],
    } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath, onReply: (reply) => delivered.push(reply) });
    const before = new AbortController(); before.abort('not sent');
    await assert.rejects(session.call('workspace_info', undefined, { signal: before.signal }), { name: 'AbortError' });
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(calls, [], 'an already-aborted call emits no protocol frame');

    const firstAbort = new AbortController();
    const first = session.call('workspace_info', undefined, { signal: firstAbort.signal });
    const second = session.call('workspace_list');
    await twoCalls;
    firstAbort.abort('stop');
    // A host racing cancellation may still try to answer the written calls;
    // the destroyed stream must not deliver either answer to another caller.
    for (const payload of [
      { reply: 'workspace', with: { name: 'demo', image: 'alpine', architecture: 'amd64' } },
      { reply: 'workspaces', with: [] },
    ]) peer.write(encode({ channel: 2, kind: KIND.response, payload }));
    await assert.rejects(first, { name: 'AbortError' });
    await assert.rejects(second, { name: 'AbortError' });
    await closed;
    assert.deepEqual(calls.map(({ call }) => call), ['workspace_info', 'workspace_list']);
    assert.deepEqual(delivered, [], 'no reply can be rebound after cancellation closes the stream');
    await assert.rejects(session.call('workspace_info'), /closed/);
  } finally {
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
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: { protocol: 1, peer: 'control', granted: ['workspace-read'] } }));
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
  host.write(encode({ channel: CONTROL, kind: KIND.open, payload: { protocol: 1, peer: 'pressure', granted: ['workspace-read'] } }));
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
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: { protocol: 1, peer: 'channel', granted: ['workspace-read'] } }));
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

test('real Unix install wait inspects revision, arms inventory, then commits exact candidate', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-install-wait-'));
  const socketPath = path.join(directory, 'host.sock'); const calls = []; const connections = new Set();
  const digest = `sha256:${'a'.repeat(64)}`;
  const candidate = { name: 'sample', version: '1', image_digest: digest, requested: ['extension-read'], installed_image_digest: null };
  const summary = { name: 'sample', image_digest: digest, version: '1', status: 'standby', enabled: false, pane_providers: [] };
  const server = net.createServer((socket) => {
    connections.add(socket); socket.on('close', () => connections.delete(socket)); const reader = new Reader();
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.channel !== 2) continue; calls.push(frame.payload.call);
      if (frame.payload.call === 'extension_acquisition_status') socket.write(encode({ channel: 2, kind: KIND.response, payload: {
        reply: 'extension_acquisition', with: { job: 'job-1', reference: 'sample:1', revision: 7, state: 'ready', progress: null, candidate, error: null },
      } }));
      else if (frame.payload.call === 'extension_install') {
        socket.write(encode({ channel: 21, kind: KIND.event, payload: { snapshot: 'extensions', of: [summary] } }));
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'extension', with: summary } }));
      } else socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
    } });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: {
      protocol: 1, peer: 'install-wait', granted: ['extension-read', 'extension-install'],
    } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).extensions.installAndWait('job-1', 7, ['extension-read']);
    assert.equal(result.changed, true);
    assert.deepEqual(calls, ['extension_acquisition_status', 'event_subscribe', 'extension_install', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve)); await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix container start wait arms first and ignores unchanged initial state', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-container-start-wait-'));
  const socketPath = path.join(directory, 'host.sock'); const calls = []; const connections = new Set();
  const id = 'a'.repeat(32);
  const summary = (state) => ({ id, name: 'agent', image: 'alpine:3.20', state, created: 1 });
  const server = net.createServer((socket) => {
    connections.add(socket); socket.on('close', () => connections.delete(socket)); const reader = new Reader();
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.channel !== 2) continue; calls.push(frame.payload.call);
      if (frame.payload.call === 'event_subscribe') {
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        setImmediate(() => socket.write(encode({ channel: 31, kind: KIND.event, payload: { snapshot: 'containers', of: [summary('created')] } })));
      } else if (frame.payload.call === 'container_start') {
        socket.write(encode({ channel: 32, kind: KIND.event, payload: { snapshot: 'containers', of: [summary('created')] } }));
        socket.write(encode({ channel: 33, kind: KIND.event, payload: { snapshot: 'containers', of: [summary('running')] } }));
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      } else socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
    } });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: {
      protocol: 1, peer: 'container-start-wait', granted: ['container-read', 'container-control'],
    } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).containers.startAndWait(id);
    assert.equal(result.changed, true); assert.equal(result.container.state, 'running');
    assert.deepEqual(calls, ['event_subscribe', 'container_start', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve)); await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix container stop wait arms first and ignores unchanged running state', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-container-stop-wait-'));
  const socketPath = path.join(directory, 'host.sock'); const calls = []; const connections = new Set();
  const id = 'b'.repeat(64);
  const summary = (state) => ({ id, name: 'agent', image: 'alpine:3.20', state, created: 1 });
  const server = net.createServer((socket) => {
    connections.add(socket); socket.on('close', () => connections.delete(socket)); const reader = new Reader();
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.channel !== 2) continue; calls.push(frame.payload.call);
      if (frame.payload.call === 'event_subscribe') {
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        setImmediate(() => socket.write(encode({ channel: 34, kind: KIND.event, payload: { snapshot: 'containers', of: [summary('running')] } })));
      } else if (frame.payload.call === 'container_stop') {
        socket.write(encode({ channel: 35, kind: KIND.event, payload: { snapshot: 'containers', of: [summary('running')] } }));
        socket.write(encode({ channel: 36, kind: KIND.event, payload: { snapshot: 'containers', of: [summary('exited')] } }));
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      } else socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
    } });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: {
      protocol: 1, peer: 'container-stop-wait', granted: ['container-read', 'container-control'],
    } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).containers.stopAndWait(id);
    assert.equal(result.changed, true); assert.equal(result.container.state, 'exited');
    assert.deepEqual(calls, ['event_subscribe', 'container_stop', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve)); await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix container remove wait rejects incomplete absence then accepts complete absence', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-container-remove-wait-'));
  const socketPath = path.join(directory, 'host.sock'); const calls = []; const connections = new Set(); let completeAbsenceSent = false;
  const id = 'c'.repeat(64); const summary = { id, name: 'agent', image: 'alpine', state: 'exited', created: 1 };
  const server = net.createServer((socket) => {
    connections.add(socket); socket.on('close', () => connections.delete(socket)); const reader = new Reader();
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.channel !== 2) continue; calls.push(frame.payload.call);
      socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      if (frame.payload.call === 'container_remove') {
        setImmediate(() => {
          socket.write(encode({ channel: 40, kind: KIND.event, payload: { snapshot: 'container_inventory', of: { containers: [], complete: false } } }));
          setImmediate(() => {
            socket.write(encode({ channel: 41, kind: KIND.event, payload: { snapshot: 'container_inventory', of: { containers: [summary], complete: true } } }));
            setTimeout(() => {
              completeAbsenceSent = true;
              socket.write(encode({ channel: 42, kind: KIND.event, payload: { snapshot: 'container_inventory', of: { containers: [], complete: true } } }));
            }, 20);
          });
        });
      }
    } });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: { protocol: 1, peer: 'remove-wait', granted: ['container-read', 'container-control'] } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    assert.deepEqual(await workspace(session).containers.removeAndWait(id), { changed: true, id });
    assert.equal(completeAbsenceSent, true, 'incomplete absence cannot settle removal');
    assert.deepEqual(calls, ['event_subscribe', 'container_remove', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve)); await rm(directory, { recursive: true, force: true });
  }
});

test('real Unix restart wait requires the same container at a newer running generation', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-container-restart-wait-'));
  const socketPath = path.join(directory, 'host.sock'); const calls = []; const connections = new Set(); const id = 'd'.repeat(64);
  const summary = (state, generation) => ({ id, name: 'agent', image: 'alpine', state, created: 1, generation });
  const server = net.createServer((socket) => {
    connections.add(socket); socket.on('close', () => connections.delete(socket)); const reader = new Reader();
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.channel !== 2) continue; calls.push(frame.payload.call);
      socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      if (frame.payload.call === 'container_restart') {
        socket.write(encode({ channel: 45, kind: KIND.event, payload: { snapshot: 'containers', of: [summary('running', 7)] } }));
        socket.write(encode({ channel: 46, kind: KIND.event, payload: { snapshot: 'containers', of: [summary('exited', 8)] } }));
        socket.write(encode({ channel: 47, kind: KIND.event, payload: { snapshot: 'containers', of: [summary('running', 8)] } }));
      }
    } });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: { protocol: 1, peer: 'restart-wait', granted: ['container-read', 'container-control'] } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const result = await workspace(session).containers.restartAndWait(id, 7);
    assert.equal(result.changed, true); assert.equal(result.container.generation, 8); assert.equal(result.container.state, 'running');
    assert.deepEqual(calls, ['event_subscribe', 'container_restart', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve)); await rm(directory, { recursive: true, force: true });
  }
});
