import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { Networks, Volumes, WorkspaceManager } from '../src/app.js';
import { host } from './host.js';

const api = {
  containers: { list: async () => [], processes: async () => [] },
  images: { list: async () => [], pull: async () => ({}) },
  volumes: { list: async () => [], inspect: async () => ({}), create: async () => ({}), remove: async () => {} },
  networks: { list: async () => [], inspect: async () => ({}), create: async () => '', remove: async () => {}, connect: async () => {}, disconnect: async () => {} },
};

test('the test host receives overview and every resource navigation choice', () => {
  const frame = host().render(h(WorkspaceManager, { api, initial: { containers: [], images: [], volumes: [], networks: [] } }));
  const labels = frame.patches.filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label').map((patch) => patch.SetProp.value.Text);
  for (const label of ['Workspace overview', 'Containers', 'Processes', 'Images', 'Volumes', 'Networks']) assert.ok(labels.includes(label), label);
  assert.equal(frame.patches.some((patch) => 'Create' in patch && patch.Create.tag === 'Card'), true);
});

test('volume and network panels render bounded real inventories and controls', () => {
  const resource = (data) => ({ data, loading: false, error: null, reload: async () => {} });
  const volumeFrame = host().render(h(Volumes, { api, resource: resource([{ name: 'cache', driver: 'local' }]) }));
  const networkFrame = host().render(h(Networks, { api, resource: resource([{ id: 'n1', name: 'private', driver: 'bridge', scope: 'local' }]) }));
  const labels = (frame) => frame.patches.filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label').map((patch) => patch.SetProp.value.Text);
  for (const label of ['Volumes', 'cache', 'Create', 'Inspect', 'Remove']) assert.ok(labels(volumeFrame).includes(label), label);
  for (const label of ['Networks', 'private', 'Connect', 'Disconnect', 'Remove']) assert.ok(labels(networkFrame).includes(label), label);
});
