import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { EntryWorkbench } from '../dist/entry.js';
import { SelectWorkbench } from '../dist/select.js';
import { SwitchWorkbench } from '../dist/switch.js';
import { host } from './host.js';

function created(patches, tag) {
  return patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id);
}

test('API references expose public types and defaults and collapse inherited props', () => {
  const frame = host().render(h(SelectWorkbench));
  for (const heading of ['Property', 'Type', 'Default', 'Description']) {
    assert.ok(labelled(frame.patches, 'TableCell', heading), `API table is missing ${heading}`);
  }
  assert.equal(labelled(frame.patches, 'TableCell', 'Control'), null);
  assert.ok(
    created(frame.patches, 'TableCell').some((id) =>
      frame.patches.some(
        (patch) =>
          patch.SetProp?.id === id &&
          patch.SetProp.prop === 'Label' &&
          String(patch.SetProp.value?.Text).includes('string'),
      ),
    ),
    'API table has no public string type',
  );
  assert.ok(
    created(frame.patches, 'Expander').some((id) =>
      frame.patches.some(
        (patch) =>
          patch.SetProp?.id === id &&
          patch.SetProp.prop === 'Label' &&
          String(patch.SetProp.value?.Text).startsWith('Inherited layout and automation props'),
      ),
    ),
    'inherited props are not collapsed separately',
  );
});

function labelled(patches, tag, label) {
  const candidates = new Set(created(patches, tag));
  return (
    patches.find(
      (patch) =>
        candidates.has(patch.SetProp?.id) &&
        patch.SetProp.prop === 'Label' &&
        patch.SetProp.value.Text === label,
    )?.SetProp.id ?? null
  );
}

for (const [name, Workbench, tag] of [
  ['Entry', EntryWorkbench, 'Entry'],
  ['Select', SelectWorkbench, 'Select'],
  ['Switch', SwitchWorkbench, 'Switch'],
]) {
  test(`${name} has a complete single-component document`, () => {
    const frame = host().render(h(Workbench));
    assert.ok(labelled(frame.patches, 'Heading', name));
    for (const section of ['Overview', 'Accessibility', 'API']) {
      assert.ok(labelled(frame.patches, 'Heading', section), `${name} is missing ${section}`);
    }
    assert.ok(created(frame.patches, tag).length > 0, `${name} has no live example`);
    assert.ok(labelled(frame.patches, 'Expander', 'Playground'), `${name} has no playground`);
    assert.ok(created(frame.patches, 'Code').length === 1, `${name} has no single API snippet`);
  });
}

test('Entry playground is controlled by real change reports', () => {
  const stage = host();
  const first = stage.render(h(EntryWorkbench));
  const entry = created(first.patches, 'Entry')[0];
  const before = stage.frames.length;
  assert.ok(
    stage.surface.dispatch({
      trigger: 'Change',
      node: entry,
      id: `${entry}:Change`,
      value: 'renamed',
    }),
  );
  assert.ok(
    stage
      .since(before)
      .some((patch) => patch.SetProp?.id === entry && patch.SetProp.value?.Text === 'renamed'),
  );
});

test('Select and Switch playgrounds retain their reported values', () => {
  for (const [Workbench, tag, trigger, value, encoded] of [
    [SelectWorkbench, 'Select', 'Change', 'fish', { Text: 'fish' }],
    [SwitchWorkbench, 'Switch', 'Toggle', false, { Flag: false }],
  ]) {
    const stage = host();
    const first = stage.render(h(Workbench));
    const control = created(first.patches, tag)[0];
    const before = stage.frames.length;
    assert.ok(
      stage.surface.dispatch({ trigger, node: control, id: `${control}:${trigger}`, value }),
    );
    assert.ok(
      stage
        .since(before)
        .some((patch) =>
          Object.entries(encoded).every(
            ([key, expected]) => patch.SetProp?.value?.[key] === expected,
          ),
        ),
      `${tag} did not retain its controlled value`,
    );
  }
});
