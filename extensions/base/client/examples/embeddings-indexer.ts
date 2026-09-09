import { connect, workspace, type FileEntry } from '@husklet/client';

declare const process: {
  argv: string[];
  stdout: { write(value: string): void };
  once(name: 'SIGINT' | 'SIGTERM', listener: () => void): void;
};

type Configuration = {
  path: string;
  roots: string[];
  suffixes?: string[];
  chunkBytes?: number;
  once?: boolean;
  model?: { container: string; generation: number; command: string[] };
};
type DocumentState = { identity: string; digest: string; bytes: number };
type Checkpoint = { version: 1; revision: number; documents: Record<string, DocumentState> };

const configuration = JSON.parse(process.argv[2] ?? 'null') as Configuration | null;
if (!configuration?.path || !configuration.roots?.length) {
  throw new TypeError('usage: embeddings-indexer.ts JSON(path, roots, suffixes?, model?, once?)');
}
const suffixes = configuration.suffixes ?? ['.md', '.txt', '.ts'];
const chunkBytes = Math.max(1, Math.min(configuration.chunkBytes ?? 64 * 1024, 512 * 1024));
const checkpointCodec = {
  decode(value: unknown): Checkpoint {
    if (value === undefined) return { version: 1, revision: 0, documents: {} };
    if (
      typeof value !== 'object' ||
      value === null ||
      (value as { version?: unknown }).version !== 1
    ) {
      throw new TypeError('unsupported embeddings checkpoint schema');
    }
    return value as Checkpoint;
  },
  encode(value: Checkpoint) {
    return value;
  },
};

const controller = new AbortController();
process.once('SIGINT', () => controller.abort('SIGINT'));
process.once('SIGTERM', () => controller.abort('SIGTERM'));
const session = await connect({ path: configuration.path, pendingLimit: 8, timeout: 30_000 });
try {
  const host = workspace(session);
  let checkpoint = (await host.state.readJson(checkpointCodec)).value;
  const wanted = (entry: FileEntry) =>
    !entry.directory && suffixes.some((suffix) => entry.path.endsWith(suffix));

  const index = async (entry: FileEntry, persist = true) => {
    const exact = entry.identity ? entry : await host.files.stat(entry.path);
    if (!exact.identity || checkpoint.documents[entry.path]?.identity === exact.identity) return;
    const bytes: number[] = [];
    for await (const range of host.files.readChunks(entry.path, {
      chunkBytes,
      observed: exact.identity,
      signal: controller.signal,
    })) {
      bytes.push(...range.contents); // each next() applies consumer backpressure
    }
    let digest: string;
    if (configuration.model) {
      const result = await host.containers.execText(
        configuration.model.container,
        configuration.model.generation,
        {
          command: [...configuration.model.command, entry.path],
          maxBytes: 1024 * 1024,
          signal: controller.signal,
        },
      );
      digest = result.stdout.trim(); // replace with the model's vector/index identifier
    } else {
      digest = Array.from(
        new Uint8Array(await crypto.subtle.digest('SHA-256', new Uint8Array(bytes))),
      )
        .map((byte) => byte.toString(16).padStart(2, '0'))
        .join('');
    }
    const document = { identity: exact.identity, digest, bytes: bytes.length };
    if (persist)
      checkpoint = (
        await host.state.updateJson(checkpointCodec, (current) => ({
          ...current,
          documents: { ...current.documents, [entry.path]: document },
        }))
      ).value;
    process.stdout.write(
      `${JSON.stringify({ path: entry.path, identity: exact.identity, digest })}\n`,
    );
    return document;
  };

  const inventory = await host.files.inventory();
  if (!inventory.complete) throw new Error('filesystem inventory is incomplete');
  const scanned: Record<string, DocumentState> = {};
  for (const root of configuration.roots) {
    for await (const entry of host.files.walk(root, { signal: controller.signal })) {
      if (wanted(entry)) {
        const document = await index(entry, false);
        if (document) scanned[entry.path] = document;
      }
    }
  }
  const caughtUp = await host.files.changes(inventory.revision);
  if (caughtUp.truncated) throw new Error('filesystem journal gap requires a full rescan');
  if (caughtUp.changes.length > 0) {
    throw new Error('document changed after inventory; refusing a stale checkpoint');
  }
  checkpoint = (
    await host.state.updateJson(checkpointCodec, (current) => ({
      ...current,
      revision: caughtUp.next,
      documents: { ...current.documents, ...scanned },
    }))
  ).value;

  if (!configuration.once) {
    const stop = await host.files.watchChanges(
      async (page) => {
        if (page.truncated) throw new Error('filesystem journal gap requires a full rescan');
        for (const change of page.changes) {
          if (change.entry && wanted(change.entry)) await index(change.entry);
          if (!change.entry && checkpoint.documents[change.path]) {
            const { [change.path]: _removed, ...documents } = checkpoint.documents;
            void _removed;
            checkpoint = (
              await host.state.updateJson(checkpointCodec, (current) => ({
                ...current,
                revision: page.next,
                documents,
              }))
            ).value;
          }
        }
        checkpoint = (
          await host.state.updateJson(checkpointCodec, (current) => ({
            ...current,
            revision: page.next,
          }))
        ).value;
      },
      { after: checkpoint.revision, signal: controller.signal },
    );
    await new Promise<void>((resolve) => {
      if (controller.signal.aborted) resolve();
      else controller.signal.addEventListener('abort', () => resolve(), { once: true });
    });
    await stop();
  }
} finally {
  await session.close();
}
