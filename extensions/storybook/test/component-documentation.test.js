import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
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
  const inheritedLabels = created(frame.patches, 'Expander').flatMap((id) =>
    frame.patches
      .filter((patch) => patch.SetProp?.id === id && patch.SetProp.prop === 'Label')
      .map((patch) => String(patch.SetProp.value?.Text)),
  );
  assert(
    inheritedLabels.some((label) => label.startsWith('Inherited behavior and visibility props')),
  );
  assert(inheritedLabels.some((label) => label.startsWith('Inherited layout props')));
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
  assert.ok(
    stage.since(before).some((patch) => patch.SetProp?.value?.Text === 'Valid extension name.'),
  );
});

test('Entry teaches validation and state anatomy before its API', () => {
  const frame = host().render(h(EntryWorkbench));
  const labels = frame.patches
    .filter((patch) => patch.SetProp?.prop === 'Label')
    .map((patch) => patch.SetProp.value?.Text);
  for (const state of [
    'Empty with placeholder',
    'Focused',
    'Valid',
    'Error',
    'Disabled',
    'Secret',
  ]) {
    assert(labels.includes(state), `Entry is missing ${state}`);
  }
  const headings = created(frame.patches, 'Heading').flatMap((id) =>
    frame.patches
      .filter((patch) => patch.SetProp?.id === id && patch.SetProp.prop === 'Label')
      .map((patch) => patch.SetProp.value.Text),
  );
  assert(headings.indexOf('Widths') < headings.indexOf('API'));
  assert(headings.indexOf('Accessibility') < headings.indexOf('API'));
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

test('Select teaches controlled selection and bounded states before its API', () => {
  const frame = host().render(h(SelectWorkbench));
  const labels = frame.patches
    .filter((patch) => patch.SetProp?.prop === 'Label')
    .map((patch) => patch.SetProp.value?.Text);
  for (const state of ['Empty', 'Focused', 'Selected', 'Disabled', 'Invalid', 'Long label']) {
    assert(labels.includes(state), `Select is missing ${state}`);
  }
  assert(
    frame.patches.some(
      (patch) => patch.SetProp?.prop === 'Tone' && patch.SetProp.value?.Tone === 'Danger',
    ),
    'Select is missing its invalid control tone',
  );
  const headings = created(frame.patches, 'Heading').flatMap((id) =>
    frame.patches
      .filter((patch) => patch.SetProp?.id === id && patch.SetProp.prop === 'Label')
      .map((patch) => patch.SetProp.value.Text),
  );
  assert(headings.indexOf('States') < headings.indexOf('API'));
  assert(headings.indexOf('Accessibility') < headings.indexOf('API'));
});

test('Switch distinguishes every state and explains its control boundary before API', () => {
  const frame = host().render(h(SwitchWorkbench));
  const labels = frame.patches
    .filter((patch) => patch.SetProp?.prop === 'Label')
    .map((patch) => patch.SetProp.value?.Text);
  for (const state of [
    'Restore panes · on',
    'Restore panes · off',
    'Focused',
    'Disabled · off',
    'Disabled · on',
  ]) {
    assert(labels.includes(state), `Switch is missing ${state}`);
  }
  assert(labels.some((label) => label?.includes('ToggleButton')));
  assert(labels.some((label) => label?.includes('Checkbox')));
  const headings = created(frame.patches, 'Heading').flatMap((id) =>
    frame.patches
      .filter((patch) => patch.SetProp?.id === id && patch.SetProp.prop === 'Label')
      .map((patch) => patch.SetProp.value.Text),
  );
  assert(headings.indexOf('States') < headings.indexOf('API'));
  assert(headings.indexOf('Choose the right control') < headings.indexOf('API'));
  const switches = new Set(created(frame.patches, 'Switch'));
  assert.equal(
    frame.patches.filter(
      (patch) => switches.has(patch.SetProp?.id) && patch.SetProp.prop === 'Tooltip',
    ).length,
    0,
    'visible Switch captions are not duplicated as tooltips',
  );
  const labelRows = new Set(created(frame.patches, 'FormControlLabel'));
  assert(
    frame.patches.some(
      (patch) =>
        labelRows.has(patch.SetProp?.id) &&
        patch.SetProp.prop === 'Height' &&
        patch.SetProp.value?.Bounds?.minimum?.Step === 11,
    ),
    'Switch label rows preserve a 44px hit target',
  );
});

test('playground controls keep visible labels in the rendered tree', () => {
  for (const [Workbench, labels] of [
    [EntryWorkbench, ['Field width', 'Validation tone', 'Field enabled', 'Hide value']],
    [SelectWorkbench, ['Selector enabled', 'Use wide field']],
    [SwitchWorkbench, ['Example context', 'Preview enabled']],
  ]) {
    const frame = host().render(h(Workbench));
    for (const label of labels) {
      assert.ok(
        labelled(frame.patches, 'FormLabel', label) ||
          labelled(frame.patches, 'FormControlLabel', label),
        `missing persistent playground label: ${label}`,
      );
    }
  }
});

test('dense state specimens use the shared source-ordered narrow layout', () => {
  for (const Workbench of [EntryWorkbench, SelectWorkbench, SwitchWorkbench]) {
    const frame = host().render(h(Workbench));
    assert.equal(
      frame.patches.filter(
        (patch) => patch.SetProp?.prop === 'Columns' && patch.SetProp.value?.Integer === 2,
      ).length,
      0,
      'shared specimens must not force two columns into a narrow allocation',
    );
  }
});

test('Select state specimens declare matching bounded widths in source', () => {
  const source = readFileSync(new URL('../src/select.tsx', import.meta.url), 'utf8');
  const states = source.match(
    /<DocumentationSection title="States">([\s\S]*?)<\/DocumentationSection>/,
  )?.[1];
  assert.ok(states, 'Select states section is missing');
  assert.equal(states.match(/width=\{\{ chars: 30 \}\}/g)?.length, 12);
  assert.doesNotMatch(states, /width="fill"/);
});
