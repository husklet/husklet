// The render path, without a host: the reconciler collects patches into frames
// and the test reads them, exactly as @husklet/react's own tests do.

import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { Playground } from '../src/app.js';
import { tags } from '../src/catalogue.js';
import { defaults } from '../src/defaults.js';
import { components } from '@husklet/react';

import { host } from './host.js';

/** The nodes a frame created, as `{id, tag}`. */
function created(patches) {
  return patches.filter((patch) => 'Create' in patch).map((patch) => patch.Create);
}

/** The identity of the first node created with the given tag and label. */
function node(patches, tag, label) {
  let candidate = null;
  for (const patch of patches) {
    if ('Create' in patch && patch.Create.tag === tag) candidate = patch.Create.id;
    if (
      candidate !== null &&
      'SetProp' in patch &&
      patch.SetProp.id === candidate &&
      patch.SetProp.prop === 'Label' &&
      patch.SetProp.value.Text === label
    ) {
      return candidate;
    }
  }
  return null;
}

test('every component renders with its defaults as sane patches', () => {
  for (const tag of tags) {
    const stage = host();
    const opened = defaults(tag.name);
    const frame = stage.render(
      h(
        components[tag.name],
        opened.props,
        ...opened.children.map((child, index) => h(components[child.tag], { key: index, ...child.props })),
      ),
    );
    assert.ok(frame, `<${tag.name}> rendered nothing at all`);
    const roots = created(frame.patches).filter((entry) => entry.tag === tag.name);
    assert.equal(roots.length, 1, `<${tag.name}> is not a single instance of itself`);
    assert.ok(
      frame.patches.some((patch) => 'SetProp' in patch && patch.SetProp.id === roots[0].id) ||
        opened.children.length > 0,
      `<${tag.name}> arrived with no properties and no children`,
    );
  }
});

test('the playground renders one frame holding the three panes', () => {
  const stage = host();
  const frame = stage.render(h(Playground));
  const built = created(frame.patches).map((entry) => entry.tag);
  assert.equal(frame.sequence, 1, 'a commit is one atomic frame');
  assert.equal(built.filter((tag) => tag === 'Row').length >= 1, true);
  assert.equal(built.filter((tag) => tag === 'ListItemButton').length, tags.length, 'every component is listed');
  assert.ok(built.includes('Scroll'), 'the sidebar and the inspector scroll');
  assert.ok(built.includes('Select') && built.includes('Switch') && built.includes('NumberEntry'));
});

test('the preview is a real instance of the selected component', () => {
  const stage = host();
  const frame = stage.render(h(Playground));
  assert.ok(
    created(frame.patches).some((entry) => entry.tag === 'Button'),
    'the component the playground opens on is rendered, not described',
  );
});

test('selecting a component in the sidebar renders that component', () => {
  const stage = host();
  const first = stage.render(h(Playground));
  const item = node(first.patches, 'ListItemButton', 'Chip');
  assert.ok(item, 'the sidebar has no row for <Chip>');
  const before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: item, id: `${item}:Invoke`, value: null }));
  const patches = stage.since(before);
  assert.ok(
    created(patches).some((entry) => entry.tag === 'Chip'),
    'selecting <Chip> did not render a <Chip>',
  );
});

test('the inspector follows the selected component contract and shows its interactions', () => {
  const stage = host();
  const first = stage.render(h(Playground));
  const item = node(first.patches, 'ListItemButton', 'Switch');
  assert.ok(item, 'the sidebar has no row for <Switch>');
  const before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: item, id: `${item}:Invoke`, value: null }));
  const patches = stage.since(before);
  assert.ok(node(patches, 'Text', 'checked'), '<Switch> does not expose its checked property');
  assert.ok(node(patches, 'Text', 'onToggle'), '<Switch> does not expose its Toggle interaction');
  assert.equal(node(patches, 'Text', 'label'), null, '<Switch> exposes Button-only label editing');
});

test('editing a property re-renders the preview with the new value', () => {
  const stage = host();
  const first = stage.render(h(Playground));
  // The preview's button carries the default label; the inspector's Entry for
  // `label` is the one bound to Change beside the row named "label".
  const entry = labelEntry(first.patches);
  assert.ok(entry, 'the inspector has no text field for the label');
  const before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Change', node: entry, id: `${entry}:Change`, value: 'Pressed' }));
  const patches = stage.since(before);
  assert.ok(
    patches.some(
      (patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label' && patch.SetProp.value.Text === 'Pressed',
    ),
    'the new label never reached the host',
  );
});

/** The identity of the inspector's `label` field: the Entry after that row's name. */
function labelEntry(patches) {
  let seenRow = false;
  let entry = null;
  for (const patch of patches) {
    if (
      'SetProp' in patch &&
      patch.SetProp.prop === 'Label' &&
      patch.SetProp.value.Text === 'label' &&
      !seenRow
    ) {
      seenRow = true;
      continue;
    }
    if (seenRow && 'Create' in patch && patch.Create.tag === 'Entry') {
      entry = patch.Create.id;
      break;
    }
  }
  return entry;
}
