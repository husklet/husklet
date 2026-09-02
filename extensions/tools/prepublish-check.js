import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const repository = path.resolve(root, '..');
const readJson = (name) => JSON.parse(fs.readFileSync(path.join(root, name, 'package.json')));
const react = readJson('react');
const mcp = readJson('mcp');
const expected = process.env.RELEASE_VERSION ?? react.version;

for (const manifest of [react, mcp]) {
  assert.equal(manifest.version, expected, `${manifest.name} version must equal ${expected}`);
  assert.deepEqual(manifest.publishConfig, { access: 'public', provenance: true });
  assert.equal(manifest.private, undefined, `${manifest.name} must not be private`);
  assert.equal(manifest.license, 'MIT');
  assert.equal(manifest.repository?.directory, manifest.name === '@husklet/react' ? 'extensions/react' : 'extensions/mcp');
}
assert.equal(mcp.dependencies['@husklet/react'], expected, 'MCP must depend on the exact paired React release');

// Trusted publishing is partly a workflow property, so keep its security-critical
// contract in the same locally runnable check as the package metadata.
const workflow = fs.readFileSync(path.join(repository, '.github/workflows/release.yml'), 'utf8');
const job = workflow.match(/^  react-package:\n(?<body>[\s\S]*?)(?=^  [a-z][a-z0-9-]+:\n)/m)?.groups?.body;
assert(job, 'release workflow must contain the react-package job');
assert.match(job, /permissions:\n\s+contents: read\n\s+id-token: write\n/);
assert.match(job, /environment: npm\n/);
const reactPublish = 'npm publish --workspace @husklet/react --access public --provenance';
const mcpPublish = 'npm publish --workspace @husklet/mcp --access public --provenance';
assert.equal(job.split(reactPublish).length - 1, 1, 'React must have one public provenance publish');
assert.equal(job.split(mcpPublish).length - 1, 1, 'MCP must have one public provenance publish');
assert(job.indexOf(reactPublish) < job.indexOf(mcpPublish), 'React must publish before its MCP dependent');

const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-release-pack-'));
try {
  const pack = (workspace) => {
    const result = JSON.parse(execFileSync('npm', [
      'pack', '--json', '--ignore-scripts', '--pack-destination', scratch,
    ], { cwd: path.join(root, workspace), encoding: 'utf8' }))[0];
    assert.equal(result.name, `@husklet/${workspace}`);
    assert.equal(result.version, expected);
    assert(result.integrity?.startsWith('sha512-'), `${result.name} must have package integrity`);
    return path.join(scratch, result.filename);
  };
  const reactTarball = pack('react');
  const mcpTarball = pack('mcp');

  const consumer = path.join(scratch, 'consumer');
  fs.mkdirSync(consumer);
  fs.writeFileSync(path.join(consumer, 'package.json'), JSON.stringify({ private: true, type: 'module' }));
  execFileSync('npm', [
    'install', '--ignore-scripts', '--no-audit', '--no-fund',
    reactTarball, mcpTarball, 'react@18.3.1',
  ], { cwd: consumer, stdio: 'pipe' });

  execFileSync('npm', ['ls', '--all'], { cwd: consumer, stdio: 'pipe' });
  execFileSync(process.execPath, ['--input-type=module', '--eval', `
    import reactPackage from '@husklet/react/package.json' with { type: 'json' };
    import mcpPackage from '@husklet/mcp/package.json' with { type: 'json' };
    import { Session } from '@husklet/react';
    import { createServer, tools } from '@husklet/mcp';
    if (reactPackage.version !== ${JSON.stringify(expected)}) process.exit(1);
    if (mcpPackage.version !== ${JSON.stringify(expected)}) process.exit(1);
    if (typeof Session !== 'function' || typeof createServer !== 'function' || typeof tools !== 'function') process.exit(1);
  `], { cwd: consumer, stdio: 'pipe' });

  const lock = JSON.parse(fs.readFileSync(path.join(consumer, 'package-lock.json')));
  for (const name of ['@husklet/react', '@husklet/mcp']) {
    const entry = lock.packages[`node_modules/${name}`];
    assert.equal(entry.version, expected);
    assert.match(entry.resolved, /^file:/, `${name} must resolve from the exact local tarball in this smoke`);
    assert(entry.integrity?.startsWith('sha512-'));
  }
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}
