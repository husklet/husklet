import assert from 'node:assert/strict';
import test from 'node:test';
import React from 'react';

import { Surface, reconciler } from '../src/reconciler.js';
import { TerminalTranscript, TERMINAL_TRANSCRIPT_LINE_LIMIT } from '../src/index.js';

function stage() {
  const frames = [];
  const surface = new Surface((frame) => frames.push(frame));
  const container = reconciler.createContainer(surface, 0, null, false, null, '', () => {}, null);
  return {
    frames, surface,
    render(element) { reconciler.updateContainer(element, container, null, null); },
  };
}

test('terminal transcript bounds its tail and exposes cursor and selection actions', async () => {
  const selected = [];
  const host = stage();
  const lines = Array.from({ length: TERMINAL_TRANSCRIPT_LINE_LIMIT + 20 }, (_, index) => ({
    id: `line-${index}`, number: index + 1, text: index === 275 ? 'ready' : `line ${index}`, stream: index === 274 ? 'stderr' : 'stdout',
  }));
  host.render(React.createElement(TerminalTranscript, {
    lines, lineNumbers: true, cursor: { line: 276, column: 5 }, truncated: true, droppedLines: 20,
    onSelect: (line) => selected.push(line.id), actions: [{ label: 'Copy visible', onInvoke: () => selected.push('copy') }],
  }));
  const patches = host.frames.flatMap((frame) => frame.patches);
  const rows = patches.filter((patch) => patch.Create?.tag === 'ListItemButton');
  assert.equal(rows.length, TERMINAL_TRANSCRIPT_LINE_LIMIT);
  assert.ok(patches.some((patch) => patch.SetProp?.value?.Text?.includes('ready▉')));
  assert.ok(patches.some((patch) => patch.SetProp?.value?.Text?.includes('20 earlier lines omitted')));
  const last = rows.at(-1).Create.id;
  host.surface.dispatch({ trigger: 'Invoke', node: last, id: `${last}:Invoke` });
  assert.deepEqual(selected, ['line-275']);
});

test('terminal transcript clips oversized UTF-8 lines without splitting characters', async () => {
  const host = stage();
  host.render(React.createElement(TerminalTranscript, { lines: ['🧪'.repeat(600)] }));
  const labels = host.frames.flatMap((frame) => frame.patches).filter((patch) => patch.SetProp?.prop === 'Label');
  const line = labels.find((patch) => patch.SetProp.value.Text.includes('🧪'));
  assert.equal(new TextEncoder().encode(line.SetProp.value.Text).byteLength, 2048);
  assert.equal(line.SetProp.value.Text.endsWith('🧪'), true);
});
