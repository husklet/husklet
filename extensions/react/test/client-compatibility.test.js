import assert from 'node:assert/strict';
import test from 'node:test';

import { Session as ClientSession, workspace as clientWorkspace } from '@husklet/client';
import { Session, workspace } from '../src/index.js';

test('React preserves the framework-neutral client exports by identity', () => {
  assert.equal(Session, ClientSession);
  assert.equal(workspace, clientWorkspace);
});
