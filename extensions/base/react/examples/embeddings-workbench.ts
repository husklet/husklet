import React from 'react';
import {
  Badge,
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
  root: string;
  model: { container: string; generation: number; credential: string };
};
type Checkpoint = {
  version: 1;
  revision: number;
  documents: Record<string, { identity: string; embedding: string }>;
};

const configuration = JSON.parse(process.argv[2] ?? 'null') as Configuration;
const codec = {
  decode(value: unknown): Checkpoint {
    if (value === undefined) return { version: 1, revision: 0, documents: {} };
    if (!value || typeof value !== 'object' || (value as { version?: unknown }).version !== 1)
      throw new TypeError('unsupported embeddings checkpoint');
    return value as Checkpoint;
  },
  encode(value: Checkpoint) {
    return value;
  },
};

const session = await connect({ path: configuration.path });
try {
  const host = workspace(session);
  const resumed = await host.state.readJson(codec);
  const inventory = await host.files.inventory();
  if (!inventory.complete) throw new Error('workspace inventory is incomplete');
  const journal = await host.files.changes(resumed.value.revision, 128);
  if (journal.truncated) throw new Error('workspace journal requires a full rescan');

  const documents = { ...resumed.value.documents };
  let indexed = 0;
  for await (const entry of host.files.walk(configuration.root, { pageSize: 64 })) {
    if (entry.directory || !entry.path.endsWith('.ts') || !entry.identity) continue;
    if (documents[entry.path]?.identity === entry.identity) continue;
    const chunks = [];
    for await (const range of host.files.readChunks(entry.path, {
      observed: entry.identity,
      chunkBytes: 8,
    }))
      chunks.push(...range.contents);
    const result = await host.containers.execText(
      configuration.model.container,
      configuration.model.generation,
      {
        command: ['embed', entry.path],
        credentials: [['EMBEDDINGS_API_KEY', configuration.model.credential]],
        input: [chunks],
        maxBytes: 4096,
        pageLimit: 1,
      },
    );
    if (result.execution.exit_code !== 0) throw new Error(result.stderr || 'embedding failed');
    documents[entry.path] = { identity: entry.identity, embedding: result.stdout.trim() };
    indexed += 1;
  }

  const checkpoint = await host.state.writeJson(
    resumed.identity,
    { version: 1, revision: journal.next, documents },
    codec,
  );
  const surface = render(
    React.createElement(
      Column,
      { gap: 2, pad: 3, grow: true },
      React.createElement(Heading, { label: 'Code index', scale: 'title' }),
      React.createElement(
        Row,
        { gap: 1, align: 'center' },
        React.createElement(Badge, { label: 'Up to date', tone: 'positive' }),
        React.createElement(Text, { label: `${Object.keys(documents).length} documents` }),
      ),
      React.createElement(Progress, { fraction: 1 }),
      React.createElement(Text, { label: `${indexed} changed document indexed after restart` }),
    ),
    session,
    { title: 'Embeddings' },
  );
  await surface.ready;
  await surface.flush();
  process.stdout.write(JSON.stringify({ indexed, revision: journal.next, checkpoint }));
} finally {
  await session.close();
}
