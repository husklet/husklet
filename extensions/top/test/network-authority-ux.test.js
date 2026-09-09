import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { Networks } from '../dist/app.js';
import { host } from './host.js';

const networkId = 'a'.repeat(32);
const resource = {
  data: [
    {
      id: networkId,
      name: 'private',
      driver: 'bridge',
      scope: 'local',
      kind: 'custom',
      endpoints: { containers: [], truncated: false },
    },
  ],
  loading: false,
  error: null,
  reload: async () => {},
};

test('network scope refusal explains recovery and does not offer a futile retry', async () => {
  const denied = Object.assign(new Error('network is outside the consented resource scope'), {
    kind: 'denied',
    capability: 'networks:read',
  });
  const stage = host();
  stage.render(
    h(Networks, {
      api: {
        networks: {
          inspect: async () => {
            throw denied;
          },
          create: async () => '',
          removeAndWait: async () => ({ changed: true, id: networkId }),
          connect: async () => {},
          disconnect: async () => {},
        },
      },
      resource,
      containers: { data: [], loading: false, error: null, reload: async () => {} },
    }),
  );

  invoke(stage, 'Inspect');
  await settled();
  await settled();

  assert.ok(
    labelled(
      stage,
      'This extension was not granted access to inspect this network. Change its exact network access from Extensions, then inspect again.',
    ),
  );
  const access = labelled(stage, 'Access required');
  assert.ok(access);
  assert.equal(latestProperty(stage, access.SetProp.id, 'Enabled')?.Flag, false);
  assert.ok(labelled(stage, 'Danger zone'));
  assert.ok(labelled(stage, 'Remove'));
});

function labelled(stage, label) {
  return stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label)
    .at(-1);
}

function latestProperty(stage, node, prop) {
  return stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.id === node && patch.SetProp.prop === prop)
    .at(-1)?.SetProp.value;
}

function invoke(stage, label) {
  const nodes = stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label)
    .map((patch) => patch.SetProp.id)
    .reverse();
  assert.ok(
    nodes.some((node) =>
      stage.surface.dispatch({ trigger: 'Invoke', node, id: `${node}:Invoke`, value: null }),
    ),
  );
}

const settled = () => new Promise((resolve) => setImmediate(resolve));
