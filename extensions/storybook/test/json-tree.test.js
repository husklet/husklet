import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { Playground } from '../src/app.js';
import { JSON_TREE_STORY, JsonTreeStory } from '../src/json-tree.js';
import { host } from './host.js';

test('JSON tree story is composed, bounded, and reports interactions', () => {
  const stage = host();
  const frame = stage.render(h(JsonTreeStory));
  assert.equal(frame.patches.some((patch) => patch.Create?.tag === 'JsonTree'), false);
  assert.ok(frame.patches.some((patch) => patch.Create?.tag === 'Search'));
  const copy = frame.patches.find((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === 'Copy $.status').SetProp.id;
  assert.equal(stage.surface.dispatch({ trigger: 'Invoke', node: copy, id: `${copy}:Invoke`, value: null }), true);
  assert.ok(stage.frames.flatMap((item) => item.patches).some((patch) => patch.SetProp?.value?.Text === 'Copy requested for $.status: ready'));
  assert.ok(frame.patches.some((patch) => patch.SetProp?.value?.Text?.includes('Truncated values are marked.')));
});

test('bounded JSON tree is selectable through the shipped playground flow', () => {
  const frame = host().render(h(Playground, { initialStory: JSON_TREE_STORY }));
  assert.ok(frame.patches.some((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === 'Bounded JSON tree'));
  assert.ok(frame.patches.some((patch) => patch.Create?.tag === 'Search'));
});
