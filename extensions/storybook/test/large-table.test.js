import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { LargeDataTableStory, LargeRecordSource, LOGICAL_ROWS, WINDOW_LIMIT, SOURCE } from '../src/large-table.js';
import { host } from './host.js';

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
  assert.equal(source.generated, WINDOW_LIMIT);

  await source.configure({ descending: true });
  const sorted = source.answer({ ...request, id: 5, version: 2, range: { start: 0, count: 8 } });
  assert.equal(sorted.rows[0].id, LOGICAL_ROWS - 1);
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
  assert.equal(failed.rows[0].cells[2].Badge.tone, 'danger');
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
  assert(tagsSince(before).includes('Banner'));
  stage.surface.dispatch({ trigger: 'Change', node: entry, id: `${entry}:Change`, value: 'needle' });
  stage.surface.dispatch({ trigger: 'Invoke', node: button, id: `${button}:Invoke`, value: null });
  assert.equal(source.filter, 'needle');
  assert.equal(source.descending, true);
});
