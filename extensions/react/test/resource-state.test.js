import assert from 'node:assert/strict';
import test from 'node:test';
import React from 'react';
import { RESOURCE_STATE_TEXT_BYTE_LIMIT, ResourceState, Text } from '../src/index.js';
import { Surface, reconciler } from '../src/reconciler.js';

const h = React.createElement;
function stage() { const frames = []; const surface = new Surface((frame) => frames.push(frame)); const root = reconciler.createContainer(surface, 0, null, false, null, '', () => {}, null); return { frames, surface, render: (node) => reconciler.updateContainer(node, root, null, null) }; }
function labels(frames) { return frames.flatMap((frame) => frame.patches).filter((patch) => patch.SetProp?.prop === 'Label').map((patch) => patch.SetProp.value.Text); }

test('resource states are mutually exclusive and ready reveals children', () => {
  const view = stage();
  for (const [state, expected] of [['loading', 'Fetching containers'], ['empty', 'No containers'], ['error', 'Host unavailable'], ['ready', 'api running']]) {
    view.render(h(ResourceState, { state, loadingLabel: 'Fetching containers', emptyLabel: 'No containers', error: 'Host unavailable' }, h(Text, { label: 'api running' })));
    const visible = labels(view.frames);
    assert.equal(visible.at(-1), expected);
  }
});

test('error retry dispatches exactly once and bounds host text', () => {
  let retries = 0;
  const view = stage();
  view.render(h(ResourceState, { state: 'error', error: 'é'.repeat(2000), retryLabel: 'Retry inventory', onRetry: () => { retries += 1; } }));
  const patches = view.frames.flatMap((frame) => frame.patches);
  const retry = patches.find((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value.Text === 'Retry inventory').SetProp.id;
  const error = patches.find((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value.Text.startsWith('é')).SetProp.value.Text;
  assert(new TextEncoder().encode(error).byteLength <= RESOURCE_STATE_TEXT_BYTE_LIMIT);
  assert(view.surface.dispatch({ trigger: 'Invoke', node: retry, id: `${retry}:Invoke` }));
  assert.equal(retries, 1);
});

test('invalid state and retry contracts fail closed', () => {
  const view = stage();
  assert.throws(() => view.render(h(ResourceState, { state: 'stale' })), /state must be/);
  assert.throws(() => view.render(h(ResourceState, { state: 'error', onRetry: true })), /onRetry must be/);
});
