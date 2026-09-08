import { ExecutionOperationError, connect, workspace } from '@husklet/client';

declare const process: { argv: string[]; stdout: { write(value: string): void } };

type Configuration = {
  path: string;
  containerId: string;
  generation: number;
  database: string;
  query: string;
  passwordCredential: string;
  timeoutMs?: number;
};

const configuration = JSON.parse(process.argv[2] ?? 'null') as Configuration | null;
if (
  !configuration?.path ||
  !configuration.containerId ||
  !Number.isSafeInteger(configuration.generation) ||
  !configuration.database ||
  !configuration.query ||
  !configuration.passwordCredential
) {
  throw new TypeError(
    'usage: postgres-browser.ts JSON(path, containerId, generation, database, query, passwordCredential, timeoutMs?)',
  );
}

const session = await connect({ path: configuration.path, pendingLimit: 8, timeout: 5_000 });
let executionId: string | undefined;
try {
  const containers = workspace(session).containers;
  const container = await containers.inspect(configuration.containerId);
  if (container.state !== 'running') throw new Error('Postgres container is not running');

  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort('query timed out'), configuration.timeoutMs ?? 30_000);
  const decoder = new TextDecoder('utf-8', { fatal: true });
  let pending = '';
  let rows = 0;
  const preview: unknown[] = [];
  try {
    const result = await containers.execStreaming(
      container.id,
      container.generation,
      {
        command: [
          'psql',
          '--no-psqlrc',
          '--quiet',
          '--tuples-only',
          '--no-align',
          '--dbname',
          configuration.database,
          '--command',
          `SELECT row_to_json(husklet_row)::text FROM (${configuration.query}) AS husklet_row`,
        ],
        credentials: [['PGPASSWORD', configuration.passwordCredential]],
        signal: abort.signal,
        cancelSignal: 'SIGINT',
        pageLimit: 32,
      },
      (page) => {
        for (const entry of page.entries) {
          if (entry.stream === 'stderr') continue;
          pending += decoder.decode(Uint8Array.from(entry.bytes), { stream: true });
          const lines = pending.split('\n');
          pending = lines.pop() ?? '';
          for (const line of lines) {
            if (!line) continue;
            rows += 1;
            if (preview.length < 25) preview.push(JSON.parse(line));
          }
        }
      },
    );
    executionId = result.executionId;
    pending += decoder.decode();
    if (pending) {
      rows += 1;
      if (preview.length < 25) preview.push(JSON.parse(pending));
    }
    if (result.execution.exit_code !== 0) {
      throw new Error(`psql exited with status ${result.execution.exit_code ?? 'unknown'}`);
    }
    process.stdout.write(`${JSON.stringify({ rows, preview })}\n`);
  } catch (error) {
    if (error instanceof ExecutionOperationError) executionId = error.executionId;
    throw error;
  } finally {
    clearTimeout(timer);
    if (executionId) await containers.removeExecution(executionId).catch(() => {});
  }
} finally {
  await session.close();
}
