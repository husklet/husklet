import { connect, workspace, type FileInventory } from '@husklet/client';
declare const process: { argv: string[]; stdout: { write(value: string): void } };

type Configuration = { path: string; document: string; index: string; chunkBytes?: number };
const configuration = JSON.parse(process.argv[2] ?? 'null') as Configuration | null;
if (!configuration?.path || !configuration.document || !configuration.index) {
  throw new TypeError('usage: embeddings-indexer.ts JSON(path, document, index, chunkBytes?)');
}
const chunkBytes = Math.max(1, Math.min(configuration.chunkBytes ?? 64 * 1024, 512 * 1024));
const session = await connect({ path: configuration.path, pendingLimit: 8, timeout: 5_000 });
try {
  const files = workspace(session).files;
  let acceptInventory!: (inventory: FileInventory) => void;
  const initial = new Promise<FileInventory>((resolve) => {
    acceptInventory = resolve;
  });
  const dispose = await workspace(session).watchFilesystem(acceptInventory);
  const inventory = await initial;
  await dispose();
  if (
    !inventory.complete ||
    !inventory.entries.some(({ path }) => path === configuration.document)
  ) {
    throw new Error('filesystem inventory is incomplete or document is absent');
  }
  let offset = 0;
  let observed: string | null = null;
  const bytes: number[] = [];
  for (;;) {
    const range = await files.readRange(configuration.document, offset, chunkBytes, observed);
    observed ??= range.identity;
    if (range.identity !== observed || range.offset !== offset)
      throw new Error('document changed during indexing');
    bytes.push(...range.contents);
    offset += range.contents.length;
    if (range.eof) break;
  }
  const index = await files.stat(configuration.index);
  if (!index.identity) throw new Error('index has no compare-and-swap identity');
  const digest = Array.from(
    new Uint8Array(await crypto.subtle.digest('SHA-256', new Uint8Array(bytes))),
  )
    .map((byte) => byte.toString(16).padStart(2, '0'))
    .join('');
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
  process.stdout.write(
    `${JSON.stringify({ bytes: bytes.length, documentIdentity: observed, indexIdentity: nextIdentity, digest })}\n`,
  );
} finally {
  await session.close();
}
