import { ExecutionOperationError, connect, workspace } from '@husklet/client';

declare const process: { argv: string[]; stdout: { write(value: string): void } };

type Configuration = {
  path: string;
  containerId: string;
  generation: number;
  networkId: string;
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
  !configuration.networkId ||
  !configuration.database ||
  !configuration.query ||
  !configuration.passwordCredential
) {
  throw new TypeError(
    'usage: postgres-browser.ts JSON(path, containerId, generation, networkId, database, query, passwordCredential, timeoutMs?)',
  );
}

const session = await connect({ path: configuration.path, pendingLimit: 8, timeout: 5_000 });
let executionId: string | undefined;
try {
  const containers = workspace(session).containers;
  // A saved database target is an exact lifecycle identity. Never follow a reused
  // container ID/name onto a replacement generation without fresh user selection.
  const container = await containers.inspectObserved(
    configuration.containerId,
    configuration.generation,
  );
  if (container.state !== 'running') throw new Error('Postgres container is not running');

  const query = configuration.query.trim().replace(/;$/, '');
  if (!query) throw new TypeError('query must contain a row-producing statement');
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort('query timed out'), configuration.timeoutMs ?? 30_000);
  let rows = 0;
  const preview: unknown[] = [];
  try {
    const result = await workspace(session).networks.withTemporaryConnection(
      configuration.networkId,
      container.id,
      () => containers.execJsonLines(container.id, container.generation, {
        command: [
          'psql',
          '--no-psqlrc',
          '--quiet',
          '--tuples-only',
          '--no-align',
          '--dbname',
          configuration.database,
          '--file',
          '-',
        ],
        credentials: [['PGPASSWORD', configuration.passwordCredential]],
        input: [`SELECT row_to_json(husklet_row)::text FROM (${query}) AS husklet_row;\n`],
        maxLineBytes: 1024 * 1024,
        pageLimit: 16,
        signal: abort.signal,
        onStarted: (id) => {
          executionId = id;
        },
      }, (value) => {
        rows += 1;
        if (preview.length < 25) preview.push(value);
      }),
      { aliases: ['postgres-inspector'] },
    );
    const execution = result.execution;
    if (execution.exit_code !== 0) {
      const output = await containers.executionLogs(result.executionId, {
        stdout: false,
        stderr: true,
      });
      throw new Error(
        `psql exited with status ${execution.exit_code ?? 'unknown'}: ${new TextDecoder().decode(Uint8Array.from(output.stderr))}`,
      );
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
