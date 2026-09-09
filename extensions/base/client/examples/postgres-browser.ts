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

  const query = configuration.query.trim().replace(/;$/, '');
  if (!query) throw new TypeError('query must contain a row-producing statement');
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort('query timed out'), configuration.timeoutMs ?? 30_000);
  let rows = 0;
  const preview: unknown[] = [];
  try {
    const result = await containers.execJsonLines(
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
          `SELECT row_to_json(husklet_row)::text FROM (${query}) AS husklet_row`,
        ],
        credentials: [['PGPASSWORD', configuration.passwordCredential]],
        signal: abort.signal,
        cancelSignal: 'SIGINT',
        pageLimit: 32,
        maxLineBytes: 1024 * 1024,
      },
      (value) => {
        rows += 1;
        if (preview.length < 25) preview.push(value);
      },
    );
    executionId = result.executionId;
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
