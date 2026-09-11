import assert from 'node:assert/strict';
import test from 'node:test';
import { semanticText, semanticXml } from '../dist/index.js';

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

test('semantic text distinguishes host truncation from bounded XML projection truncation', () => {
  const node = (id) => ({
    id,
    role: 'button',
    label: 'label'.repeat(80),
    value: null,
    disabled: false,
    destructive: false,
    actions: ['invoke'],
    children: [],
  });
  const projected = semanticText({
    slot: 'surface',
    generation: 4,
    revision: 9,
    truncated: false,
    root: { ...node(0), children: Array.from({ length: 255 }, (_, index) => node(index + 1)) },
  });
  assert.equal(projected.complete, false);
  assert.equal(projected.sourceTruncated, false);
  assert.equal(projected.projectionTruncated, true);
  assert.match(projected.text, /<truncated\/>/);
  assert(new TextEncoder().encode(projected.text).byteLength <= 64 * 1024);

  const source = semanticText({
    slot: 'surface',
    generation: 4,
    revision: 10,
    truncated: true,
    root: node(0),
  });
  assert.equal(source.complete, false);
  assert.equal(source.sourceTruncated, true);
  assert.equal(source.projectionTruncated, false);
});
