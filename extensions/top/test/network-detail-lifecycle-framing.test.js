import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { createElement as h } from 'react';
import { connect, workspace } from '../../../extensions/base/react/dist/index.js';
import { KIND, Reader, encode } from '../../../extensions/base/react/dist/wire.js';
import { Top } from '../dist/app.js';
import { host } from './host.js';

test(
  'authoritative network replacement invalidates detail and identity consent',
  { timeout: 5_000 },
  async () => {
    const directory = await mkdtemp(join(tmpdir(), 'husklet-network-detail-lifecycle-'));
    const socketPath = join(directory, 'host.sock');
    const id = 'a'.repeat(32);
    const container = 'c'.repeat(32);
    let lists = 0;
    let inspections = 0;
    const server = net.createServer((socket) => {
      const reader = new Reader();
      socket.write(
        encode({
          channel: 0,
          kind: KIND.open,
          payload: {
            protocol: 1,
            extension: 'network-detail-lifecycle-test',
            granted: [
              'networks:read',
              'networks:create',
              'networks:remove',
              'networks:connect',
              'networks:disconnect',
              'networks:publish',
            ],
          },
        }),
      );
      socket.on('data', (chunk) => {
        for (const frame of reader.take(chunk)) {
          const call = frame.payload?.call;
          if (!call) continue;
          let payload = { reply: 'done' };
          if (call === 'network_list') {
            lists += 1;
            payload = {
              reply: 'networks',
              with: {
                networks: [
                  {
                    id,
                    name: lists === 1 ? 'old-net' : 'new-net',
                    driver: 'bridge',
                    scope: 'local',
                    kind: 'custom',
                  },
                ],
                truncated: false,
              },
            };
          } else if (call === 'network_inspect') {
            inspections += 1;
            payload = {
              reply: 'network',
              with: {
                id,
                name: inspections === 1 ? 'old-net' : 'new-net',
                driver: 'bridge',
                scope: inspections === 1 ? 'old-scope' : 'new-scope',
                kind: 'custom',
                endpoints: { containers: [container], truncated: false },
              },
            };
          }
          const delay = call === 'network_inspect' && inspections === 1 ? 150 : 20;
          setTimeout(
            () =>
              socket.write(
                encode({ channel: frame.channel, kind: KIND.response, flags: 1, payload }),
              ),
            delay,
          );
        }
      });
    });
    await new Promise((resolve, reject) =>
      server.listen(socketPath, (error) => (error ? reject(error) : resolve())),
    );
    let session;
    let inspectionSession;
    let stage;
    try {
      session = await connect({ path: socketPath });
      inspectionSession = await connect({ path: socketPath });
      const framed = workspace(session);
      const inspectionApi = workspace(inspectionSession);
      stage = host();
      stage.render(
        h(Top, {
          api: {
            ...framed,
            networks: { ...framed.networks, inspect: inspectionApi.networks.inspect },
            subscribe: undefined,
            unsubscribe: undefined,
          },
          initial: {
            containers: [
              {
                id: container,
                name: 'worker',
                image: 'alpine:3.20',
                state: 'exited',
                created: 1,
                generation: 1,
              },
            ],
            executions: [],
            images: [],
            volumes: [],
          },
        }),
      );
      invoke(stage, 'Networks');
      await until(() => labelled(stage, 'old-net'));
      invoke(stage, 'Manage connections');
      await until(() => inspections === 1 && labelled(stage, 'Network details'));
      choose(stage, container);
      await until(() => labelled(stage, 'Disconnect'));
      invoke(stage, 'Remove');
      assert.ok(labelled(stage, `Remove immutable network ${id} (old-net)?`));
      const start = stage.frames.length;
      invoke(stage, 'Refresh');
      await until(
        () => labelled(stage, 'new-net'),
        () => `lists=${lists}`,
      );
      const patches = stage.frames.slice(start).flatMap((frame) => frame.patches);
      assert.ok(patches.some((patch) => 'Remove' in patch));
      for (const stale of [`Remove immutable network ${id} (old-net)?`, 'old-scope'])
        assert.equal(
          patches.some((patch) => patch.SetProp?.value?.Text === stale),
          false,
          `${stale} does not remount`,
        );
      await new Promise((resolve) => setTimeout(resolve, 170));
      invoke(stage, 'Manage connections');
      await until(() => inspections === 2 && labelled(stage, 'Scope · new-scope'));
      assert.ok(labelled(stage, 'Scope · new-scope'));
      await until(() => labelled(stage, 'Disconnect'));
      invoke(stage, 'Disconnect');
      assert.ok(labelled(stage, `Disconnect immutable container ${container} from network ${id}?`));
      assert.equal(inspections, 2);
    } finally {
      stage?.render(null);
      await new Promise((r) => setTimeout(r, 30));
      session?.close();
      inspectionSession?.close();
      await new Promise((r) => server.close(r));
      await rm(directory, { recursive: true, force: true });
    }
  },
);

function labelled(stage, label) {
  return stage.frames
    .flatMap((f) => f.patches)
    .filter((p) => p.SetProp?.prop === 'Label' && p.SetProp.value?.Text === label)
    .at(-1);
}
function invoke(stage, label) {
  const nodes = stage.frames
    .flatMap((f) => f.patches)
    .filter((p) => p.SetProp?.prop === 'Label' && p.SetProp.value?.Text === label)
    .map((p) => p.SetProp.id)
    .reverse();
  assert.ok(
    nodes.some((node) =>
      stage.surface.dispatch({ trigger: 'Invoke', node, id: `${node}:Invoke`, value: null }),
    ),
    `${label} invokes`,
  );
}
function choose(stage, value) {
  const node = stage.frames
    .flatMap((f) => f.patches)
    .filter(
      (p) =>
        p.SetProp?.prop === 'Choices' &&
        p.SetProp.value?.Choices?.some((item) => item.value === value),
    )
    .at(-1)?.SetProp.id;
  assert.ok(stage.surface.dispatch({ trigger: 'Change', node, id: `${node}:Change`, value }));
}
async function until(done, debug = () => '') {
  const deadline = Date.now() + 2000;
  while (!done()) {
    if (Date.now() >= deadline)
      throw new Error(`network detail lifecycle did not settle ${debug()}`);
    await new Promise((r) => setTimeout(r, 10));
  }
}
