import React from 'react';
import {
  Badge,
  Code,
  Column,
  Heading,
  Progress,
  Row,
  Text,
  connect,
  render,
  workspace,
} from '@husklet/react';

declare const process: { argv: string[]; stdout: { write(value: string): void } };
type Configuration = {
  path: string;
  repository: string;
  container: string;
  generation: number;
  file: string;
  terminal: string;
};
type Checkpoint = { version: 1; head: string; reviewed: Record<string, string> };
const configuration = JSON.parse(process.argv[2] ?? 'null') as Configuration;
const codec = {
  decode(value: unknown): Checkpoint {
    if (value === undefined) return { version: 1, head: '', reviewed: {} };
    if (!value || typeof value !== 'object' || (value as { version?: unknown }).version !== 1)
      throw new TypeError('unsupported review checkpoint');
    return value as Checkpoint;
  },
  encode(value: Checkpoint) {
    return value;
  },
};

const session = await connect({ path: configuration.path });
try {
  const host = workspace(session);
  const checkpoint = await host.state.readJson(codec);
  const runGit = (arguments_: string[], maxBytes: number) =>
    host.containers.execText(configuration.container, configuration.generation, {
      command: ['git', '-C', configuration.repository, ...arguments_],
      maxBytes,
      pageLimit: 2,
    });
  const [status, diff] = await Promise.all([
    runGit(['status', '--porcelain=v2', '--branch', '-z'], 64 * 1024),
    runGit(['diff', '--no-ext-diff', '--unified=3', '--', configuration.file], 256 * 1024),
  ]);
  if (status.execution.exit_code !== 0 || diff.execution.exit_code !== 0)
    throw new Error(status.stderr || diff.stderr || 'git inspection failed');
  const entry = await host.files.stat(configuration.file);
  if (!entry.identity) throw new Error('review target has no stable identity');
  const source = await host.files.readText(configuration.file, {
    observed: entry.identity,
    maxBytes: 1024 * 1024,
    chunkBytes: 16,
  });
  const terminal = await host.terminal.toText(configuration.terminal, { lines: 20 });
  const replacement = source.text.replace('return false;', 'return true;');
  if (replacement === source.text) throw new Error('reviewed edit no longer applies');
  const written = await host.files.writeObserved(
    configuration.file,
    source.identity,
    new TextEncoder().encode(replacement),
  );
  const head = status.stdout.match(/# branch\.oid ([0-9a-f]{40})/)?.[1] ?? '';
  const persisted = await host.state.writeJson(
    checkpoint.identity,
    { version: 1, head, reviewed: { ...checkpoint.value.reviewed, [configuration.file]: written } },
    codec,
  );
  const surface = render(
    React.createElement(
      Column,
      { gap: 2, pad: 3, grow: true },
      React.createElement(Heading, { label: 'Pull request review', scale: 'title' }),
      React.createElement(
        Row,
        { gap: 1, align: 'center' },
        React.createElement(Badge, { label: 'Applied safely', tone: 'positive' }),
        React.createElement(Text, { label: configuration.file }),
      ),
      React.createElement(Progress, { fraction: 1 }),
      React.createElement(Code, { label: diff.stdout.slice(0, 4096) }),
      React.createElement(Text, { label: `Terminal context: ${terminal.text.slice(0, 256)}` }),
    ),
    session,
    { title: 'Review' },
  );
  await surface.ready;
  await surface.flush();
  process.stdout.write(
    JSON.stringify({ head, written, persisted, resumed: checkpoint.value.head }),
  );
} finally {
  await session.close();
}
