import React from 'react';
import {
  Column,
  DataTable,
  Heading,
  Text,
  connect,
  render,
  workspace,
  type ColumnSpec,
} from '@husklet/react';
declare const process: { argv: string[]; stdout: { write(value: string): void } };

type Configuration = { path: string; containerId: string };
const configuration = JSON.parse(process.argv[2] ?? 'null') as Configuration | null;
if (!configuration?.path || !configuration.containerId) {
  throw new TypeError('usage: postgres-inspector.ts JSON(path, containerId)');
}
const session = await connect({ path: configuration.path, pendingLimit: 16, timeout: 5_000 });
try {
  const host = workspace(session);
  const [container, processes, logs, networks] = await Promise.all([
    host.containers.inspect(configuration.containerId),
    host.containers.processes(configuration.containerId),
    host.containers.logs(configuration.containerId, { stdout: true, stderr: true }),
    host.networks.list(),
  ]);
  const schema: readonly ColumnSpec[] = processes.titles.map((title, index) => ({
    key: String(index),
    title,
    width: index === 0 ? { chars: 8 } : 'fill',
  }));
  const source = 1;
  const surface = render(
    React.createElement(
      Column,
      { gap: 1, pad: 2, grow: true },
      React.createElement(Heading, { label: container.name, scale: 'title' }),
      React.createElement(Text, {
        label: `${container.image} · ${container.state} · ${networks.length} networks`,
      }),
      React.createElement(DataTable, { schema, source, grow: true }),
    ),
    session,
    { title: `Postgres · ${container.name}` },
  );
  await surface.ready;
  await surface.flush();
  await surface.source({ Open: { source, columns: schema } });
  await surface.source({ Length: { source, version: 1, rows: processes.processes.length } });
  await surface.source({
    Window: {
      source,
      version: 1,
      request: 1,
      range: { start: 0, count: Math.min(128, processes.processes.length) },
      rows: processes.processes
        .slice(0, 128)
        .map((row, key) => ({ key, cells: row.map((value) => ({ Text: value })) })),
    },
  });
  await surface.flush();
  process.stdout.write(
    `${JSON.stringify({ container: container.id, processes: processes.processes.length, logsComplete: logs.eof, networks: networks.length, slot: surface.slot })}\n`,
  );
  await surface.close();
} finally {
  await session.close();
}
