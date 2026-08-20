import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { Surface, reconciler } from '../src/reconciler.js';
import { value } from '../src/protocol.js';
import { Button, Column, Text } from '../src/components.js';

/** A surface that keeps its frames instead of writing them to a socket. */
function surface() {
  const frames = [];
  const sent = new Surface((frame) => frames.push(frame));
  const container = reconciler.createContainer(sent, 0, null, false, null, '', () => {}, null);
  return {
    frames,
    sent,
    render(element) {
      reconciler.updateContainer(element, container, null, null);
      return frames.at(-1) ?? null;
    },
    /** The patches of the most recent render, or none when nothing was sent. */
    last(before) {
      return frames.length === before ? [] : frames.at(-1).patches;
    },
  };
}

test('a button in a column is exactly six patches', () => {
  const host = surface();
  const frame = host.render(h(Column, null, h(Button, { label: 'Go', onInvoke: () => {} })));
  assert.deepEqual(frame, {
    sequence: 1,
    patches: [
      { Create: { id: 1, tag: 'Button' } },
      { SetProp: { id: 1, prop: 'Label', value: { Text: 'Go' } } },
      { SetHandler: { id: 1, handler: { trigger: 'Invoke', id: '1:Invoke' } } },
      { Create: { id: 2, tag: 'Column' } },
      { Insert: { parent: 2, child: 1, before: null } },
      { Insert: { parent: 0, child: 2, before: null } },
    ],
  });
});

test('rendering the same tree again sends nothing at all', () => {
  const host = surface();
  const tree = () => h(Column, null, h(Button, { label: 'Go', onInvoke: () => {} }));
  host.render(tree());
  const before = host.frames.length;
  host.render(tree());
  assert.equal(host.frames.length, before, 'an unchanged render is an empty frame, which is no frame');
});

test('changing one property is one patch', () => {
  const host = surface();
  host.render(h(Column, null, h(Text, { label: 'one' })));
  const before = host.frames.length;
  host.render(h(Column, null, h(Text, { label: 'two' })));
  assert.deepEqual(host.last(before), [{ SetProp: { id: 1, prop: 'Label', value: { Text: 'two' } } }]);
});

test('dropping a property clears it rather than leaving it behind', () => {
  const host = surface();
  host.render(h(Column, null, h(Text, { label: 'one', tone: 'accent' })));
  const before = host.frames.length;
  host.render(h(Column, null, h(Text, { label: 'one' })));
  assert.deepEqual(host.last(before), [{ ClearProp: { id: 1, prop: 'Tone' } }]);
});

test('dropping a handler clears it', () => {
  const host = surface();
  host.render(h(Column, null, h(Button, { label: 'Go', onInvoke: () => {} })));
  const before = host.frames.length;
  host.render(h(Column, null, h(Button, { label: 'Go' })));
  assert.deepEqual(host.last(before), [{ ClearHandler: { id: 1, trigger: 'Invoke' } }]);
});

test('removing a child removes it', () => {
  const host = surface();
  host.render(h(Column, null, h(Text, { key: 'a', label: 'a' }), h(Text, { key: 'b', label: 'b' })));
  const before = host.frames.length;
  host.render(h(Column, null, h(Text, { key: 'a', label: 'a' })));
  assert.deepEqual(host.last(before), [{ Remove: { id: 2 } }]);
});

test('reordering keyed children moves them instead of rebuilding them', () => {
  const host = surface();
  const row = (order) => h(Column, null, ...order.map((name) => h(Text, { key: name, label: name })));
  host.render(row(['a', 'b', 'c']));
  const before = host.frames.length;
  host.render(row(['c', 'a', 'b']));
  const patches = host.last(before);
  // React reaches c,a,b by moving a and then b past c rather than by moving c.
  assert.deepEqual(patches, [
    { Move: { parent: 4, child: 1, before: null } },
    { Move: { parent: 4, child: 2, before: null } },
  ]);
  for (const patch of patches) {
    assert.ok(!('Create' in patch) && !('Remove' in patch), 'a reorder is not a rebuild');
  }
});

test('the sequence increases by exactly one per frame', () => {
  const host = surface();
  for (let step = 0; step < 5; step += 1) {
    host.render(h(Column, null, h(Text, { label: `step ${step}` })));
  }
  assert.deepEqual(
    host.frames.map((frame) => frame.sequence),
    [1, 2, 3, 4, 5],
  );
  assert.equal(host.sent.sequence, 5);
});

test('text children become the label', () => {
  const host = surface();
  const frame = host.render(h(Text, null, 'hello'));
  assert.deepEqual(frame.patches, [
    { Create: { id: 1, tag: 'Text' } },
    { SetProp: { id: 1, prop: 'Label', value: { Text: 'hello' } } },
    { Insert: { parent: 0, child: 1, before: null } },
  ]);
});

test('an unknown prop is refused rather than dropped', () => {
  const host = surface();
  assert.throws(() => host.render(h(Text, { lable: 'typo' })), /has no prop lable/);
});

test('a growth factor is sent as a number, because a flag decodes as nothing', () => {
  // The host reads Grow with as_number(); a Flag returns None there and
  // silently means "do not expand", which is the opposite of what was asked.
  assert.deepEqual(value('Grow', true), { Number: 1 });
  assert.deepEqual(value('Grow', false), { Number: 0 });
  assert.deepEqual(value('Grow', 2), { Number: 2 });
});
