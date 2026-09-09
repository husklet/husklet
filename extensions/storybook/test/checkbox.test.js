import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { CheckboxWorkbench } from '../dist/checkbox.js';
import { apiRows } from '../dist/editors.js';
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

test('Checkbox API owns each generated prop and Toggle handler exactly once', () => {
  const api = apiRows('Checkbox');
  const owned = api.filter((row) =>
    ['label', 'checked', 'indeterminate', 'enabled', 'onToggle'].includes(row.name),
  );
  assert.deepEqual(
    owned.map((row) => row.name),
    ['label', 'checked', 'indeterminate', 'enabled', 'onToggle'],
  );
  for (const name of owned.map((row) => row.name)) {
    assert.equal(api.filter((row) => row.name === name).length, 1, `${name} is duplicated`);
  }
  assert.equal(owned.find((row) => row.name === 'onToggle').type, '(report: Report) => void');
  assert.match(
    owned.find((row) => row.name === 'indeterminate').note,
    /presentation.*resolves the next Toggle report/i,
  );
  assert(
    api.find((row) => row.name === 'selected'),
    'legacy selected remains generated',
  );
});

test('Checkbox rendered API places owned rows directly after its header', () => {
  const text = labels(host().render(h(CheckboxWorkbench)).patches);
  const header = text.indexOf('Property');
  const owned = ['label', 'checked', 'indeterminate', 'enabled', 'onToggle'];
  assert(header >= 0);
  assert.deepEqual(
    text.slice(header + 4, header + 4 + owned.length * 4).filter((_, index) => index % 4 === 0),
    owned,
  );
  assert(text.indexOf('Compatibility props · 1') > text.indexOf('onToggle'));
});
