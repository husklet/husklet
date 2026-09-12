import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { CardWorkbench } from '../dist/card.js';
import { componentPages } from '../dist/component-pages.js';
import { host } from './host.js';

function creations(frame, tag) {
  return frame.patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id);
}

function props(frame, id) {
  return Object.fromEntries(
    frame.patches
      .filter((patch) => patch.SetProp?.id === id)
      .map((patch) => [patch.SetProp.prop, patch.SetProp.value]),
  );
}

function ancestorProps(frame, id, tag) {
  const patches = frame.patches;
  const tags = new Map(
    patches.filter((patch) => patch.Create).map((patch) => [patch.Create.id, patch.Create.tag]),
  );
  const parents = new Map(
    patches
      .filter((patch) => patch.Insert)
      .map((patch) => [patch.Insert.child, patch.Insert.parent]),
  );
  let node = id;
  while (parents.has(node)) {
    node = parents.get(node);
    if (tags.get(node) === tag) return props(frame, node);
  }
  return undefined;
}

test('Card owns a dedicated single-component document with canonical anatomy', () => {
  assert.equal(componentPages.Card, CardWorkbench);
  const frame = host().render(h(CardWorkbench));
  const headings = creations(frame, 'Heading').map((id) => props(frame, id).Label?.Text);
  assert.deepEqual(headings.slice(0, 7), [
    'Card',
    'Overview',
    'Anatomy',
    'Variants',
    'Sizing',
    'Inventory layout',
    'Wrapping',
  ]);
  assert(headings.includes('API'));

  const cards = creations(frame, 'Card');
  assert.equal(cards.length, 6);
  const variants = cards.map((id) => props(frame, id).Variant?.Variant);
  assert(variants.includes('Outline'));
  assert(variants.includes('Filled'));
  assert.equal(creations(frame, 'CardHeader').length, cards.length);
  assert.equal(creations(frame, 'CardContent').length, cards.length);
  assert.equal(creations(frame, 'CardActions').length, cards.length);
  assert(
    cards.some((id) => props(frame, id).Width?.Length?.Chars === 32),
    'Card documents a bounded native outer width',
  );
  const inventory = creations(frame, 'CardHeader').find(
    (id) => props(frame, id).Label?.Text === 'Inventory record',
  );
  assert.ok(inventory, 'Card documents an operational inventory record');
  assert.deepEqual(ancestorProps(frame, inventory, 'Card')?.Width, { Length: 'Fill' });
});

test('Card examples keep compact explicit actions and long copy inside the component', () => {
  const frame = host().render(h(CardWorkbench));
  const buttonLabels = creations(frame, 'Button').map((id) => props(frame, id).Label?.Text);
  assert(buttonLabels.includes('Open'));
  assert(buttonLabels.includes('More actions'));
  assert(buttonLabels.includes('Inspect'));
  assert(buttonLabels.includes('Remove'));
  assert(
    frame.patches.some(
      (patch) =>
        patch.SetProp?.prop === 'Label' &&
        patch.SetProp.value.Text?.includes('Long identifiers and operational explanations wrap'),
    ),
  );
});
