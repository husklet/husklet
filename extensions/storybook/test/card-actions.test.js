import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { CardActionsWorkbench } from '../dist/card-actions.js';
import { componentPages } from '../dist/component-pages.js';
import { host } from './host.js';

const creations = (frame, tag) =>
  frame.patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id);

const props = (frame, id) =>
  Object.fromEntries(
    frame.patches
      .filter((patch) => patch.SetProp?.id === id)
      .map((patch) => [patch.SetProp.prop, patch.SetProp.value]),
  );

test('CardActions owns a focused single-component workbench', () => {
  assert.equal(componentPages.CardActions, CardActionsWorkbench);
  const frame = host().render(h(CardActionsWorkbench));
  const headings = creations(frame, 'Heading').map((id) => props(frame, id).Label?.Text);
  assert.deepEqual(headings, [
    'CardActions',
    'Overview',
    'Alignment',
    'Density',
    'Width constraints',
    'Accessibility',
    'API',
  ]);
  const rows = creations(frame, 'CardActions').map((id) => props(frame, id));
  assert.equal(rows.length, 6);
  assert(rows.every((row) => row.Align?.Align === 'Center'));
  assert(rows.some((row) => row.Justify?.Align === 'Start'));
  assert(rows.some((row) => row.Justify?.Align === 'End'));
  assert(rows.some((row) => row.Justify?.Align === 'Center'));
  const buttons = creations(frame, 'Button').map((id) => props(frame, id));
  assert(buttons.length >= 10);
  assert(buttons.every((button) => button.Size?.ControlSize === 'Small'));
});
