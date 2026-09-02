import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { WorkspaceManager } from '../src/app.js';
import { host } from './host.js';

const api = {
  containers: { list: async () => [], processes: async () => [] },
  images: { list: async () => [], pull: async () => ({}) },
};

test('the test host receives overview and every resource navigation choice', () => {
  const frame = host().render(h(WorkspaceManager, { api, initial: { containers: [], images: [] } }));
  const labels = frame.patches.filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label').map((patch) => patch.SetProp.value.Text);
  for (const label of ['Workspace overview', 'Containers', 'Processes', 'Images', 'Volumes', 'Networks']) assert.ok(labels.includes(label), label);
  assert.equal(frame.patches.some((patch) => 'Create' in patch && patch.Create.tag === 'Card'), true);
});
