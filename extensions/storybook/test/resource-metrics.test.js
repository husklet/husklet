import assert from 'node:assert/strict';
import test from 'node:test';

import { SAMPLE_LIMIT, boundedSamples } from '../dist/resource-metrics.js';

test('resource trends keep only the newest finite bounded samples', () => {
  const values = [...Array.from({ length: 80 }, (_, index) => index), Number.NaN];
  const samples = boundedSamples(values).split(',').map(Number);
  assert.equal(samples.length, SAMPLE_LIMIT);
  assert.equal(samples[0], 16);
  assert.equal(samples.at(-1), 79);
});
