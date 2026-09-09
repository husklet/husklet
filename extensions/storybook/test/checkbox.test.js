import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { CheckboxWorkbench } from '../dist/checkbox.js';
import { host } from './host.js';

function labels(patches) {
  return patches
    .filter((patch) => patch.SetProp?.prop === 'Label')
    .map((patch) => patch.SetProp.value?.Text);
}

test('mixed Select all resolves to checked through its controlled Toggle report', () => {
  const stage = host();
  const first = stage.render(h(CheckboxWorkbench));
  const parent = first.patches.find((patch) => patch.Create?.tag === 'Checkbox')?.Create.id;
  assert.ok(parent);
  assert(
    first.patches.some(
      (patch) =>
        patch.SetProp?.id === parent &&
        patch.SetProp.prop === 'Indeterminate' &&
        patch.SetProp.value?.Flag === true,
    ),
  );
  const before = stage.frames.length;
  assert(
    stage.surface.dispatch({
      trigger: 'Toggle',
      node: parent,
      id: `${parent}:Toggle`,
      value: true,
    }),
  );
  const changed = stage.since(before);
  assert(
    changed.some(
      (patch) =>
        patch.SetProp?.id === parent &&
        patch.SetProp.prop === 'Checked' &&
        patch.SetProp.value?.Flag === true,
    ),
  );
  assert(
    changed.some(
      (patch) =>
        patch.SetProp?.id === parent &&
        patch.SetProp.prop === 'Indeterminate' &&
        patch.SetProp.value?.Flag === false,
    ),
  );
  assert(labels(changed).includes('Select all · 3 of 3'));
  assert(labels(changed).includes('All diagnostics selected.'));
});

test('Checkbox documents enabled and disabled unchecked, checked, and mixed states', () => {
  const frame = host().render(h(CheckboxWorkbench));
  const text = labels(frame.patches);
  for (const state of [
    'Enabled · unchecked',
    'Enabled · checked',
    'Enabled · mixed',
    'Disabled · unchecked',
    'Disabled · checked',
    'Disabled · mixed',
  ])
    assert(text.includes(state));
  assert(
    text.some((label) =>
      label?.includes('pressing Space on a mixed parent resolves it to checked'),
    ),
  );
  assert(text.indexOf('State matrix') < text.indexOf('API'));
});
