import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { ExpanderWorkbench } from '../dist/expander.js';
import { componentPages } from '../dist/component-pages.js';
import { host } from './host.js';

function created(patches, tag) {
  return patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id);
}

function props(patches, id) {
  return Object.fromEntries(
    patches
      .filter((patch) => patch.SetProp?.id === id)
      .map((patch) => [patch.SetProp.prop, patch.SetProp.value]),
  );
}

function labelled(patches, tag, label) {
  return created(patches, tag).find((id) => props(patches, id).Label?.Text === label);
}

test('Expander owns one precise component document with meaningful disclosure states', () => {
  const frame = host().render(h(ExpanderWorkbench));
  const headings = created(frame.patches, 'Heading').map(
    (id) => props(frame.patches, id).Label?.Text,
  );

  assert.equal(componentPages.Expander, ExpanderWorkbench);
  assert.deepEqual(headings.slice(0, 7), [
    'Expander',
    'Overview',
    'API',
    'States',
    'Action disclosure',
    'Behavior',
    'Accessibility',
  ]);

  const controlled = labelled(frame.patches, 'Expander', 'Runtime diagnostics');
  const examples = created(frame.patches, 'Expander').map((id) => props(frame.patches, id));
  assert.ok(controlled, 'controlled specimen has a visible summary');
  assert.equal(props(frame.patches, controlled).Expanded.Flag, false);
  assert(
    examples.some(
      (value) => value.Label?.Text === 'Technical details' && value.Expanded?.Flag === false,
    ),
  );
  assert(
    examples.some(
      (value) => value.Label?.Text === 'Technical details' && value.Expanded?.Flag === true,
    ),
  );
  assert(examples.some((value) => value.Label?.Text?.startsWith('Connection diagnostics')));
  assert(
    examples.some(
      (value) =>
        value.Label?.Text === 'More actions' &&
        value.Variant?.Variant === 'Outline' &&
        value.Width?.Length === 'Content' &&
        value.Align?.Align === 'Start',
    ),
    'the compact action disclosure is a visibly bounded control',
  );
  assert(
    created(frame.patches, 'TableRow').length > 0,
    'the generated Expander API remains visible',
  );
});

test('Expander reports and renders back each controlled state transition', () => {
  const stage = host();
  const first = stage.render(h(ExpanderWorkbench));
  const disclosure = labelled(first.patches, 'Expander', 'Runtime diagnostics');
  assert.ok(disclosure);

  let before = stage.frames.length;
  assert.ok(
    stage.surface.dispatch({
      trigger: 'Expand',
      node: disclosure,
      id: `${disclosure}:Expand`,
      value: true,
    }),
  );
  let changes = stage.since(before);
  assert(
    changes.some((patch) => patch.SetProp?.id === disclosure && patch.SetProp.value?.Flag === true),
  );
  assert(
    changes.some(
      (patch) => patch.SetProp?.value?.Text === 'Runtime diagnostics expanded · 1 event',
    ),
  );

  before = stage.frames.length;
  assert.ok(
    stage.surface.dispatch({
      trigger: 'Expand',
      node: disclosure,
      id: `${disclosure}:Expand`,
      value: false,
    }),
  );
  changes = stage.since(before);
  assert(
    changes.some(
      (patch) => patch.SetProp?.id === disclosure && patch.SetProp.value?.Flag === false,
    ),
  );
  assert(
    changes.some(
      (patch) => patch.SetProp?.value?.Text === 'Runtime diagnostics collapsed · 2 events',
    ),
  );
});
