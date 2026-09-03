import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-postpublish-test-'));
try {
  const pack = (workspace) => {
    const result = JSON.parse(execFileSync('npm', [
      'pack', '--json', '--ignore-scripts', '--pack-destination', scratch,
    ], { cwd: path.join(root, workspace), encoding: 'utf8' }))[0];
    return path.join(scratch, result.filename);
  };
  execFileSync(process.execPath, [path.join(root, 'tools', 'postpublish-check.js')], {
    cwd: root,
    env: {
      ...process.env,
      RELEASE_VERSION: '0.1.0',
      HUSKLET_POSTPUBLISH_REACT_SPEC: pack('react'),
      HUSKLET_POSTPUBLISH_MCP_SPEC: pack('mcp'),
      HUSKLET_POSTPUBLISH_METADATA: JSON.stringify(['react', 'mcp'].map((name) => ({
        name: `@husklet/${name}`,
        version: '0.1.0',
        dist: { integrity: 'sha512-fixture', attestations: { url: 'https://registry.npmjs.org/attestation' } },
      }))),
    },
    stdio: 'inherit',
  });

  assert.throws(() => execFileSync(process.execPath, [path.join(root, 'tools', 'postpublish-check.js')], {
    cwd: root,
    env: { ...process.env, RELEASE_VERSION: 'latest' },
    stdio: 'pipe',
  }), /Command failed/, 'a floating release version must be refused');
  assert.throws(() => execFileSync(process.execPath, [path.join(root, 'tools', 'postpublish-check.js')], {
    cwd: root,
    env: {
      ...process.env,
      RELEASE_VERSION: '0.1.0',
      HUSKLET_POSTPUBLISH_REACT_SPEC: 'unused-react',
      HUSKLET_POSTPUBLISH_MCP_SPEC: 'unused-mcp',
      HUSKLET_POSTPUBLISH_METADATA: JSON.stringify(['react', 'mcp'].map((name) => ({
        name: `@husklet/${name}`, version: '0.1.0', dist: { integrity: 'sha512-fixture' },
      }))),
    },
    stdio: 'pipe',
  }), /Command failed/, 'registry metadata without provenance must be refused');
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}
