import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { DragReorderStory, EVENT_LIMIT, initialItems, reorder } from '../src/drag-reorder.js';
import { host } from './host.js';

test('reorder is bounded to known immutable item identities', () => {
  assert.deepEqual(reorder(initialItems, 'build', 'publish').map(({ id }) => id), ['test', 'publish', 'build']);
  assert.equal(reorder(initialItems, 'missing', 'test'), initialItems);
  assert.equal(EVENT_LIMIT, 6);
});

test('story exposes native drag/drop handlers and keyboard alternatives', () => {
  const stage = host();
  const frame = stage.render(h(DragReorderStory));
  const triggers = frame.patches.filter((patch) => patch.SetHandler).map((patch) => patch.SetHandler.handler.trigger);
  assert.equal(triggers.filter((trigger) => trigger === 'Drag').length, initialItems.length);
  assert.equal(triggers.filter((trigger) => trigger === 'Drop').length, initialItems.length);
  assert.ok(triggers.filter((trigger) => trigger === 'Invoke').length >= initialItems.length * 2);
  assert.ok(frame.patches.some((patch) => patch.Create?.tag === 'Heading'));
});
