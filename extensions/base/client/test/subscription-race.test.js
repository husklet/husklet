import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';

import { connect, workspace } from '../dist/index.js';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

test('failed pending subscription cannot retire a later consumer released concurrently', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-subscription-race-'));
  const socketPath = path.join(directory, 'host.sock');
  const calls = [];
  const connections = new Set();
  let subscribedSocket;
  let rejectFirst;
  let firstRequestSeen;
  const firstRequest = new Promise((resolve) => {
    firstRequestSeen = resolve;
  });
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request || frame.channel !== 2) continue;
        calls.push(frame.payload.call);
        if (frame.payload.call === 'event_subscribe') {
          const first = calls.filter((call) => call === 'event_subscribe').length === 1;
          if (first) {
            rejectFirst = () =>
              socket.write(
                encode({
                  channel: 2,
                  kind: KIND.response,
                  flags: 3,
                  payload: { error: 'failed', detail: 'transient subscription failure' },
                }),
              );
            firstRequestSeen();
          } else {
            socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
            subscribedSocket = socket;
          }
        } else if (frame.payload.call === 'event_unsubscribe') {
          socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        }
      }
    });
    socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.open,
        payload: { protocol: 1, peer: 'subscription-race', granted: ['images:read'] },
      }),
    );
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const delivered = [];
    let eventSeen;
    const event = new Promise((resolve) => {
      eventSeen = resolve;
    });
    const session = await connect({ path: socketPath });
    const host = workspace(session);
    session.onEvent((received) => {
      delivered.push(received);
      eventSeen();
    });

    const failed = host.subscribe('images');
    await firstRequest;
    const surviving = host.subscribe('images');
    const released = host.unsubscribe('images');
    rejectFirst();
    const outcomes = await Promise.allSettled([failed, surviving, released]);
    assert.equal(outcomes[0].status, 'rejected');
    assert.match(outcomes[0].reason.message, /transient subscription failure/);
    assert.deepEqual(
      outcomes.slice(1).map(({ status }) => status),
      ['fulfilled', 'fulfilled'],
    );

    subscribedSocket.write(
      encode({
        channel: 7,
        kind: KIND.event,
        payload: { snapshot: 'images', of: { images: [], truncated: false } },
      }),
    );
    await Promise.race([
      event,
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error('surviving subscription received no event')), 200),
      ),
    ]);
    assert.deepEqual(delivered, [{ snapshot: 'images', of: { images: [], truncated: false } }]);

    await host.unsubscribe('images');
    assert.deepEqual(calls, ['event_subscribe', 'event_subscribe', 'event_unsubscribe']);
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});
