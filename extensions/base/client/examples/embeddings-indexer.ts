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
  maxDocumentBytes?: number;
  once?: boolean;
  model?: { container: string; generation: number; command: string[] };
};
type DocumentState = { identity: string; digest: string; bytes: number };
type Checkpoint = {
  version: 1;
  journal: string | null;
  revision: number;
  documents: Record<string, DocumentState>;
};

const configuration = JSON.parse(process.argv[2] ?? 'null') as Configuration | null;
if (!configuration?.path || !configuration.roots?.length) {
  throw new TypeError('usage: embeddings-indexer.ts JSON(path, roots, suffixes?, model?, once?)');
}
const suffixes = configuration.suffixes ?? ['.md', '.txt', '.ts'];
const chunkBytes = Math.max(1, Math.min(configuration.chunkBytes ?? 64 * 1024, 64 * 1024));
const maxDocumentBytes = configuration.maxDocumentBytes ?? 16 * 1024 * 1024;
const checkpointCodec = {
  decode(value: unknown): Checkpoint {
    if (value === undefined) return { version: 1, journal: null, revision: 0, documents: {} };
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
  const roots = configuration.roots.map((path) => {
    const grant = host.files.pathGrant('read', path);
    if (grant === null) {
      throw new Error(
        `configured root ${JSON.stringify(path)} is outside the filesystem:read grant`,
      );
    }
    return { path, grant };
  });
  let checkpoint = (await host.state.readJson(checkpointCodec)).value;
  const wanted = (entry: FileEntry) =>
    !entry.directory && suffixes.some((suffix) => entry.path.endsWith(suffix));

  const index = async (
    entry: FileEntry,
    persist = true,
    signal: AbortSignal = controller.signal,
  ) => {
    const exact = entry.identity ? entry : await host.files.stat(entry.path);
    if (!exact.identity || checkpoint.documents[entry.path]?.identity === exact.identity) return;
    const document = await host.files.readText(entry.path, {
      maxBytes: maxDocumentBytes,
      chunkBytes,
      observed: exact.identity,
      signal,
    });
    let digest: string;
    if (configuration.model) {
      let executionId: string | undefined;
      try {
        async function* documentChunks() {
          // 16k UTF-16 code units can never exceed the protocol's 64 KiB UTF-8 chunk limit.
          for (let offset = 0; offset < document.text.length; offset += 16 * 1024) {
            yield document.text.slice(offset, offset + 16 * 1024);
          }
        }
        const result = await host.containers.execText(
          configuration.model.container,
          configuration.model.generation,
          {
            command: [...configuration.model.command, entry.path],
            input: documentChunks(),
            maxBytes: 1024 * 1024,
            signal,
            onStarted(id) {
              executionId = id;
            },
          },
        );
        if (result.execution.exit_code !== 0) {
          throw new Error(
            `embedding model exited with status ${result.execution.exit_code ?? 'unknown'}`,
          );
        }
        digest = result.stdout.trim();
      } finally {
        if (executionId) await host.containers.removeExecution(executionId).catch(() => {});
      }
    } else {
      digest = Array.from(
        new Uint8Array(
          await crypto.subtle.digest('SHA-256', new TextEncoder().encode(document.text)),
        ),
      )
        .map((byte) => byte.toString(16).padStart(2, '0'))
        .join('');
    }
    const indexed = { identity: document.identity, digest, bytes: document.bytes };
    if (persist)
      checkpoint = (
        await host.state.updateJson(checkpointCodec, (current) => ({
          ...current,
          documents: { ...current.documents, [entry.path]: indexed },
        }))
      ).value;
    process.stdout.write(
      `${JSON.stringify({ path: entry.path, identity: exact.identity, digest })}\n`,
    );
    return indexed;
  };

  const inventory = await host.files.inventory();
  if (!inventory.complete) throw new Error('filesystem inventory is incomplete');
  const scanned: Record<string, DocumentState> = {};
  for (const root of roots) {
    if (root.grant === 'exact') {
      const entry = await host.files.stat(root.path);
      if (wanted(entry)) {
        const document = await index(entry, false);
        if (document) scanned[entry.path] = document;
      }
    } else {
      for await (const entry of host.files.walk(root.path, { signal: controller.signal })) {
        if (wanted(entry)) {
          const document = await index(entry, false);
          if (document) scanned[entry.path] = document;
        }
      }
    }
  }
  const caughtUp = await host.files.catchUpChanges({
    cursor: { journal: inventory.journal, revision: inventory.revision },
    maxChanges: 4_096,
    maxPages: 64,
    signal: controller.signal,
  });
  if (!caughtUp.caughtUp)
    throw new Error('filesystem catch-up exceeded its bound; resume before publishing');
  if (caughtUp.changes.length > 0) {
    throw new Error('document changed after inventory; refusing a stale checkpoint');
  }
  checkpoint = (
    await host.state.updateJson(checkpointCodec, (current) => ({
      ...current,
      revision: caughtUp.cursor.revision,
      journal: caughtUp.cursor.journal,
      documents: { ...current.documents, ...scanned },
    }))
  ).value;

  if (!configuration.once) {
    const stop = await host.files.watchLatestChanges(
      async (page, signal) => {
        if (page.truncated) throw new Error('filesystem journal gap requires a full rescan');
        for (const change of page.changes) {
          if (change.entry && wanted(change.entry)) await index(change.entry, true, signal);
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
            journal: page.journal,
            revision: page.next,
          }))
        ).value;
      },
      {
        cursor: {
          journal: checkpoint.journal ?? inventory.journal,
          revision: checkpoint.revision,
        },
        signal: controller.signal,
      },
    );
    const interrupted = new Promise<void>((resolve) => {
      if (controller.signal.aborted) resolve();
      else controller.signal.addEventListener('abort', () => resolve(), { once: true });
    });
    await Promise.race([stop.done, interrupted]);
    await stop();
  }
} finally {
  await session.close();
}
