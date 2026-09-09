import React from 'react';
import {
  Badge,
  Code,
  Column,
  Heading,
  Progress,
  Text,
  connect,
  render,
  workspace,
} from '@husklet/react';

declare const process: { argv: string[]; stdout: { write(value: string): void } };
type Configuration = {
  path: string;
  containerName: string;
  source: string;
  helper: string[];
  credential?: string;
};
const configuration = JSON.parse(process.argv[2] ?? 'null') as Configuration;
const session = await connect({ path: configuration.path, pendingLimit: 8 });
try {
  const host = workspace(session);
  const container = (await host.containers.list()).find(
    (candidate) => candidate.name === configuration.containerName && candidate.state === 'running',
  );
  if (!container) throw new Error('debug target container is not running');
  const processes = [];
  for await (const page of host.containers.processPages(container.id, { limit: 32 })) {
    processes.push(...page.processes);
  }

  const source = await host.files.stat(configuration.source);
  if (!source.identity) throw new Error('debug source has no stable identity');
  const exact = await host.files.readText(configuration.source, {
    observed: source.identity,
    maxBytes: 1024 * 1024,
  });
  const stdout: string[] = [];
  const stderr: string[] = [];
  const result = await host.containers.execLines(
    container.id,
    container.generation,
    {
      command: configuration.helper,
      credentials: configuration.credential
        ? [['DEBUG_TOKEN', configuration.credential]]
        : undefined,
      maxLineBytes: 256 * 1024,
      pageLimit: 4,
      onStderr: (text) => {
        stderr.push(text);
      },
    },
    (line) => {
      stdout.push(line);
    },
  );
  if (result.execution.exit_code !== 0) throw new Error(stderr.join('') || 'debug helper failed');

  // A real editor would derive this from a reviewed diagnostic action. Identity observation makes
  // a concurrent rename or edit fail instead of overwriting a different file.
  const replacement = exact.text.replace('debugger;', 'debugger; // reviewed');
  const written =
    replacement === exact.text
      ? exact.identity
      : await host.files.writeObserved(
          configuration.source,
          exact.identity,
          new TextEncoder().encode(replacement),
        );
  const surface = render(
    React.createElement(
      Column,
      { gap: 2, pad: 3, grow: true },
      React.createElement(Heading, { label: 'Code diagnostics', scale: 'title' }),
      React.createElement(Badge, {
        label: result.execution.exit_code === 0 ? 'Helper completed' : 'Helper failed',
        tone: result.execution.exit_code === 0 ? 'positive' : 'danger',
      }),
      React.createElement(Progress, { fraction: 1 }),
      React.createElement(Text, { label: `${processes.length} target processes inspected` }),
      React.createElement(Code, { label: stdout.join('\n').slice(0, 4096) || 'No diagnostics' }),
    ),
    session,
    { title: 'Diagnostics' },
  );
  await surface.ready;
  await surface.flush();
  process.stdout.write(JSON.stringify({ executionId: result.executionId, written }));
} finally {
  await session.close();
}
