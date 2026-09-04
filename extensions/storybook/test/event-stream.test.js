import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import {
  EVENT_RETENTION_LIMIT,
  EVENT_SOURCE,
  EVENT_STREAM_STORY,
  EVENT_WINDOW_LIMIT,
  EventStreamStory,
  TimelineSource,
} from '../dist/event-stream.js';
import { Playground } from '../dist/app.js';
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

test('event timeline keeps logical history and materialized windows independently bounded', () => {
  const source = new TimelineSource();
  const stage = host();
  const frame = stage.render(h(EventStreamStory, { source }));
  assert.equal(frame.patches.filter((patch) => patch.Create?.tag === 'EventStream').length, 1);
  assert.equal(frame.patches.filter((patch) => patch.Create?.tag === 'TableRow').length, 0);

  const window = source.answer({ source: EVENT_SOURCE, version: 1, id: 7, range: { start: 40, count: 1_000 } });
  assert.equal(window.rows.length, EVENT_WINDOW_LIMIT);
  assert.equal(source.generated, EVENT_WINDOW_LIMIT);
  assert.equal(EVENT_RETENTION_LIMIT, 10_000);
});

test('event timeline is selectable and its acknowledgement rerenders visibly', () => {
  const source = new TimelineSource();
  const stage = host();
  const first = stage.render(h(EventStreamStory, { source }));
  const action = labelled(first.patches, 'Button', 'Acknowledge newest');
  stage.surface.dispatch({ trigger: 'Invoke', node: action, id: `${action}:Invoke` });
  assert.ok(stage.frames.flatMap((frame) => frame.patches)
    .some((patch) => patch.SetProp?.value?.Text === 'Acknowledged newest event 1 time.'));

  const browser = host();
  assert.ok(labelled(browser.render(h(Playground)).patches, 'ListItemButton', EVENT_STREAM_STORY));
});
