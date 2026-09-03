import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';

export function determinePublication(local, published) {
  for (const name of ['client', 'react']) {
    if (published[name] !== null && published[name] !== local[name]) {
      throw new Error(`@husklet/${name} already exists with different tarball integrity`);
    }
  }
  if (published.react !== null && published.client === null) {
    throw new Error('@husklet/react exists while its exact @husklet/client dependency is absent');
  }
  return { client: published.client === null, react: published.react === null };
}

function packedIntegrity(name) {
  return JSON.parse(execFileSync('npm', [
    'pack', '--dry-run', '--json', '--ignore-scripts', '--workspace', `@husklet/${name}`,
  ], { encoding: 'utf8' }))[0].integrity;
}

function publishedIntegrity(name, version) {
  try {
    const value = execFileSync('npm', [
      'view', `@husklet/${name}@${version}`, 'dist.integrity', '--json',
    ], { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
    return JSON.parse(value);
  } catch (error) {
    const diagnostic = `${error.stdout ?? ''}\n${error.stderr ?? ''}`;
    if (/E404|404 Not Found/.test(diagnostic)) return null;
    throw error;
  }
}

if (process.argv[1] && new URL(import.meta.url).pathname === fs.realpathSync(process.argv[1])) {
  const version = process.env.RELEASE_VERSION;
  const output = process.env.GITHUB_OUTPUT;
  assert.match(version ?? '', /^\d+\.\d+\.\d+$/, 'RELEASE_VERSION must have exact X.Y.Z form');
  assert(output, 'GITHUB_OUTPUT is required');
  const local = { client: packedIntegrity('client'), react: packedIntegrity('react') };
  const published = {
    client: publishedIntegrity('client', version),
    react: publishedIntegrity('react', version),
  };
  const plan = determinePublication(local, published);
  fs.appendFileSync(output, `client=${plan.client}\nreact=${plan.react}\n`);
}
