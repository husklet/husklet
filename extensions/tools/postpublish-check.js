import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
const version = process.env.RELEASE_VERSION;
if (!version) throw new Error('RELEASE_VERSION is required');
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-public-'));
try {
  fs.writeFileSync(path.join(scratch, 'package.json'), JSON.stringify({ private: true, type: 'module' }));
  execFileSync('npm', ['install', '--ignore-scripts', '--no-audit', '--no-fund', `@husklet/client@${version}`, `@husklet/react@${version}`, 'react@18.3.1'], { cwd: scratch, stdio: 'inherit' });
  execFileSync(process.execPath, ['--input-type=module', '--eval', "import { Session } from '@husklet/client'; import { Session as ReactSession } from '@husklet/react'; if (Session !== ReactSession) process.exit(1)"], { cwd: scratch });
} finally { fs.rmSync(scratch, { recursive: true, force: true }); }
