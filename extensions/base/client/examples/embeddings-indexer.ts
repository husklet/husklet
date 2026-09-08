import { connect, workspace } from '@husklet/client';
declare const process: { argv: string[]; stdout: { write(value: string): void } };

type Configuration = {
  path: string;
  root: string;
  document: string;
  index: string;
  chunkBytes?: number;
};
const configuration = JSON.parse(process.argv[2] ?? 'null') as Configuration | null;
if (
  !configuration?.path ||
  !configuration.root ||
  !configuration.document ||
  !configuration.index
) {
  throw new TypeError(
    'usage: embeddings-indexer.ts JSON(path, root, document, index, chunkBytes?)',
  );
}
const chunkBytes = Math.max(1, Math.min(configuration.chunkBytes ?? 64 * 1024, 512 * 1024));
const session = await connect({ path: configuration.path, pendingLimit: 8, timeout: 5_000 });
try {
  const host = workspace(session);
  const files = host.files;
  const inventory = await files.inventory();
  if (!inventory.complete) throw new Error('filesystem change retention gap requires a rescan');
  let discovered = false;
  for await (const entry of files.walk(configuration.root)) {
    if (entry.path === configuration.document && !entry.directory) discovered = true;
  }
  if (!discovered) throw new Error('document is absent from the configured subtree');
  let observed: string | null = null;
  const bytes: number[] = [];
  for await (const range of files.readChunks(configuration.document, { chunkBytes })) {
    observed ??= range.identity;
    bytes.push(...range.contents);
  }
  const index = await files.stat(configuration.index);
  if (!index.identity) throw new Error('index has no compare-and-swap identity');
  const digest = Array.from(
    new Uint8Array(await crypto.subtle.digest('SHA-256', new Uint8Array(bytes))),
  )
    .map((byte) => byte.toString(16).padStart(2, '0'))
    .join('');
  const changes = await files.changes(inventory.revision);
  if (changes.truncated) throw new Error('filesystem change retention gap requires a rescan');
  if (changes.changes.some((change) => change.path === configuration.document))
    throw new Error('document changed after inventory; refusing a stale index publication');
  const confirmed = await files.stat(configuration.document);
  if (confirmed.identity !== observed)
    throw new Error('document identity changed before index publication');
  const nextIdentity = await files.writeObserved(
    configuration.index,
    index.identity,
    new TextEncoder().encode(
      JSON.stringify({
        document: configuration.document,
        identity: observed,
        bytes: bytes.length,
        digest,
      }),
    ),
  );
  type Checkpoint = {
    version: 1;
    documents: Record<string, { identity: string | null; digest: string }>;
  };
  const checkpoint = {
    decode(value: unknown): Checkpoint {
      if (value === undefined) return { version: 1, documents: {} };
      if (
        typeof value === 'object' &&
        value !== null &&
        (value as { version?: unknown }).version === 1 &&
        typeof (value as { documents?: unknown }).documents === 'object'
      )
        return value as Checkpoint;
      throw new TypeError('unsupported embeddings checkpoint schema');
    },
    encode(value: Checkpoint) {
      return value;
    },
  };
  const state = await host.state.updateJson(checkpoint, (current) => ({
    ...current,
    documents: {
      ...current.documents,
      [configuration.document]: { identity: observed, digest },
    },
  }));
  process.stdout.write(
    `${JSON.stringify({ bytes: bytes.length, documentIdentity: observed, indexIdentity: nextIdentity, stateIdentity: state.identity, digest })}\n`,
  );
} finally {
  await session.close();
}
