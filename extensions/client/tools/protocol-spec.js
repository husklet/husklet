import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { requestCapability } from '../src/index.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const repository = path.resolve(here, '../../..');
const source = fs.readFileSync(path.join(repository, 'src/workspaces/hl-extension/src/request.rs'), 'utf8');
const body = source.match(/pub enum Request \{(?<body>[\s\S]*?)\n\}/)?.groups?.body;
assert(body, 'Rust Request enum must remain discoverable');
const snake = (name) => name.replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase();
const requests = [...body.matchAll(/^    ([A-Z][A-Za-z0-9]+)(?:\s*\{|\s*,)/gm)].map(([, name]) => snake(name));
assert(requests.length > 50, 'protocol extraction must not silently become vacuous');
const classify = (call) => {
  if (call === 'event_subscribe' || call === 'event_unsubscribe') return 'topic-dependent';
  return requestCapability(call);
};
const classified = Object.fromEntries(requests.map((call) => [call, classify(call)]));
const spec = { protocol: 1, transport: 'unix-length-prefixed-full-duplex', requests: classified };
const output = JSON.stringify(spec, null, 2) + '\n';
const target = path.join(here, '..', 'protocol.json');
if (process.argv.includes('--write')) fs.writeFileSync(target, output);
else assert.equal(fs.readFileSync(target, 'utf8'), output, 'client protocol manifest is stale; run npm run protocol:generate');
