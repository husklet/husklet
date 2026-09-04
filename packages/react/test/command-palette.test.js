import assert from 'node:assert/strict';
import test from 'node:test';
import React from 'react';

import { CommandPaletteView, COMMAND_PALETTE_ITEM_LIMIT, filterCommands } from '../src/index.js';
import { Surface, reconciler } from '../src/reconciler.js';

function stage() {
  const frames = [];
  const surface = new Surface((frame) => frames.push(frame));
  const container = reconciler.createContainer(surface, 0, null, false, null, '', () => {}, null);
  return { frames, surface, render(element) { reconciler.updateContainer(element, container, null, null); } };
}

function node(patches, tag, label) {
  let candidate;
  for (const patch of patches) {
    if (patch.Create?.tag === tag) candidate = patch.Create.id;
    if (candidate && patch.SetProp?.id === candidate && patch.SetProp.prop === 'Label' && patch.SetProp.value.Text === label) return candidate;
  }
}

test('command palette fuzzy filters with stable identity and hard bounds', () => {
  const commands = Array.from({ length: COMMAND_PALETTE_ITEM_LIMIT + 10 }, (_, index) => ({ id: `c${index}`, title: `Command ${index}` }));
  assert.equal(filterCommands(commands, '').length, COMMAND_PALETTE_ITEM_LIMIT);
  assert.deepEqual(filterCommands([
    { id: 'files', title: 'Open file', keywords: ['picker'] },
    { id: 'terminal', title: 'New terminal' },
  ], 'ntrm').map(({ id }) => id), ['terminal']);
});

test('command palette renders grouped disabled/destructive semantics and keyboard selection', () => {
  const chosen = [];
  const host = stage();
  host.render(React.createElement(CommandPaletteView, { commands: [
    { id: 'open', title: 'Open terminal', group: 'Workspace', shortcut: '⌘T' },
    { id: 'locked', title: 'Unavailable', group: 'Workspace', disabled: true },
    { id: 'remove', title: 'Remove workspace', group: 'Danger', destructive: true },
  ], onSelect: (command) => chosen.push(command.id) }));
  const patches = host.frames.flatMap(({ patches }) => patches);
  assert.ok(node(patches, 'ListSubheader', 'Workspace'));
  const locked = node(patches, 'Button', 'Unavailable');
  const remove = node(patches, 'Button', 'Remove workspace');
  assert.ok(patches.some((patch) => patch.SetProp?.id === locked && patch.SetProp.prop === 'Enabled' && !patch.SetProp.value.Flag));
  assert.ok(patches.some((patch) => patch.SetProp?.id === remove && patch.SetProp.prop === 'Destructive' && patch.SetProp.value.Flag));
  const input = patches.find((patch) => patch.Create?.tag === 'CommandPalette').Create.id;
  host.surface.dispatch({ trigger: 'Key', node: input, id: `${input}:Key`, key: 'ArrowDown' });
  host.surface.dispatch({ trigger: 'Key', node: input, id: `${input}:Key`, key: 'Enter' });
  assert.deepEqual(chosen, ['remove']);
});

test('command palette displays a truthful empty state', () => {
  const host = stage();
  host.render(React.createElement(CommandPaletteView, { commands: [], emptyLabel: 'Nothing here' }));
  const patches = host.frames.flatMap(({ patches }) => patches);
  assert.ok(node(patches, 'EmptyState', 'Nothing here'));
});
