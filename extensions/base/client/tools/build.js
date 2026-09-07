import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const source = path.join(root, 'src');
const output = path.join(root, 'dist');
fs.mkdirSync(output, { recursive: true });
for (const name of ['generated-protocol.js', 'generated-protocol.d.ts']) {
  fs.copyFileSync(path.join(source, name), path.join(output, name));
}
