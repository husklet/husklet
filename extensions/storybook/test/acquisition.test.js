import assert from 'node:assert/strict';
import test from 'node:test';
import React from 'react';

import { AcquisitionProgressStory, acquisitionStates } from '../dist/acquisition.js';
import { host } from './host.js';

const { createElement: h } = React;

test('acquisition examples preserve the user-visible semantic lifecycle', () => {
  assert.deepEqual(
    acquisitionStates.map((state) => state.key),
    ['checking', 'pulling-indeterminate', 'pulling-determinate', 'manifest', 'failure', 'ready'],
  );
  const byKey = Object.fromEntries(acquisitionStates.map((state) => [state.key, state]));
  assert.match(byKey['pulling-indeterminate'].status, /progress unavailable/);
  assert.match(byKey['pulling-determinate'].status, /25%; 25 of 100 bytes/);
  assert.deepEqual(byKey.failure.actions, ['Retry']);
  assert.deepEqual(byKey.ready.actions, ['Install', 'Cancel']);
  assert.ok(
    ['checking', 'pulling-indeterminate', 'pulling-determinate', 'manifest'].every((key) =>
      byKey[key].actions.includes('Cancel download'),
    ),
  );
});

test('acquisition states are selectable without materializing every state at once', () => {
  const stage = host();
  const frame = stage.render(h(AcquisitionProgressStory));
  assert.equal(frame.patches.filter((patch) => patch.Create?.tag === 'Card').length, 1);
  const select = frame.patches.find((patch) => patch.Create?.tag === 'Select')?.Create.id;
  assert.ok(select);
  assert.equal(stage.surface.dispatch({ trigger: 'Change', node: select, id: `${select}:Change`, value: 'ready' }), true);
  const labels = stage.frames.flatMap(({ patches }) => patches)
    .filter((patch) => patch.SetProp?.prop === 'Label')
    .map((patch) => patch.SetProp.value?.Text);
  assert.ok(labels.includes('Ready for consent'));
  assert.ok(labels.includes('Install') && labels.includes('Cancel'));
});
