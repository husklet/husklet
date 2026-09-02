import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-mcp-pack-'));
try {
  const packed = JSON.parse(execFileSync('npm', ['pack', '--json', '--ignore-scripts', '--pack-destination', scratch], { cwd: root, encoding: 'utf8' }));
  const tarball = path.join(scratch, packed[0].filename);
  const reactRoot = path.resolve(root, '../react');
  const reactPacked = JSON.parse(execFileSync('npm', ['pack', '--json', '--ignore-scripts', '--pack-destination', scratch], { cwd: reactRoot, encoding: 'utf8' }));
  const reactTarball = path.join(scratch, reactPacked[0].filename);
  const consumer = path.join(scratch, 'consumer');
  fs.mkdirSync(consumer);
  fs.writeFileSync(path.join(consumer, 'package.json'), JSON.stringify({ private: true, type: 'module' }));
  execFileSync('npm', ['install', '--ignore-scripts', '--no-audit', '--no-fund', reactTarball, tarball], { cwd: consumer, stdio: 'pipe' });
  execFileSync(process.execPath, ['--input-type=module', '--eval', `
    import { tools, createServer, semanticXml } from '@husklet/mcp';
    if (typeof tools !== 'function' || typeof createServer !== 'function') process.exit(1);
    const xml = semanticXml({ slot: 'packed', revision: 1, truncated: false, root: { id: 0, role: 'column', label: null, value: null, disabled: false, actions: [], children: [] } });
    if (!xml.startsWith('<pane slot="packed"')) process.exit(1);
  `], { cwd: consumer, stdio: 'pipe' });
  fs.writeFileSync(path.join(consumer, 'consumer.ts'), `
    import { createServer, semanticXml, tools, type ToolDefinition } from '@husklet/mcp';
    import type { Session } from '@husklet/react';
    declare const session: Session;
    const server = createServer(session);
    const definitions: ToolDefinition[] = tools({} as Parameters<typeof tools>[0]);
    const xml: string = semanticXml({ slot: 'typed', revision: 1, truncated: false, root: { id: 0, role: 'column', label: null, value: null, disabled: false, actions: [], children: [] } });
    void server; void definitions; void xml;
    // @ts-expect-error a Session is required
    createServer({});
  `);
  execFileSync(path.resolve(root, '../node_modules/.bin/tsc'), [
    '--noEmit', '--strict', '--skipLibCheck', '--target', 'ES2022',
    '--module', 'NodeNext', '--moduleResolution', 'NodeNext', 'consumer.ts',
  ], { cwd: consumer, stdio: 'pipe' });
  const names = new Set(packed[0].files.map(({ path: name }) => name));
  assert(names.has('src/cli.js'));
  assert(![...names].some((name) => name.startsWith('test/') || name.startsWith('tools/')));
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}
