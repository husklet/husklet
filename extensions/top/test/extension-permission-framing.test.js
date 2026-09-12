import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { createElement as h } from 'react';
import { connect, workspace } from '../../../extensions/base/react/dist/index.js';
import { KIND, Reader, encode } from '../../../extensions/base/react/dist/wire.js';
import { Extensions } from '../dist/app.js';
import { host } from './host.js';

test('persisted exact-file authority reaches Top unchanged over real Unix framing', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-extension-permission-'));
  const socketPath = join(directory, 'host.sock');
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.write(
      encode({
        channel: 0,
        kind: KIND.open,
        payload: {
          protocol: 1,
          extension: 'extension-permission-test',
          granted: ['extensions:read'],
        },
      }),
    );
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        const call = frame.payload?.call;
        if (!call) continue;
        const payload =
          call === 'extension_list'
            ? {
                reply: 'extensions',
                with: [
                  {
                    name: 'indexer',
                    image_digest: `sha256:${'a'.repeat(64)}`,
                    version: '1.0.0',
                    enabled: true,
                    status: 'duty',
                    pane_providers: [],
                    filesystem: {
                      read: [{ subtree: 'documents' }],
                      write: [{ exact: 'settings/index.json' }],
                      create: [],
                      delete: [],
                      rename: [],
                    },
                  },
                ],
              }
            : call === 'extension_catalogue'
              ? { reply: 'extension_catalogue', with: { entries: [], complete: true } }
              : { reply: 'done' };
        socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
      }
    });
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));

  let session;
  let stage;
  try {
    session = await connect({ path: socketPath });
    stage = host();
    stage.render(h(Extensions, { api: workspace(session) }));
    await until(() =>
      labelled(stage, 'Only this file · modify existing contents · settings/index.json'),
    );
    assert.ok(labelled(stage, 'This folder subtree · view contents · documents/'));
    assert.equal(
      labelled(stage, 'Modify existing contents folder · settings/ and everything inside'),
      undefined,
    );
  } finally {
    stage?.render(null);
    await new Promise((resolve) => setTimeout(resolve, 20));
    await session?.close();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

function labelled(stage, label) {
  return stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label)
    .at(-1);
}

async function until(done) {
  const deadline = Date.now() + 2_000;
  while (!done()) {
    if (Date.now() >= deadline) throw new Error('extension permission view did not settle');
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}
