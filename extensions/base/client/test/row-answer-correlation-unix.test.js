import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';

import { connect, RowReplyMismatchError, RowRequestUnavailableError } from '../dist/index.js';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

test('row answers retain exact request correlation over fragmented Unix framing', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-row-correlation-'));
  const socketPath = path.join(directory, 'host.sock');
  const connections = new Set();
  const received = [];
  let issue;
  let issueTimer;
  const issued = new Promise((resolve) => {
    issue = resolve;
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        received.push(frame);
        if (frame.kind === KIND.request && frame.payload.call === 'workspace_info') {
          socket.write(
            encode({
              channel: frame.channel,
              kind: KIND.response,
              payload: {
                reply: 'workspace',
                with: { name: 'postgres', image: 'alpine', architecture: 'amd64' },
              },
            }),
          );
        }
      }
    });
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: { protocol: 1, peer: 'rows', granted: ['workspaces:read'] },
    });
    socket.write(greeting.subarray(0, 3));
    socket.write(greeting.subarray(3));
    setImmediate(() => {
      const request = encode({
        channel: 41,
        kind: KIND.event,
        payload: {
          id: 7,
          source: 3,
          version: 11,
          range: { start: 128, count: 4 },
          sort: null,
          filter: null,
        },
      });
      for (const byte of request) socket.write(Uint8Array.of(byte));
    });
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    let row;
    const session = await connect({
      path: socketPath,
      timeout: 500,
      connectTimeout: 500,
      onRows(request, channel) {
        row = { request, channel };
        issue();
      },
    });
    await Promise.race([
      issued,
      new Promise((_, reject) => {
        issueTimer = setTimeout(() => reject(new Error('host row request was not delivered')), 500);
      }),
    ]);
    clearTimeout(issueTimer);
    const exact = {
      source: 3,
      version: 11,
      request: 7,
      range: { start: 128, count: 4 },
      rows: [],
    };
    assert.throws(
      () => session.answer(41, { ...exact, request: 8 }),
      (error) => {
        assert(error instanceof RowReplyMismatchError);
        assert.equal(error.channel, 41);
        assert.deepEqual(error.request, {
          id: 7,
          source: 3,
          version: 11,
          range: { start: 128, count: 4 },
        });
        return true;
      },
    );
    assert.equal(
      received.some((frame) => frame.channel === 41),
      false,
    );
    assert.deepEqual(row, {
      request: {
        id: 7,
        source: 3,
        version: 11,
        range: { start: 128, count: 4 },
        sort: null,
        filter: null,
      },
      channel: 41,
    });
    session.answer(41, exact);
    assert.throws(() => session.answer(41, exact), RowRequestUnavailableError);
    assert.equal((await session.call('workspace_info')).with.name, 'postgres');
    await session.close();
  } finally {
    clearTimeout(issueTimer);
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
