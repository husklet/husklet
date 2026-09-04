import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { JsonTree, ObjectInspector, inspectJson, visibleJsonRows } from '../src/json-tree.js';
import { Surface, reconciler } from '../src/reconciler.js';

function host() {
  const frames = [];
  const surface = new Surface((frame) => frames.push(frame));
  const container = reconciler.createContainer(surface, 0, null, false, null, '', () => {}, null);
  return { frames, surface, render(element) { reconciler.updateContainer(element, container, null, null); return frames.at(-1); } };
}

function labelled(stage, label) {
  return stage.frames.flatMap((frame) => frame.patches).filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label).at(-1);
}

function invoke(stage, label) {
  const node = labelled(stage, label)?.SetProp.id;
  assert.notEqual(node, undefined, `${label} exists`);
  assert.equal(stage.surface.dispatch({ trigger: 'Invoke', node, id: `${node}:Invoke`, value: null }), true);
}

test('inspection is cycle safe and explicitly bounds depth, nodes, and strings', () => {
  assert.equal(ObjectInspector, JsonTree);
  const value = { long: 'x'.repeat(20), deep: { child: { leaf: true } }, many: [1, 2, 3, 4] };
  value.self = value;
  const depth = inspectJson(value, { maxDepth: 2, maxNodes: 50, maxStringLength: 8 });
  assert.equal(depth.truncated, true);
  assert.match(depth.rows.find((row) => row.path === '$.long').display, /\+12 characters/);
  assert.equal(depth.rows.find((row) => row.path === '$.deep.child').truncated, true);
  assert.match(depth.rows.find((row) => row.path === '$.self').display, /Circular → \$/);

  const nodes = inspectJson(value, { maxDepth: 20, maxNodes: 5, maxStringLength: 100 });
  assert.equal(nodes.rows.length, 5);
  assert.match(nodes.rows.at(-1).display, /Node limit reached/);
});

test('filter matches values and retains their ancestors', () => {
  const { rows } = inspectJson({ alpha: { beta: { answer: 42 }, ignored: false }, other: 'no' });
  assert.deepEqual(visibleJsonRows(rows, new Set(), '42').map((row) => row.path), ['$', '$.alpha', '$.alpha.beta', '$.alpha.beta.answer']);
  assert.deepEqual(visibleJsonRows(rows, new Set(['$']), '').map((row) => row.path), ['$', '$.alpha', '$.other']);
});

test('composite renders only native nodes and expands, selects, copies, and filters', () => {
  const selected = []; const copied = [];
  const stage = host();
  const first = stage.render(h(JsonTree, { value: { nested: { answer: 42 }, enabled: true }, onSelect: (event) => selected.push(event), onCopy: (event) => copied.push(event) }));
  assert.equal(first.patches.some((patch) => patch.Create?.tag === 'JsonTree'), false);
  assert.ok(first.patches.some((patch) => patch.Create?.tag === 'Search'));
  invoke(stage, 'Expand $.nested');
  invoke(stage, '42');
  invoke(stage, 'Copy $.nested.answer');
  assert.deepEqual(selected, [{ path: '$.nested.answer', type: 'number', value: 42 }]);
  assert.deepEqual(copied, [{ path: '$.nested.answer', type: 'number', value: 42, text: '42' }]);
  const search = first.patches.find((patch) => patch.SetProp?.prop === 'Placeholder' && patch.SetProp.value?.Text === 'Filter paths, types, and values').SetProp.id;
  assert.equal(stage.surface.dispatch({ trigger: 'Change', node: search, id: `${search}:Change`, value: 'answer' }), true);
  assert.ok(stage.frames.flatMap((frame) => frame.patches).some((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text?.endsWith(' $.nested')), 'search retains the matching value ancestor');
});
