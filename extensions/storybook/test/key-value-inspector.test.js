import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { Playground } from '../dist/app.js';
import {
  KEY_VALUE_RECORDS,
  KEY_VALUE_SOURCE,
  KEY_VALUE_STORY,
  KEY_VALUE_WINDOW_LIMIT,
  KeyValueInspectorStory,
  KeyValueSource,
} from '../dist/key-value-inspector.js';
import { host } from './host.js';

function labelled(patches, tag, label) {
  let candidate = null;
  for (const patch of patches) {
    if (patch.Create?.tag === tag) candidate = patch.Create.id;
    if (candidate !== null && patch.SetProp?.id === candidate && patch.SetProp.prop === 'Label'
      && patch.SetProp.value.Text === label) return candidate;
  }
  return null;
}

test('key/value inspection keeps logical metadata and wire windows independently bounded', () => {
  const source = new KeyValueSource();
  const frame = host().render(h(KeyValueInspectorStory, { source }));
  assert.equal(frame.patches.filter((patch) => patch.Create?.tag === 'KeyValueTable').length, 1);
  assert.equal(frame.patches.filter((patch) => patch.Create?.tag === 'TableRow').length, 0);
  const window = source.answer({ source: KEY_VALUE_SOURCE, version: 1, id: 9, range: { start: 4, count: 10_000 } });
  assert.equal(window.rows.length, KEY_VALUE_WINDOW_LIMIT);
  assert.equal(source.generated, KEY_VALUE_WINDOW_LIMIT);
  assert.equal(KEY_VALUE_RECORDS, 256);
  assert.deepEqual(window.rows[0].cells, [{ Text: 'manifest.field.4' }, { Code: 'value-4' }]);
  assert.equal(source.answer(null), null);
  assert.equal(source.answer({ source: KEY_VALUE_SOURCE, version: 1, id: 10, range: { start: 0, count: -1 } }), null);
});

test('key/value inspector is selectable and refresh is visibly acknowledged', () => {
  const source = new KeyValueSource();
  const stage = host();
  const first = stage.render(h(KeyValueInspectorStory, { source }));
  const refresh = labelled(first.patches, 'Button', 'Refresh metadata');
  stage.surface.dispatch({ trigger: 'Invoke', node: refresh, id: `${refresh}:Invoke` });
  assert.ok(stage.frames.flatMap((frame) => frame.patches)
    .some((patch) => patch.SetProp?.value?.Text === 'Metadata refreshed 1 time.'));
  assert.ok(labelled(host().render(h(Playground)).patches, 'ListItemButton', KEY_VALUE_STORY));
});
