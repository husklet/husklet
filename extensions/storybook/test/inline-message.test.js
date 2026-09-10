import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';

import { enums } from '../dist/catalogue.js';
import { componentPages } from '../dist/component-pages.js';
import { InlineMessageWorkbench } from '../dist/inline-message.js';
import { host } from './host.js';

function created(patches, tag) {
  return patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id);
}

function props(patches, id) {
  return Object.fromEntries(
    patches
      .filter((patch) => patch.SetProp?.id === id)
      .map((patch) => [patch.SetProp.prop, patch.SetProp.value]),
  );
}

test('InlineMessage owns a one-component reference for every generated tone', () => {
  const frame = host().render(h(InlineMessageWorkbench));
  const headings = created(frame.patches, 'Heading').map(
    (id) => props(frame.patches, id).Label?.Text,
  );
  const messages = created(frame.patches, 'InlineMessage').map((id) => props(frame.patches, id));

  assert.equal(componentPages.InlineMessage, InlineMessageWorkbench);
  assert.deepEqual(headings.slice(0, 6), [
    'Inline Message',
    'Overview',
    'Tones',
    'Wrapping',
    'Accessibility',
    'API',
  ]);
  for (const { wire } of enums.Tone) {
    assert(messages.some((message) => message.Tone?.Tone === wire));
  }
  assert(
    messages.some(
      (message) =>
        message.Label?.Text === 'Connected through the workspace network.' &&
        message.Icon?.Text === 'network-workgroup-symbolic',
    ),
    'the reference omits the explicit icon override',
  );
  assert(created(frame.patches, 'TableRow').length > 0, 'the generated public API is missing');
});
