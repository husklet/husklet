import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { Top } from '../dist/app.js';
import { host } from './host.js';

const containerId = 'c'.repeat(32);
const networkId = 'n'.repeat(32);

test('container permission recovery navigates Top to Extensions', async () => {
  const stage = host();
  stage.render(
    h(Top, {
      api: deniedApi('container'),
      initialSection: 'containers',
      initial: {
        containers: [
          {
            id: containerId,
            name: 'private-api',
            image: 'internal/api:latest',
            state: 'running',
            generation: 4,
          },
        ],
        executions: [],
        images: [],
        volumes: [],
        networks: [],
        terminals: [],
        extensions: [],
      },
    }),
  );

  invoke(stage, 'Details');
  await settledTwice();
  invoke(stage, 'Review access');
  await settledTwice();

  assert.equal(taggedProperty(stage, 'Extensions', 'NavigationMenuItem', 'Selected')?.Flag, true);
  assert.ok(
    labelled(stage, 'Discover tools, review their access, and manage what runs in this workspace.'),
  );
});

test('network permission recovery navigates Top to Extensions', async () => {
  const stage = host();
  stage.render(
    h(Top, {
      api: deniedApi('network'),
      initialSection: 'networks',
      initial: {
        containers: [],
        executions: [],
        images: [],
        volumes: [],
        networks: [
          {
            id: networkId,
            name: 'private',
            driver: 'bridge',
            scope: 'local',
            kind: 'custom',
            endpoints: { containers: [], truncated: false },
          },
        ],
        terminals: [],
        extensions: [],
      },
    }),
  );

  invoke(stage, 'Manage connections');
  await settledTwice();
  invoke(stage, 'Review access');
  await settledTwice();

  assert.equal(taggedProperty(stage, 'Extensions', 'NavigationMenuItem', 'Selected')?.Flag, true);
});

function deniedApi(resource) {
  const denied = Object.assign(new Error('outside the consented resource scope'), {
    kind: 'denied',
  });
  return {
    containers: {
      list: async () => [],
      inspect: async () => {
        if (resource === 'container') throw denied;
        return {};
      },
      processes: async () => [],
      executions: async () => ({ executions: [], truncated: false }),
    },
    images: { list: async () => [] },
    volumes: { list: async () => [] },
    networks: {
      list: async () => [],
      inspect: async () => {
        if (resource === 'network') throw denied;
        return {};
      },
      create: async () => '',
      connect: async () => {},
      disconnect: async () => {},
      removeAndWait: async (id) => ({ changed: true, id }),
    },
    terminal: { tabs: async () => [] },
    extensions: {
      list: async () => [],
      watchExtensions: async () => () => {},
    },
    watchExtensions: async () => () => {},
    subscribe: async () => {},
    unsubscribe: async () => {},
  };
}

function labelled(stage, label) {
  return stage.frames
    .flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label)
    .at(-1);
}

function taggedProperty(stage, label, tag, prop) {
  const patches = stage.frames.flatMap((frame) => frame.patches);
  const tagged = new Set(
    patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id),
  );
  const node = patches
    .filter(
      (patch) =>
        patch.SetProp?.prop === 'Label' &&
        patch.SetProp.value?.Text === label &&
        tagged.has(patch.SetProp.id),
    )
    .at(-1)?.SetProp.id;
  return patches.filter((patch) => patch.SetProp?.id === node && patch.SetProp.prop === prop).at(-1)
    ?.SetProp.value;
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
const settledTwice = async () => {
  await settled();
  await settled();
};
