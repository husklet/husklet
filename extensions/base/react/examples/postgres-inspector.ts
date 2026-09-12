import React from 'react';
import {
  Badge,
  Card,
  CardContent,
  Column,
  DataTable,
  ExecutionOperationError,
  Heading,
  Row,
  Text,
  connect,
  createWindowCache,
  render,
  workspace,
  type ColumnSpec,
  type DataRow,
  type RowRequest,
} from '@husklet/react';

declare const process: { argv: string[]; stdout: { write(value: string): void } };
type Configuration = {
  path: string;
  container?: string;
  credentialKey: string;
  query: string;
  outputMaxBytes?: number;
};
const configuration = JSON.parse(process.argv[2] ?? 'null') as Configuration | null;
if (!configuration?.path || !configuration.credentialKey || !configuration.query) {
  throw new TypeError('usage: postgres-inspector.ts JSON(path, credentialKey, query, container?)');
}

function csv(input: string): string[][] {
  const records: string[][] = [];
  let record: string[] = [];
  let field = '';
  let quoted = false;
  for (let index = 0; index < input.length; index += 1) {
    const character = input[index];
    if (quoted && character === '"' && input[index + 1] === '"') {
      field += '"';
      index += 1;
    } else if (character === '"') quoted = !quoted;
    else if (character === ',' && !quoted) {
      record.push(field);
      field = '';
    } else if (character === '\n' && !quoted) {
      record.push(field);
      records.push(record);
      record = [];
      field = '';
    } else if (character !== '\r' || quoted) field += character;
  }
  if (quoted) throw new Error('psql returned an unterminated CSV field');
  if (field || record.length) records.push([...record, field]);
  return records;
}

function identifier(value: string): string {
  return `"${value.replaceAll('"', '""')}"`;
}

const session = await connect({ path: configuration.path, pendingLimit: 16, timeout: 30_000 });
try {
  const host = workspace(session);
  const inventory = await host.containers.list();
  const running = inventory.filter((container) => container.state === 'running');
  const selected = configuration.container
    ? inventory.find(
        (container) =>
          container.id === configuration.container || container.name === configuration.container,
      )
    : running.length === 1
      ? running[0]
      : undefined;
  if (!selected)
    throw new Error('select a unique running PostgreSQL container by immutable ID or name');
  const [container, processes, networks] = await Promise.all([
    host.containers.inspect(selected.id),
    host.containers.processes(selected.id),
    host.networks.list(),
  ]);

  const executeCsv = async (statement: string, signal?: AbortSignal): Promise<string[][]> => {
    let executionId: string | undefined;
    try {
      const result = await host.containers.execText(container.id, container.generation, {
        // Exact argv: query whitespace and metacharacters never become shell syntax.
        command: ['psql', '--csv', '--no-psqlrc', '--command', statement],
        credentials: [['PGPASSWORD', configuration.credentialKey]],
        signal,
        // A viewport query remains bounded even when one PostgreSQL value is unexpectedly huge.
        maxBytes: configuration.outputMaxBytes ?? 4 * 1024 * 1024,
      });
      executionId = result.executionId;
      const finished = result.execution;
      if (finished.exit_code !== 0)
        throw new Error(result.stderr.trim() || `psql exited ${finished.exit_code}`);
      return csv(result.stdout);
    } catch (error) {
      if (error instanceof ExecutionOperationError) executionId = error.executionId;
      throw error;
    } finally {
      if (executionId) await host.containers.removeExecution(executionId).catch(() => {});
    }
  };

  const query = configuration.query.trim().replace(/;+$/, '');
  const countRows = await executeCsv(`SELECT count(*) AS rows FROM (${query}) AS husklet_count`);
  const total = Number(countRows[1]?.[0]);
  if (!Number.isSafeInteger(total) || total < 0)
    throw new Error('query did not return a safe row count');
  const firstPage = await executeCsv(`SELECT * FROM (${query}) AS husklet_page LIMIT 128 OFFSET 0`);
  const headings = firstPage.shift() ?? ['result'];
  const schema: readonly ColumnSpec[] = headings.map((title, index) => ({
    key: String(index),
    title,
    width: index === 0 ? { chars: 12 } : 'fill',
    sortable: true,
  }));
  const cache = createWindowCache<string[][]>(32);
  cache.set('0:initial', firstPage);
  const source = 1;
  const version = 1;
  const provideRows = async (
    request: RowRequest,
    { signal }: { signal: AbortSignal },
  ): Promise<readonly DataRow[]> => {
    if (request.source !== source || request.version !== version) return [];
    const cacheKey = `${request.range.start}:${JSON.stringify(request.sort)}:${request.filter ?? ''}`;
    let records =
      request.range.start === 0 && !request.sort && !request.filter
        ? cache.get('0:initial')
        : cache.get(cacheKey);
    if (!records) {
      const key = request.sort && schema.find((column) => column.key === request.sort?.column);
      const order = key
        ? ` ORDER BY ${identifier(key.title)} ${request.sort?.descending ? 'DESC' : 'ASC'}`
        : '';
      records = await executeCsv(
        `SELECT * FROM (${query}) AS husklet_page${order} LIMIT ${request.range.count} OFFSET ${request.range.start}`,
        signal,
      );
      records.shift();
      cache.set(cacheKey, records);
    }
    return records.slice(0, request.range.count).map((record, offset) => ({
      key: request.range.start + offset,
      cells: record.map((value) => ({ Text: value })),
    }));
  };

  const surface = render(
    React.createElement(
      Column,
      { gap: 1, pad: 2, grow: true },
      React.createElement(
        Row,
        { gap: 1, align: 'center' },
        React.createElement(Heading, { label: container.name, scale: 'title' }),
        React.createElement(Badge, { label: container.state, tone: 'positive' }),
      ),
      React.createElement(
        Card,
        null,
        React.createElement(
          CardContent,
          null,
          React.createElement(Text, {
            label: `${container.image} · ${processes.processes.length} processes · ${networks.length} networks · ${total.toLocaleString()} rows`,
          }),
        ),
      ),
      React.createElement(DataTable, { schema, source, grow: true }),
    ),
    session,
    { title: `Postgres · ${container.name}`, rows: provideRows },
  );
  await surface.ready;
  await surface.source({ Open: { source, columns: schema } });
  await surface.source({ Length: { source, version, rows: total } });
  await surface.flush();
  process.stdout.write(
    `${JSON.stringify({ container: container.id, processes: processes.processes.length, queryRows: total, networks: networks.length, slot: surface.slot })}\n`,
  );
  // Keep serving viewport requests until Husklet stops this extension. Without
  // this wait, the first render succeeds and every later scroll is disconnected.
  await session.closed;
} finally {
  await session.close();
}
