import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';

import { connect, DirectoryIdentityChangedError, workspace } from '../dist/index.js';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

test('a fragmented stale directory page carries restart identity and preserves the session', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-directory-identity-'));
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
          frame.payload.call === 'filesystem_list_page'
            ? {
                reply: 'directory_page',
                with: {
                  entries: [{ path: 'src/b.ts', directory: false, size: 3 }],
                  identity: 'directory-v2',
                  next: 'src/b.ts',
                  more: false,
                },
              }
            : {
                reply: 'workspace',
                with: { name: 'indexer', image: 'alpine', architecture: 'amd64' },
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
        peer: 'directory-identity',
        granted: ['filesystem:read', 'workspaces:read'],
      },
    });
    socket.write(greeting.subarray(0, 2));
    socket.write(greeting.subarray(2));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const host = workspace(session);
    await assert.rejects(
      host.files.listPage('src', {
        after: 'src/a.ts',
        observed: 'directory-v1',
        limit: 8,
      }),
      (error) => {
        assert(error instanceof DirectoryIdentityChangedError);
        assert.equal(error.path, 'src');
        assert.equal(error.expected, 'directory-v1');
        assert.equal(error.actual, 'directory-v2');
        assert.equal(error.after, 'src/a.ts');
        return true;
      },
    );
    assert.deepEqual(calls[0].with, {
      path: 'src',
      after: 'src/a.ts',
      observed: 'directory-v1',
      limit: 8,
    });
    assert.equal((await host.info()).name, 'indexer');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
