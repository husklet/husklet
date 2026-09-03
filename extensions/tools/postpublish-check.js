import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const version = process.env.RELEASE_VERSION;
assert.match(version ?? '', /^[0-9]+\.[0-9]+\.[0-9]+$/, 'RELEASE_VERSION must have exact X.Y.Z form');

const reactSpec = process.env.HUSKLET_POSTPUBLISH_REACT_SPEC ?? `@husklet/react@${version}`;
const mcpSpec = process.env.HUSKLET_POSTPUBLISH_MCP_SPEC ?? `@husklet/mcp@${version}`;
const localOverride = process.env.HUSKLET_POSTPUBLISH_REACT_SPEC || process.env.HUSKLET_POSTPUBLISH_MCP_SPEC;
const metadataOverride = process.env.HUSKLET_POSTPUBLISH_METADATA;
const attempts = localOverride ? 1 : 12;
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-postpublish-'));
const consumer = path.join(scratch, 'consumer');
fs.mkdirSync(consumer);
fs.writeFileSync(path.join(consumer, 'package.json'), JSON.stringify({ private: true, type: 'module' }));

try {
  const metadata = metadataOverride
    ? JSON.parse(metadataOverride)
    : [reactSpec, mcpSpec].map((spec) => JSON.parse(execFileSync('npm', [
      'view', spec, '--json', '--registry=https://registry.npmjs.org',
    ], { encoding: 'utf8' })));
  for (const [index, name] of ['@husklet/react', '@husklet/mcp'].entries()) {
    assert.equal(metadata[index]?.name, name, `${name} must be anonymously visible on npm`);
    assert.equal(metadata[index]?.version, version, `${name} registry version must match the release`);
    assert(metadata[index]?.dist?.integrity?.startsWith('sha512-'), `${name} registry metadata must carry integrity`);
    assert.match(metadata[index]?.dist?.attestations?.url ?? '', /^https:\/\//, `${name} must carry npm provenance`);
  }

  let installed = false;
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      execFileSync('npm', [
        'install', '--save-exact', '--ignore-scripts', '--no-audit', '--no-fund',
        '--registry=https://registry.npmjs.org', reactSpec, mcpSpec, 'react@18.3.1',
      ], { cwd: consumer, stdio: attempt === attempts ? 'inherit' : 'pipe' });
      installed = true;
      break;
    } catch (error) {
      if (attempt === attempts) throw error;
      fs.rmSync(path.join(consumer, 'node_modules'), { recursive: true, force: true });
      fs.rmSync(path.join(consumer, 'package-lock.json'), { force: true });
      await new Promise((resolve) => setTimeout(resolve, 5_000));
    }
  }
  assert(installed, 'published package pair did not become installable');
  execFileSync('npm', ['ls', '--all'], { cwd: consumer, stdio: 'inherit' });
  execFileSync(process.execPath, ['--input-type=module', '--eval', `
    import reactPackage from '@husklet/react/package.json' with { type: 'json' };
    import mcpPackage from '@husklet/mcp/package.json' with { type: 'json' };
    import { Session, workspace } from '@husklet/react';
    import { createServer, tools } from '@husklet/mcp';
    if (reactPackage.version !== ${JSON.stringify(version)}) throw new Error('wrong React version ' + reactPackage.version);
    if (mcpPackage.version !== ${JSON.stringify(version)}) throw new Error('wrong MCP version ' + mcpPackage.version);
    if (mcpPackage.dependencies['@husklet/react'] !== ${JSON.stringify(version)}) throw new Error('MCP dependency is not exact');
    if ([Session, workspace, createServer, tools].some((value) => typeof value !== 'function')) throw new Error('public API import failed');
  `], { cwd: consumer, stdio: 'inherit' });

  const executable = path.join(consumer, 'node_modules', '.bin', 'husklet-mcp');
  fs.accessSync(executable, fs.constants.X_OK);
  const lock = JSON.parse(fs.readFileSync(path.join(consumer, 'package-lock.json')));
  for (const name of ['@husklet/react', '@husklet/mcp']) {
    const entry = lock.packages[`node_modules/${name}`];
    assert.equal(entry.version, version, `${name} lock entry must be the published version`);
    assert(entry.integrity?.startsWith('sha512-'), `${name} must install with registry integrity`);
    if (!localOverride) assert.match(entry.resolved, /^https:\/\/registry\.npmjs\.org\//, `${name} must resolve from npm`);
  }
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}
