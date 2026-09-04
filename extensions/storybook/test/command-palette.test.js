import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { Playground } from '../dist/app.js';
import { COMMAND_PALETTE_STORY, CommandPaletteStory } from '../dist/command-palette.js';
import { host } from './host.js';

function labels(patches) {
  return patches.filter((patch) => patch.SetProp?.prop === 'Label').map((patch) => patch.SetProp.value.Text);
}

test('command palette story exposes grouped authority and destructive metadata', () => {
  const stage = host();
  const frame = stage.render(h(CommandPaletteStory));
  const text = labels(frame.patches);
  assert.ok(text.includes('Workspace') && text.includes('Containers') && text.includes('Danger'));
  assert.ok(text.includes('New terminal  ⌘T'));
  assert.ok(frame.patches.some((patch) => patch.SetProp?.prop === 'Destructive' && patch.SetProp.value.Flag));
  assert.ok(frame.patches.some((patch) => patch.SetProp?.prop === 'Enabled' && !patch.SetProp.value.Flag));
  const browser = host();
  assert.ok(labels(browser.render(h(Playground)).patches).includes(COMMAND_PALETTE_STORY));
});
