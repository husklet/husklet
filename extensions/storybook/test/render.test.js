// The render path, without a host: the reconciler collects patches into frames
// and the test reads them, exactly as @husklet/react's own tests do.

import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { Playground, Preview, interactionDetail, interactionProps } from '../src/app.js';
import { tags } from '../src/catalogue.js';
import { defaults } from '../src/defaults.js';
import { components } from '@husklet/react';
import { ACQUISITION_STORY, acquisitionStates } from '../src/acquisition.js';

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
  assert.equal(
    built.filter((tag) => tag === 'ListItemButton').length,
    tags.length + 1,
    'every component and the end-user flow are listed',
  );
  assert.ok(built.includes('Scroll'), 'the sidebar and the inspector scroll');
  assert.ok(built.includes('Select') && built.includes('Switch') && built.includes('NumberEntry'));
});

test('the acquisition flow renders every semantic progress state and only its supported actions', () => {
  const stage = host();
  const first = stage.render(h(Playground));
  const item = node(first.patches, 'ListItemButton', ACQUISITION_STORY);
  assert.ok(item, 'the sidebar has no acquisition flow');
  const before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: item, id: `${item}:Invoke`, value: null }));
  const patches = stage.since(before);
  for (const state of acquisitionStates) {
    assert.ok(node(patches, 'CardHeader', state.title), `${state.key} is absent`);
    assert.ok(node(patches, 'InlineMessage', state.status), `${state.key} has no semantic status`);
  }
  for (const action of ['Cancel download', 'Retry', 'Install', 'Cancel']) {
    assert.ok(node(patches, 'Button', action), `${action} is not demonstrated`);
  }
  assert.equal(
    created(patches).filter((entry) => entry.tag === 'Progress').length,
    1,
    'only measured transfer claims a fraction',
  );
  assert.equal(
    created(patches).filter((entry) => entry.tag === 'Spinner').length,
    3,
    'checking, unknown transfer and manifest read remain indeterminate',
  );
});

test('the preview is a real instance of the selected component', () => {
  const stage = host();
  const frame = stage.render(h(Playground));
  assert.ok(
    created(frame.patches).some((entry) => entry.tag === 'Button'),
    'the component the playground opens on is rendered, not described',
  );
});

test('the preview demonstrates declared interactions with a live bounded console', () => {
  const stage = host();
  const opened = defaults('Button');
  const first = stage.render(h(Preview, {
    name: 'Button',
    opened,
    triggers: ['Invoke', 'Key'],
  }));
  const preview = node(first.patches, 'Button', 'Button');
  assert.ok(preview, 'the interactive preview button is absent');
  assert.ok(node(first.patches, 'InlineMessage', 'Interact with the preview to inspect onInvoke, onKey.'));

  const before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: preview, id: `${preview}:Invoke`, value: null }));
  const patches = stage.since(before);
  assert.ok(
    patches.some(
      (patch) => 'SetProp' in patch
        && patch.SetProp.prop === 'Label'
        && patch.SetProp.value.Text === '#1 Invoke received · value=null',
    ),
    'a real preview event never reaches the visible console',
  );
});

test('interaction handlers follow the catalogue and payload descriptions stay bounded', () => {
  const seen = [];
  const handlers = interactionProps(['Change', 'Focus'], (trigger, event) => seen.push([trigger, event]));
  assert.deepEqual(Object.keys(handlers), ['onChange', 'onFocus']);
  handlers.onFocus({ focused: true });
  assert.deepEqual(seen, [['Focus', { focused: true }]]);
  assert.equal(interactionDetail({ key: 'a', pressed: true, private: 'not shown' }), 'key="a" pressed=true');
  const long = interactionDetail({ value: 'x'.repeat(500) });
  assert.ok(long.length <= 240);
  assert.ok(long.endsWith('…"'), 'a long value is not visibly marked as truncated');
  assert.equal(
    interactionDetail({ rows: Array.from({ length: 100_000 }, (_, index) => ({ index, private: 'not shown' })) }),
    'rows=[{"index":0,"private":"not shown"},{"index":1,"private":"not shown"},{"index":2,"private":"not shown"},"… 99997 more"]',
  );
});

test('the interaction console preserves a bounded sequence and can be cleared', () => {
  const stage = host();
  const opened = defaults('Button');
  const first = stage.render(h(Preview, { name: 'Button', opened, triggers: ['Invoke'] }));
  const preview = node(first.patches, 'Button', 'Button');
  for (let index = 0; index < 7; index += 1) {
    assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: preview, id: `${preview}:Invoke`, value: index }));
  }
  const labels = stage.frames.flatMap((frame) => frame.patches)
    .filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label')
    .map((patch) => patch.SetProp.value.Text);
  assert.ok(labels.includes('#7 Invoke received · value=6'), 'the newest interaction is absent');

  const latest = stage.frames.at(-1).patches;
  const removed = latest.filter((patch) => 'Remove' in patch).length;
  assert.ok(removed > 0, 'the oldest interaction was not evicted from the bounded timeline');
  const clear = node(stage.frames.flatMap((frame) => frame.patches), 'Button', 'Clear');
  assert.ok(clear, 'the populated console has no clear action');
  const before = stage.frames.length;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: clear, id: `${clear}:Invoke`, value: null }));
  assert.ok(node(stage.since(before), 'InlineMessage', 'Interact with the preview to inspect onInvoke.'));
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

test('the inspector exposes only the genuine extended interactions', () => {
  const triggers = tags.find((tag) => tag.name === 'IconButton').triggers;
  for (const interaction of ['onKey', 'onFocus', 'onPointer', 'onContext']) {
    assert.ok(triggers.includes(interaction.slice(2)), `<IconButton> does not expose ${interaction}`);
  }
  assert.equal(triggers.includes('Scroll'), false, '<IconButton> invents scrolling');
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
