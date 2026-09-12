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

test('installed extension detects a republished same-version image over real Unix framing', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-extension-update-'));
  const socketPath = join(directory, 'host.sock');
  const oldDigest = `sha256:${'a'.repeat(64)}`;
  const nextDigest = `sha256:${'b'.repeat(64)}`;
  const reference = 'registry.example/storybook:2';
  const calls = [];
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.write(
      encode({
        channel: 0,
        kind: KIND.open,
        payload: {
          protocol: 1,
          extension: 'extension-update-test',
          granted: ['extensions:read', 'extensions:install'],
        },
      }),
    );
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        const call = frame.payload?.call;
        if (!call) continue;
        calls.push(frame.payload);
        const payload =
          call === 'extension_list'
            ? {
                reply: 'extensions',
                with: [
                  {
                    name: 'storybook',
                    image_digest: oldDigest,
                    version: '2.0.0',
                    enabled: true,
                    status: 'duty',
                    pane_providers: [],
                  },
                ],
              }
            : call === 'extension_catalogue'
              ? {
                  reply: 'extension_catalogue',
                  with: {
                    entries: [
                      {
                        id: 'storybook',
                        title: 'Component playground',
                        description: 'Inspect components.',
                        version: '2.0.0',
                        reference,
                        publisher: 'Husklet',
                        source: 'husklet:first-party/storybook',
                        publisher_verified: true,
                      },
                    ],
                    complete: true,
                  },
                }
              : call === 'extension_acquisition_start'
                ? { reply: 'extension_acquisition_job', with: { job: 'update-job' } }
                : call === 'extension_acquisition_status'
                  ? {
                      reply: 'extension_acquisition',
                      with: {
                        job: 'update-job',
                        reference,
                        revision: 4,
                        state: 'ready',
                        progress: null,
                        candidate: {
                          name: 'storybook',
                          version: '2.0.0',
                          image_digest: nextDigest,
                          installed_image_digest: oldDigest,
                          requested: ['interface:render'],
                          required: ['interface:render'],
                        },
                        error: null,
                      },
                    }
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
    await until(() => labelled(stage, 'Check for changes'));
    invokeByTooltip(stage, 'Check storybook image for changes');
    await until(() => labelled(stage, 'Update with selected access'));
    await until(() =>
      labelled(
        stage,
        'Required to keep this extension available after the update: Render this extension interface. Select it below to continue.',
      ),
    );
    assert.deepEqual(
      calls.filter(({ call }) => call.startsWith('extension_acquisition')),
      [
        { call: 'extension_acquisition_start', with: { reference } },
        { call: 'extension_acquisition_status', with: { job: 'update-job' } },
      ],
    );
    assert.ok(
      labelled(
        stage,
        `Image changes from sha256:${'a'.repeat(12)}…${'a'.repeat(8)}; access has been reset.`,
      ),
    );
    assert.ok(labelled(stage, 'Verified publisher · Husklet'));
    assert.ok(labelled(stage, 'Catalogue source · husklet:first-party/storybook'));
    assert.equal(
      labelled(stage, 'Direct OCI image · no catalogue publisher verification.'),
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

function invokeByTooltip(stage, tooltip) {
  const nodes = stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Tooltip' && patch.SetProp.value?.Text === tooltip)
    .map((patch) => patch.SetProp.id)
    .reverse();
  assert.ok(
    nodes.some((node) =>
      stage.surface.dispatch({ trigger: 'Invoke', node, id: `${node}:Invoke`, value: null }),
    ),
  );
}

async function until(done) {
  const deadline = Date.now() + 2_000;
  while (!done()) {
    if (Date.now() >= deadline) throw new Error('extension update flow did not settle');
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}
