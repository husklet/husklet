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
    executionId = await containers.execWithCredentials(container.id, container.generation, {
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
      stdin: true,
    });
    await containers.pipeExecutionStdin(
      executionId,
      [`SELECT row_to_json(husklet_row)::text FROM (${query}) AS husklet_row;\n`],
      { signal: abort.signal },
    );
    const decoder = new TextDecoder('utf-8', { fatal: true });
    let pending = '';
    for await (const page of containers.executionOutputPages(executionId, {
      limit: 16,
      signal: abort.signal,
    })) {
      for (const entry of page.entries) {
        if (entry.stream === 'stderr') continue; // surface stderr from executionLogs on failure below
        pending += decoder.decode(Uint8Array.from(entry.bytes), { stream: true });
        if (new TextEncoder().encode(pending).byteLength > 1024 * 1024) {
          throw new RangeError('Postgres returned a row larger than 1 MiB');
        }
        for (;;) {
          const newline = pending.indexOf('\n');
          if (newline < 0) break;
          const line = pending.slice(0, newline).replace(/\r$/, '');
          pending = pending.slice(newline + 1);
          if (line) {
            rows += 1;
            if (preview.length < 25) preview.push(JSON.parse(line));
          }
        }
      }
    }
    pending += decoder.decode();
    if (pending.trim()) {
      rows += 1;
      if (preview.length < 25) preview.push(JSON.parse(pending));
    }
    const execution = await containers.execution(executionId);
    if (execution.exit_code !== 0) {
      const output = await containers.executionLogs(executionId, { stdout: false, stderr: true });
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
