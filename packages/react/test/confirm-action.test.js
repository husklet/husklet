import assert from 'node:assert/strict';
import test from 'node:test';
import React from 'react';

import { CONFIRM_ACTION_TEXT_BYTE_LIMIT, ConfirmAction } from '../src/index.js';
import { Surface, reconciler } from '../src/reconciler.js';

const h = React.createElement;

function host() {
  const frames = [];
  const surface = new Surface((frame) => frames.push(frame));
  const container = reconciler.createContainer(surface, 0, null, false, null, '', () => {}, null);
  return {
    frames,
    surface,
    render(element) { reconciler.updateContainer(element, container, null, null); },
    since(index) { return frames.slice(index).flatMap((frame) => frame.patches); },
  };
}

function labelled(patches, label) {
  const tags = new Map();
  let found;
  for (const patch of patches) {
    if (patch.Create) tags.set(patch.Create.id, patch.Create.tag);
    if (patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label) {
      found = { id: patch.SetProp.id, tag: tags.get(patch.SetProp.id) };
    }
  }
  return found;
}

function prop(patches, id, name) {
  return patches.findLast((patch) => patch.SetProp?.id === id && patch.SetProp.prop === name)?.SetProp.value;
}

function invoke(stage, patches, label) {
  const target = labelled(patches, label);
  assert.ok(target, `missing ${label}`);
  assert.equal(stage.surface.dispatch({ trigger: 'Invoke', node: target.id, id: `${target.id}:Invoke` }), true);
  return target;
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

const settle = () => new Promise((resolve) => setImmediate(resolve));

test('confirmation is separate and only its final action is destructive', () => {
  const calls = [];
  const stage = host();
  stage.render(h(ConfirmAction, {
    authorityKey: 'volume:cache:g7', label: 'Remove volume', confirmLabel: 'Confirm removal',
    question: 'Remove cache generation 7?', onConfirm: (key) => calls.push(key),
  }));
  const initial = stage.since(0);
  const reveal = labelled(initial, 'Remove volume');
  assert.equal(reveal.tag, 'Button');
  assert.equal(prop(initial, reveal.id, 'Destructive'), undefined);

  const before = stage.frames.length;
  invoke(stage, initial, 'Remove volume');
  const confirmation = stage.since(before);
  const final = labelled(confirmation, 'Confirm removal');
  assert.deepEqual(prop(confirmation, final.id, 'Destructive'), { Flag: true });
  assert.ok(labelled(confirmation, 'Remove cache generation 7?'));
  const created = confirmation.filter((patch) => patch.Create).map((patch) => patch.Create.tag);
  assert.ok(created.length <= 8, `confirmation materialized ${created.length} nodes`);
  assert.ok(created.every((tag) => ['Column', 'Text', 'Row', 'Button', 'Spinner', 'InlineMessage'].includes(tag)),
    `confirmation escaped the native vocabulary: ${created}`);
  assert.deepEqual(calls, []);
});

test('pending disables confirmation and cancellation, then success collapses exactly once', async () => {
  const work = deferred();
  let calls = 0;
  const stage = host();
  stage.render(h(ConfirmAction, {
    authorityKey: 'container:abc', label: 'Kill', confirmLabel: 'Confirm kill', pendingLabel: 'Killing…',
    question: 'Kill immutable container abc?', onConfirm: () => { calls += 1; return work.promise; },
  }));
  invoke(stage, stage.since(0), 'Kill');
  const open = stage.frames.length;
  invoke(stage, stage.since(0), 'Confirm kill');
  const pending = stage.since(open);
  const all = stage.frames.flatMap((frame) => frame.patches);
  const final = labelled(all, 'Killing…');
  const cancel = labelled(all, 'Cancel');
  assert.deepEqual(prop(all, final.id, 'Enabled'), { Flag: false });
  assert.deepEqual(prop(all, cancel.id, 'Enabled'), { Flag: false });
  assert.ok(pending.some((patch) => patch.Create?.tag === 'Spinner'));
  stage.surface.dispatch({ trigger: 'Invoke', node: final.id, id: `${final.id}:Invoke` });
  assert.equal(calls, 1);
  work.resolve();
  await settle();
  assert.ok(labelled(stage.frames.flatMap((frame) => frame.patches), 'Kill'));
  assert.equal(calls, 1);
});

test('failure is bounded and retryable while cancel never invokes', async () => {
  let attempts = 0;
  let cancelled = 0;
  const stage = host();
  stage.render(h(ConfirmAction, {
    authorityKey: 'image:sha256:one', label: 'Remove', confirmLabel: 'Confirm remove',
    question: 'Remove immutable image?', onCancel: () => { cancelled += 1; },
    onConfirm: async () => { attempts += 1; throw new Error('é'.repeat(2000)); },
  }));
  invoke(stage, stage.since(0), 'Remove');
  invoke(stage, stage.frames.flatMap((frame) => frame.patches), 'Confirm remove');
  await settle();
  const all = stage.frames.flatMap((frame) => frame.patches);
  const error = all.findLast((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text?.startsWith('é'));
  assert.ok(error);
  assert.ok(new TextEncoder().encode(error.SetProp.value.Text).byteLength <= CONFIRM_ACTION_TEXT_BYTE_LIMIT);
  invoke(stage, all, 'Confirm remove');
  await settle();
  assert.equal(attempts, 2);
  invoke(stage, stage.frames.flatMap((frame) => frame.patches), 'Cancel');
  assert.equal(attempts, 2);
  assert.equal(cancelled, 1);
});

test('a changed authority invalidates an open or pending confirmation', async () => {
  const work = deferred();
  const calls = [];
  const stage = host();
  const view = (key) => h(ConfirmAction, {
    authorityKey: key, label: 'Delete', confirmLabel: 'Confirm delete', question: `Delete ${key}?`,
    onConfirm: (authorityKey) => { calls.push(authorityKey); return work.promise; },
  });
  stage.render(view('generation-1'));
  invoke(stage, stage.since(0), 'Delete');
  const stale = labelled(stage.frames.flatMap((frame) => frame.patches), 'Confirm delete');
  stage.render(view('generation-2'));
  stage.surface.dispatch({ trigger: 'Invoke', node: stale.id, id: `${stale.id}:Invoke` });
  assert.deepEqual(calls, []);

  invoke(stage, stage.frames.flatMap((frame) => frame.patches), 'Delete');
  invoke(stage, stage.frames.flatMap((frame) => frame.patches), 'Confirm delete');
  assert.deepEqual(calls, ['generation-2']);
  stage.render(view('generation-3'));
  work.reject(new Error('late failure'));
  await settle();
  const later = stage.frames.flatMap((frame) => frame.patches);
  assert.ok(labelled(later, 'Delete'));
  assert.equal(later.some((patch) => patch.SetProp?.value?.Text === 'late failure'), false);
});

test('invalid authority and callback contracts fail closed', () => {
  const stage = host();
  assert.throws(() => stage.render(h(ConfirmAction, { authorityKey: '', label: 'Delete', confirmLabel: 'Confirm', question: '?' , onConfirm() {} })), /nonblank/);
  assert.throws(() => stage.render(h(ConfirmAction, { authorityKey: 'valid', label: 'Delete', confirmLabel: 'Confirm', question: '?' })), /onConfirm/);
});
