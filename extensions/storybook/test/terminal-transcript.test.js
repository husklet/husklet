import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { Playground } from '../dist/app.js';
import { TERMINAL_TRANSCRIPT_STORY, TerminalTranscriptStory } from '../dist/terminal-transcript.js';
import { host } from './host.js';

function labels(patches) {
  return patches.filter((patch) => patch.SetProp?.prop === 'Label').map((patch) => patch.SetProp.value.Text);
}

test('terminal transcript story composes a selectable cursor-bearing bounded inspection flow', () => {
  const stage = host();
  const frame = stage.render(h(TerminalTranscriptStory));
  const text = labels(frame.patches);
  assert.ok(text.some((label) => label.includes('422 12:04:08.415 $ ▉')));
  assert.ok(text.some((label) => label.includes('413 earlier lines omitted')));
  assert.ok(text.includes('Copy visible'));
  assert.ok(frame.patches.some((patch) => patch.SetProp?.prop === 'Destructive' && patch.SetProp.value.Flag));

  const browser = host();
  const catalogue = browser.render(h(Playground));
  assert.ok(labels(catalogue.patches).includes(TERMINAL_TRANSCRIPT_STORY));
});
