import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { Images, Networks, Volumes, WorkspaceManager } from '../src/app.js';
import { host } from './host.js';

const api = {
  containers: { list: async () => [], processes: async () => [] },
  images: { list: async () => [], pull: async () => ({}), inspect: async () => ({}), remove: async () => {}, prune: async () => ({ deleted: 0, space_reclaimed: 0 }) },
  volumes: { list: async () => [], inspect: async () => ({}), create: async () => ({}), remove: async () => {} },
  networks: { list: async () => [], inspect: async () => ({}), create: async () => '', remove: async () => {}, connect: async () => {}, disconnect: async () => {} },
};

test('the test host receives overview and every resource navigation choice', () => {
  const frame = host().render(h(WorkspaceManager, { api, initial: { containers: [], images: [], volumes: [], networks: [] } }));
  const labels = frame.patches.filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label').map((patch) => patch.SetProp.value.Text);
  for (const label of ['Workspace overview', 'Containers', 'Processes', 'Images', 'Volumes', 'Networks']) assert.ok(labels.includes(label), label);
  assert.equal(frame.patches.some((patch) => 'Create' in patch && patch.Create.tag === 'Card'), true);
});

test('image removal and prune require an explicit confirmation step', () => {
  const resource = { data: [{ id: 'sha256:one', reference: 'alpine:3.20', size: 7, created: 0 }], loading: false, error: null, reload: async () => {} };
  const stage = host();
  const frame = stage.render(h(Images, { api, resource }));
  const labels = () => stage.frames.flatMap((current) => current.patches).filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label');
  const remove = labels().find((patch) => patch.SetProp.value.Text === 'Remove').SetProp.id;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: remove, id: `${remove}:Invoke`, value: null }));
  assert.ok(labels().some((patch) => patch.SetProp.value.Text === 'Confirm remove'));
  assert.equal(frame.patches.some((patch) => 'SetProp' in patch && patch.SetProp.value?.Text === 'Confirm remove'), false);

  const pruneStage = host();
  const pruneFrame = pruneStage.render(h(Images, { api, resource }));
  const prune = pruneFrame.patches.find((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label' && patch.SetProp.value.Text === 'Prune unused images').SetProp.id;
  assert.ok(pruneStage.surface.dispatch({ trigger: 'Invoke', node: prune, id: `${prune}:Invoke`, value: null }));
  assert.ok(pruneStage.frames.flatMap((current) => current.patches).some((patch) => 'SetProp' in patch && patch.SetProp.value?.Text === 'Confirm prune'));
});

test('volume and network panels render bounded real inventories and controls', () => {
  const resource = (data) => ({ data, loading: false, error: null, reload: async () => {} });
  const volumeFrame = host().render(h(Volumes, { api, resource: resource([{ name: 'cache', driver: 'local' }]) }));
  const networkFrame = host().render(h(Networks, { api, resource: resource([{ id: 'n1', name: 'private', driver: 'bridge', scope: 'local' }]) }));
  const labels = (frame) => frame.patches.filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label').map((patch) => patch.SetProp.value.Text);
  for (const label of ['Volumes', 'cache', 'Create', 'Inspect', 'Remove']) assert.ok(labels(volumeFrame).includes(label), label);
  for (const label of ['Networks', 'private', 'Connect', 'Disconnect', 'Remove']) assert.ok(labels(networkFrame).includes(label), label);
  const destructive = (frame, label) => {
    const id = frame.patches.find((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label' && patch.SetProp.value.Text === label).SetProp.id;
    return frame.patches.some((patch) => 'SetProp' in patch && patch.SetProp.id === id && patch.SetProp.prop === 'Destructive' && patch.SetProp.value.Flag === true);
  };
  assert.equal(destructive(volumeFrame, 'Remove'), true);
  assert.equal(destructive(networkFrame, 'Disconnect'), true);
  assert.equal(destructive(networkFrame, 'Remove'), true);
});
