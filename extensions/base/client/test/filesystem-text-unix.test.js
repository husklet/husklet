import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';

import { connect, workspace } from '../dist/index.js';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

test('readText decodes split UTF-8 over fragmented real Unix frames and enforces its bound', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-file-text-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  const requests = [];
  const documents = new Map([
    ['docs/good.txt', { identity: 'good-v1', bytes: [0x41, 0xe2, 0x82, 0xac, 0x42] }],
    ['docs/large.txt', { identity: 'large-v1', bytes: [0x61, 0x62, 0x63] }],
    ['docs/bad.txt', { identity: 'bad-v1', bytes: [0x61, 0xc3, 0x28] }],
  ]);
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', async (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        const input = frame.payload.with;
        requests.push(input);
        const document = documents.get(input.path);
        const contents = document.bytes.slice(input.offset, input.offset + input.limit);
        const eof = input.offset + contents.length >= document.bytes.length;
        const reply = encode({
          channel: frame.channel,
          kind: KIND.response,
          payload: {
            reply: 'file_range',
            with: {
              path: input.path,
              identity: document.identity,
              offset: input.offset,
              total: document.bytes.length,
              contents,
              eof,
              truncated: !eof,
            },
          },
        });
        // Exercise header, multibyte JSON, and payload boundaries rather than
        // relying on the kernel to happen to fragment a local write.
        for (const byte of reply) socket.write(Uint8Array.of(byte));
      }
    });
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: { protocol: 1, peer: 'file-text', granted: ['filesystem:read'] },
    });
    socket.write(greeting.subarray(0, 5));
    socket.write(greeting.subarray(5));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const session = await connect({ path: socketPath });
    const files = workspace(session).files;
    assert.deepEqual(
      await files.readText('docs/good.txt', {
        maxBytes: 5,
        chunkBytes: 2,
        observed: 'good-v1',
      }),
      { text: 'A€B', identity: 'good-v1', bytes: 5 },
    );
    assert.deepEqual(
      requests.slice(0, 3).map(({ offset, limit, observed }) => ({ offset, limit, observed })),
      [
        { offset: 0, limit: 2, observed: 'good-v1' },
        { offset: 2, limit: 2, observed: 'good-v1' },
        { offset: 4, limit: 2, observed: 'good-v1' },
      ],
    );

    const beforeLarge = requests.length;
    await assert.rejects(
      files.readText('docs/large.txt', { maxBytes: 2, chunkBytes: 1 }),
      /exceeds the caller's 2 byte limit/,
    );
    assert.equal(requests.length, beforeLarge + 1, 'reported total stops the read after one page');

    await assert.rejects(
      files.readText('docs/bad.txt', { maxBytes: 3, chunkBytes: 1 }),
      /not valid UTF-8/,
    );
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('readText rejects invalid bounds before writing to the transport', async () => {
  const calls = [];
  const files = workspace({
    granted: [],
    onEvent() {
      return () => false;
    },
    async call(name, argument) {
      calls.push({ name, argument });
      throw new Error('must not be called');
    },
  }).files;

  for (const maxBytes of [0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, 64 * 1024 * 1024 + 1]) {
    await assert.rejects(files.readText('docs/a.txt', { maxBytes }), /maxBytes/);
  }
  await assert.rejects(
    files.readText('docs/a.txt', { maxBytes: 4, chunkBytes: 65_537 }),
    /filesystem range limit/,
  );
  assert.deepEqual(calls, []);
});
