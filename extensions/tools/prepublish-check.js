import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const read = (name) => JSON.parse(fs.readFileSync(path.join(root, name, 'package.json')));
const client = read('client');
const clientStarter = JSON.parse(fs.readFileSync(path.join(root, 'client/examples/starter/package.json')));
const react = read('react');
const starter = JSON.parse(fs.readFileSync(path.join(root, 'react/examples/starter/package.json')));
const expected = process.env.RELEASE_VERSION ?? client.version;
for (const manifest of [client, react]) {
  assert.equal(manifest.version, expected);
  assert.deepEqual(manifest.publishConfig, { access: 'public', provenance: true });
}
assert.equal(react.dependencies['@husklet/client'], expected);
assert.equal(clientStarter.dependencies['@husklet/client'], expected);
assert.equal(starter.dependencies['@husklet/client'], expected);
assert.equal(starter.dependencies['@husklet/react'], expected);
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-pack-'));
try {
  const pack = (name) => {
    const result = JSON.parse(execFileSync('npm', ['pack', '--json', '--ignore-scripts', '--pack-destination', scratch], { cwd: path.join(root, name), encoding: 'utf8' }))[0];
    assert.equal(result.name, `@husklet/${name}`);
    assert(result.integrity.startsWith('sha512-'));
    return path.join(scratch, result.filename);
  };
  const clientTarball = pack('client');
  const reactTarball = pack('react');
  const consumer = path.join(scratch, 'consumer');
  fs.mkdirSync(consumer);
  fs.writeFileSync(path.join(consumer, 'package.json'), JSON.stringify({ private: true, type: 'module' }));
  execFileSync('npm', ['install', '--ignore-scripts', '--no-audit', '--no-fund', clientTarball, reactTarball, 'react@18.3.1'], { cwd: consumer });
  execFileSync(process.execPath, ['--input-type=module', '--eval', "import { Session, workspace } from '@husklet/client'; import { Session as ReactSession } from '@husklet/react'; if (Session !== ReactSession || typeof workspace !== 'function') process.exit(1)"], { cwd: consumer });
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}
