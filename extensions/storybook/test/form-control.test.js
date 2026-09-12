import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { FormControlWorkbench } from '../dist/form-control.js';
import { host } from './host.js';

function labels(patches) {
  return patches
    .filter((patch) => patch.SetProp?.prop === 'Label')
    .map((patch) => patch.SetProp.value?.Text);
}

test('FormControl canonical field retains the child Entry value and result', () => {
  const stage = host();
  const first = stage.render(h(FormControlWorkbench));
  const entry = first.patches.find((patch) => patch.Create?.tag === 'Entry')?.Create.id;
  assert.ok(entry);
  const before = stage.frames.length;
  assert(
    stage.surface.dispatch({
      trigger: 'Change',
      node: entry,
      id: `${entry}:Change`,
      value: 'pane-tools',
    }),
  );
  const changed = stage.since(before);
  assert(
    changed.some(
      (patch) =>
        patch.SetProp?.id === entry &&
        patch.SetProp.prop === 'Value' &&
        patch.SetProp.value?.Text === 'pane-tools',
    ),
  );
  assert(labels(changed).includes('Current name: pane-tools'));
});

test('FormControl teaches real states, ownership, and associations before API', () => {
  const frame = host().render(h(FormControlWorkbench));
  const text = labels(frame.patches);
  for (const label of ['Normal', 'Required *', 'Error', 'Disabled', 'With helper text']) {
    assert(text.includes(label), `missing ${label}`);
  }
  assert(text.some((label) => label?.includes('Entry owns value')));
  assert(text.some((label) => label?.includes('accessible label')));
  assert(text.some((label) => label?.includes('as its description')));
  assert(text.indexOf('States') < text.indexOf('API'));
  assert(text.indexOf('Accessibility') < text.indexOf('API'));
  assert(text.includes('Settings fields'));
  assert(text.includes('Default shell'));
  assert(text.includes('A placeholder shows an example; it does not replace the field label.'));
});

test('FormControl documents one compact wrapping row for a field and its actions', () => {
  const frame = host().render(h(FormControlWorkbench));
  const text = labels(frame.patches);
  assert(text.includes('Inline actions'));
  assert(text.includes('Image reference'));
  assert(text.includes('Pull'));
  assert(text.includes('Keep the field and its immediate actions in one wrapping row.'));
  assert.equal(
    frame.patches.filter((patch) => patch.Create?.tag === 'IconButton').length,
    1,
    'the specimen uses a semantic compact refresh action',
  );
});
