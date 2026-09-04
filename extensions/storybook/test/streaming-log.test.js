import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { LOG_CHUNK_LIMIT, STREAMING_LOG_STORY, StreamingLogStory } from '../dist/streaming-log.js';
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

test('streaming log publishes bounded deltas and is selectable', () => {
  const stage = host();
  const first = stage.render(h(StreamingLogStory));
  const append = labelled(first.patches, 'Button', 'Append batch');
  assert.ok(append);
  const before = stage.frames.length;
  stage.surface.dispatch({ trigger: 'Invoke', node: append, id: `${append}:Invoke` });
  const values = stage.since(before).filter((patch) => patch.SetProp?.prop === 'Value');
  assert.equal(values.length, 1, 'one append sends one delta rather than the retained history');
  assert(values[0].SetProp.value.Text.length <= LOG_CHUNK_LIMIT);

  const browser = host();
  const frame = browser.render(h(Playground));
  assert.ok(labelled(frame.patches, 'ListItemButton', STREAMING_LOG_STORY));
});
