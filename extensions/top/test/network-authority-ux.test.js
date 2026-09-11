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
  let openedExtensions = 0;
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
      onOpenExtensions: () => {
        openedExtensions += 1;
      },
    }),
  );

  invoke(stage, 'Manage connections');
  await settled();
  await settled();

  assert.ok(
    labelled(
      stage,
      'Top does not have permission to inspect this network. Review its exact network access in Extensions, then inspect again.',
    ),
  );
  assert.equal(labelled(stage, 'Access required'), undefined);
  assert.equal(labelled(stage, 'Retry managing connections'), undefined);
  assert.equal(
    activeLabels(stage).filter((label) => label.includes('permission to inspect this network'))
      .length,
    1,
    'the refusal is explained once',
  );
  invoke(stage, 'Review access');
  assert.equal(openedExtensions, 1, 'the recovery action invokes application navigation');
  assert.ok(labelled(stage, 'Danger zone'));
  assert.ok(labelled(stage, 'Remove'));
});

function labelled(stage, label) {
  return stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label)
    .at(-1);
}

function activeLabels(stage) {
  const labels = new Map();
  const parents = new Map();
  const removed = new Set();
  for (const patch of stage.frames.flatMap((frame) => frame.patches)) {
    if (patch.Insert) {
      parents.set(patch.Insert.child, patch.Insert.parent);
      removed.delete(patch.Insert.child);
    }
    if (patch.Remove) removed.add(patch.Remove.id);
    if (patch.SetProp?.prop === 'Label') labels.set(patch.SetProp.id, patch.SetProp.value?.Text);
  }
  const active = (node) => {
    for (let current = node; current !== undefined; current = parents.get(current)) {
      if (removed.has(current)) return false;
    }
    return true;
  };
  return [...labels].filter(([node]) => active(node)).map(([, label]) => label);
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
