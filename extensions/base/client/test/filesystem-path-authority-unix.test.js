import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';

import { connect, workspace } from '../dist/index.js';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

test('invalid workspace paths never cross a fragmented Unix connection and the session stays reusable', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-path-authority-'));
  const socketPath = path.join(directory, 'host.sock');
  const requests = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        requests.push(frame.payload);
        const reply = encode({
          channel: frame.channel,
          kind: KIND.response,
          payload: {
            reply: 'entry',
            with: { path: frame.payload.with.path, directory: false, size: 4, identity: 'v1' },
          },
        });
        for (let offset = 0; offset < reply.length; offset += 3)
          socket.write(reply.subarray(offset, offset + 3));
      }
    });
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: { protocol: 1, peer: 'path-authority', granted: ['filesystem:read'] },
    });
    for (let offset = 0; offset < greeting.length; offset += 2)
      socket.write(greeting.subarray(offset, offset + 2));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const files = workspace(session).files;
    await assert.rejects(files.stat('../secret'), /parent traversal/);
    await assert.rejects(files.stat('/etc/passwd'), /must be relative/);
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(requests.length, 0, 'invalid names are rejected before framing');
    assert.equal((await files.stat('scope/index.ts')).path, 'scope/index.ts');
    assert.equal(requests.length, 1, 'a valid in-scope request still uses the same session');
    session.close();
  } finally {
    for (const socket of connections) socket.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
