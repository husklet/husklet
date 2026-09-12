import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';

import { connect } from '../dist/index.js';
import { CONTROL, KIND, encode } from '../dist/wire.js';

test('fragmented greeting exposes only the caller filesystem grant as immutable selectors', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-grant-'));
  const socketPath = path.join(directory, 'host.sock');
  const sockets = new Set();
  const server = net.createServer((socket) => {
    sockets.add(socket);
    socket.on('close', () => sockets.delete(socket));
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'indexer',
        granted: ['filesystem:read', 'filesystem:write'],
        filesystem: {
          read: [{ subtree: 'src' }],
          write: [{ exact: 'state/index.json' }],
        },
      },
    });
    for (const byte of greeting) socket.write(Uint8Array.of(byte));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    assert.deepEqual(session.grantedFilesystem, {
      read: [{ subtree: 'src' }],
      write: [{ exact: 'state/index.json' }],
      create: [],
      delete: [],
      rename: [],
    });
    assert(Object.isFrozen(session.grantedFilesystem));
    assert(Object.isFrozen(session.grantedFilesystem.read));
    assert(Object.isFrozen(session.grantedFilesystem.read[0]));
    assert.throws(() => session.grantedFilesystem.read.push({ subtree: 'secret' }), TypeError);
    session.close();
  } finally {
    for (const socket of sockets) socket.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
