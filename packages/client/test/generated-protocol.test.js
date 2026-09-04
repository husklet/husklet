import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import {
  PROTOCOL_CAPABILITIES, PROTOCOL_REPLIES, PROTOCOL_REQUEST_CAPABILITIES, PROTOCOL_TOPICS, encodeRequest,
  validateFailure, validateReply, validateReplyFor, validateRequest, validateSnapshot,
} from '../src/index.js';

test('generated validators follow authoritative request/reply/failure/snapshot roots', () => {
  assert.deepEqual(encodeRequest('workspace_info'), { call: 'workspace_info' });
  assert.deepEqual(validateRequest({ call: 'terminal_write_pane', with: {
    slot: 'p1', generation: 2, revision: 3, contents: [0, 255],
  } }), { call: 'terminal_write_pane', with: {
    slot: 'p1', generation: 2, revision: 3, contents: [0, 255],
  } });
  assert.deepEqual(validateReply({ reply: 'done' }), { reply: 'done' });
  assert.deepEqual(validateReplyFor('workspace_info', { reply: 'workspace', with: { name: 'dev', architecture: 'arm64', image: 'alpine' } }),
    { reply: 'workspace', with: { name: 'dev', architecture: 'arm64', image: 'alpine' } });
  assert.throws(() => validateReplyFor('workspace_info', { reply: 'workspaces', with: [] }), /must be workspace/);
  assert.deepEqual(validateFailure({ error: 'denied', capability: 'terminal-control', detail: 'not granted' }),
    { error: 'denied', capability: 'terminal-control', detail: 'not granted' });
  assert.deepEqual(validateSnapshot({ snapshot: 'containers', of: [] }), { snapshot: 'containers', of: [] });
  assert.throws(() => validateRequest({ call: 'workspace_info', with: { invented: true } }), /absent/);
  assert.throws(() => validateReply({ reply: 'container', with: { id: 'partial' } }), /name must be present/);
  assert.throws(() => validateSnapshot({ snapshot: 'containers', of: [{}] }), /id must be present/);
  assert(PROTOCOL_CAPABILITIES.length >= 20);
  assert.equal(PROTOCOL_REPLIES.event_subscribe, 'done');
  assert.equal(PROTOCOL_REQUEST_CAPABILITIES.container_attach_terminal, 'container-attach');
  assert.equal(PROTOCOL_REQUEST_CAPABILITIES.event_subscribe, null);
  assert.equal(PROTOCOL_TOPICS.find(({ wire }) => wire === 'pane-changes').snapshot, 'pane_changes');
});

test('generated declarations correlate every authoritative request with its exact reply', () => {
  const declarations = fs.readFileSync(new URL('../src/generated-protocol.d.ts', import.meta.url), 'utf8');
  for (const [call, reply] of Object.entries(PROTOCOL_REPLIES)) {
    assert.match(declarations, new RegExp(`"${call}": Extract<WireReply, \\{ reply: "${reply}" \\}>;`));
  }
  assert.match(declarations, /WireRequestParameters<C extends WireCall>/);
  assert.match(declarations, /WireReplyFor<C extends WireCall> = WireReplyByCall\[C\]/);
});

test('integer widths and the cross-language lossless boundary are enforced before framing', () => {
  const safe = Number.MAX_SAFE_INTEGER;
  assert.deepEqual(validateRequest({ call: 'extension_acquisition_cancel', with: { job: 'job-1', revision: safe } }),
    { call: 'extension_acquisition_cancel', with: { job: 'job-1', revision: safe } });
  assert.throws(() => validateRequest({ call: 'extension_acquisition_cancel', with: { job: 'job-1', revision: safe + 1 } }),
    /integer from 0 through 9007199254740991/);
  assert.throws(() => validateRequest({ call: 'terminal_write_pane', with: {
    slot: 'p1', generation: 1, revision: 1, contents: [256],
  } }), /integer from 0 through 255/);
  assert.throws(() => validateSnapshot({ snapshot: 'pane_changes', of: {
    slot: 'p1', kind: 'terminal', generation: safe + 1, revision: 1, coalesced: 0,
  } }), /integer from 0 through 9007199254740991/);
});
