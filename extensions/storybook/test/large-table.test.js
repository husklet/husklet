import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { Playground } from '../dist/app.js';
import { LargeDataTableStory, LargeRecordSource, LOGICAL_ROWS, OPERATION_HISTORY_LIMIT, WINDOW_LIMIT, SOURCE } from '../dist/large-table.js';
import { host } from './host.js';

function node(patches, tag, label) {
  let candidate = null;
  for (const patch of patches) {
    if ('Create' in patch && patch.Create.tag === tag) candidate = patch.Create.id;
    if (candidate !== null && 'SetProp' in patch && patch.SetProp.id === candidate
      && patch.SetProp.prop === 'Label' && patch.SetProp.value.Text === label) return candidate;
  }
  return null;
}

test('one DataTable node represents 100k rows without materializing row nodes', () => {
  const stage = host();
  const source = new LargeRecordSource();
  const frame = stage.render(h(LargeDataTableStory, { source }));
  const created = frame.patches.filter((patch) => 'Create' in patch).map((patch) => patch.Create.tag);
  assert.equal(source.length(), LOGICAL_ROWS);
  assert.equal(created.filter((tag) => tag === 'DataTable').length, 1);
  assert.equal(created.filter((tag) => tag === 'TableRow' || tag === 'TableCell').length, 0);
  assert(created.length < 20, `100k logical rows created ${created.length} React nodes`);
});

test('sort, filter, resize and all states remain bounded source operations', async () => {
  const mutations = [];
  const source = new LargeRecordSource(async (call, argument) => mutations.push([call, argument]));
  await source.publish();
  assert.equal(mutations[0][1].mutation.Length.rows, LOGICAL_ROWS);

  const request = { id: 4, source: SOURCE, version: 1, range: { start: 40_000, count: 10_000 }, sort: null, filter: null };
  const first = source.answer(request);
  assert.equal(first.rows.length, WINDOW_LIMIT, 'a resized viewport cannot exceed one protocol window');
  assert(first.rows.length <= 128, 'the wire window has a fixed production bound');
  assert.equal(source.generated, WINDOW_LIMIT);

  assert.deepEqual(await source.sort({ source: SOURCE, version: 1, column: 'id', descending: true }), { accepted: true });
  const sorted = source.answer({ ...request, id: 5, version: 2, range: { start: 0, count: 8 } });
  assert.equal(sorted.rows[0].key, LOGICAL_ROWS - 1);
  assert.equal(source.answer(request), null, 'a stale pre-sort request cannot repopulate the table');

  await source.configure({ filter: 'needle' });
  assert.equal(source.length(), 1_000);
  const filtered = source.answer({ ...request, id: 6, version: 3, range: { start: 0, count: 4 } });
  assert.match(filtered.rows[0].cells[1].Text, /^needle-/);

  await source.configure({ state: 'loading' });
  assert.equal(source.answer({ ...request, version: 4 }), null);
  await source.configure({ state: 'empty' });
  assert.equal(source.length(), 0);
  await source.configure({ state: 'error' });
  const failed = source.answer({ ...request, version: 6, range: { start: 0, count: 1 } });
  assert.equal(failed.rows[0].cells[2].Badge.tone, 'Danger');
  assert(mutations.every(([call]) => call === 'source_resize'));
});

test('the story controls visibly demonstrate loading, empty, error, filter and sort', () => {
  const stage = host();
  const source = new LargeRecordSource();
  const first = stage.render(h(LargeDataTableStory, { source }));
  const created = first.patches.filter((patch) => 'Create' in patch).map((patch) => patch.Create);
  const select = created.find(({ tag }) => tag === 'Select').id;
  const entry = created.find(({ tag }) => tag === 'Entry').id;
  const button = created.find(({ tag }) => tag === 'Button').id;
  const tagsSince = (before) => stage.since(before).filter((patch) => 'Create' in patch).map((patch) => patch.Create.tag);

  let before = stage.frames.length;
  stage.surface.dispatch({ trigger: 'Change', node: select, id: `${select}:Change`, value: 'loading' });
  assert(tagsSince(before).includes('Progress'));
  before = stage.frames.length;
  stage.surface.dispatch({ trigger: 'Change', node: select, id: `${select}:Change`, value: 'empty' });
  assert(tagsSince(before).includes('EmptyState'));
  before = stage.frames.length;
  stage.surface.dispatch({ trigger: 'Change', node: select, id: `${select}:Change`, value: 'error' });
  const failed = stage.since(before);
  assert(tagsSince(before).includes('Banner'));
  const retry = node(failed, 'Button', 'Retry row source');
  assert(retry, 'the failed source has an actionable recovery path');
  stage.surface.dispatch({ trigger: 'Invoke', node: retry, id: `${retry}:Invoke` });
  assert.equal(source.state, 'loading');
  stage.surface.dispatch({ trigger: 'Change', node: entry, id: `${entry}:Change`, value: 'needle' });
  stage.surface.dispatch({ trigger: 'Invoke', node: button, id: `${button}:Invoke`, value: null });
  assert.equal(source.filter, 'needle');
  assert.equal(source.descending, true);
});

test('selection requires immutable current-generation authority and remains bounded', () => {
  const stage = host();
  const source = new LargeRecordSource();
  const first = stage.render(h(LargeDataTableStory, { source }));
  const table = first.patches.find((patch) => patch.Create?.tag === 'DataTable').Create.id;

  stage.surface.dispatch({ trigger: 'Select', node: table, id: `${table}:Select`, rows: [42], collection: {
    source: SOURCE, version: source.version, rows: [{ index: 42, id: '90042' }],
  } });
  for (let index = 0; index < OPERATION_HISTORY_LIMIT + 4; index += 1) {
    stage.surface.dispatch({ trigger: 'Focus', node: table, id: `${table}:Focus`, focused: true });
  }

  const labels = stage.frames.flatMap((frame) => frame.patches)
    .filter((patch) => patch.SetProp?.prop === 'Label')
    .map((patch) => patch.SetProp.value.Text);
  assert.ok(labels.includes('Selected immutable record 90042'));
  assert.ok(labels.includes(`Recent operations (${OPERATION_HISTORY_LIMIT}/${OPERATION_HISTORY_LIMIT})`));
  const final = stage.frames.at(-1).patches.filter((patch) => patch.SetProp?.prop === 'Label');
  assert(final.length <= OPERATION_HISTORY_LIMIT + 1, 'one event must not materialize an unbounded detail list');
  assert(!JSON.stringify(stage.frames).includes('1041'), 'selection details must not serialize all selected rows');

  source.version += 1;
  stage.surface.dispatch({ trigger: 'Select', node: table, id: `${table}:Select`, rows: [42], collection: {
    source: SOURCE, version: source.version - 1, rows: [{ index: 42, id: '90042' }],
  } });
  assert.ok(stage.frames.flatMap((frame) => frame.patches)
    .some((patch) => patch.SetProp?.value?.Text === 'No current record selected'));
});

test('edits bind immutable row and source version, validate, and advance the version', async () => {
  const mutations = [];
  const source = new LargeRecordSource(async (call, argument) => mutations.push([call, argument]));
  assert.deepEqual(await source.edit({ source: SOURCE, version: 1, row: { index: 42, id: '42' }, column: 'name', value: 'renamed' }), { accepted: true });
  assert.equal(source.version, 2);
  assert.equal(source.row(42).cells[1].Text, 'renamed');
  assert.equal(mutations.at(-1)[1].mutation.Length.version, 2);
  assert.deepEqual(await source.edit({ source: SOURCE, version: 1, row: { index: 42, id: '42' }, column: 'name', value: 'stale' }), { accepted: false, reason: 'stale version' });
  assert.equal(source.row(42).cells[1].Text, 'renamed');
  assert.deepEqual(await source.edit({ source: SOURCE, version: 2, row: { index: 42, id: '42' }, column: 'name', value: ' '.repeat(2) }), { accepted: false, reason: 'invalid value' });
});

test('native sort proposals bind source version and refuse stale or unsortable authority', async () => {
  const mutations = [];
  const source = new LargeRecordSource(async (call, argument) => mutations.push([call, argument]));
  assert.deepEqual(await source.sort({ source: SOURCE, version: 1, column: 'name', descending: true }), { accepted: true });
  assert.equal(source.version, 2);
  assert.equal(source.descending, true);
  assert.equal(mutations.at(-1)[1].mutation.Length.version, 2);
  assert.deepEqual(await source.sort({ source: SOURCE, version: 1, column: 'name', descending: false }), { accepted: false, reason: 'stale version' });
  assert.deepEqual(await source.sort({ source: SOURCE, version: 2, column: 'state', descending: false }), { accepted: false, reason: 'unsortable column' });
  assert.equal(source.version, 2);
  assert.equal(source.descending, true);
});

test('the rendered 100k-row table exposes only the declared editable column', () => {
  const stage = host();
  const source = new LargeRecordSource();
  const frame = stage.render(h(LargeDataTableStory, { source }));
  const schema = frame.patches.find((patch) => patch.SetProp?.prop === 'Schema').SetProp.value.Schema;
  assert.deepEqual(schema.filter((column) => column.editable).map((column) => column.key), ['name']);
});

test('the high-density operations story is selectable in the shipped playground', () => {
  const stage = host();
  const source = new LargeRecordSource();
  const first = stage.render(h(Playground, { largeSource: source }));
  const selector = first.patches.find((patch) => patch.Create?.tag === 'Select').Create.id;
  stage.surface.dispatch({ trigger: 'Change', node: selector, id: `${selector}:Change`, value: 'tables' });
  const item = node(stage.frames.flatMap((frame) => frame.patches), 'ListItemButton', 'DataTable');
  assert.ok(item);
  const before = stage.frames.length;
  stage.surface.dispatch({ trigger: 'Invoke', node: item, id: `${item}:Invoke` });
  assert.ok(node(stage.since(before), 'Heading', '100,000 logical records'));
});
