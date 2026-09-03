import assert from 'node:assert/strict';
import test from 'node:test';
import {
  PROTOCOL_CAPABILITIES, PROTOCOL_TOPICS, encodeRequest,
  validateFailure, validateReply, validateRequest, validateSnapshot,
} from '../src/index.js';

test('generated validators follow authoritative request/reply/failure/snapshot roots', () => {
  assert.deepEqual(encodeRequest('workspace_info'), { call: 'workspace_info' });
  assert.deepEqual(validateRequest({ call: 'terminal_write_pane', with: { slot: 'p1', contents: [0, 255] } }),
    { call: 'terminal_write_pane', with: { slot: 'p1', contents: [0, 255] } });
  assert.deepEqual(validateReply({ reply: 'done' }), { reply: 'done' });
  assert.deepEqual(validateFailure({ error: 'denied', capability: 'terminal-control', detail: 'not granted' }),
    { error: 'denied', capability: 'terminal-control', detail: 'not granted' });
  assert.deepEqual(validateSnapshot({ snapshot: 'containers', of: [] }), { snapshot: 'containers', of: [] });
  assert.throws(() => validateRequest({ call: 'workspace_info', with: { invented: true } }), /absent/);
  assert.throws(() => validateReply({ reply: 'container', with: { id: 'partial' } }), /name must be present/);
  assert.throws(() => validateSnapshot({ snapshot: 'containers', of: [{}] }), /id must be present/);
  assert(PROTOCOL_CAPABILITIES.length >= 20);
  assert.equal(PROTOCOL_TOPICS.find(({ wire }) => wire === 'pane-changes').snapshot, 'pane_changes');
});
