import assert from 'node:assert/strict';
import test from 'node:test';
import { semanticXml } from '../src/index.js';

test('semantic XML is deterministic, escaped, redacted, and bounded', () => {
  const xml = semanticXml({ slot: 'pane<&', generation: 2, revision: 3, truncated: false, root: {
    id: 1, role: 'password', label: '<Secret>', value: 'never-print-me', disabled: false,
    destructive: true, actions: ['invoke'], children: [],
  } });
  assert.match(xml, /^<pane slot="pane&lt;&amp;" generation="2" revision="3"/);
  assert.match(xml, /<label>&lt;Secret&gt;<\/label>/);
  assert.match(xml, /<value>\[redacted\]<\/value>/);
  assert(!xml.includes('never-print-me'));
  assert(new TextEncoder().encode(xml).byteLength <= 64 * 1024);
});
