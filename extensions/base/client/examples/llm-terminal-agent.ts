import { connect, workspace } from '@husklet/client';
declare const process: { argv: string[]; stdout: { write(value: string): void } };

type Configuration = { path: string; slot: string; prompt: string };
const configuration = JSON.parse(process.argv[2] ?? 'null') as Configuration | null;
if (!configuration?.path || !configuration.slot || !configuration.prompt) {
  throw new TypeError('usage: llm-terminal-agent.ts JSON(path, slot, prompt)');
}

const session = await connect({ path: configuration.path, pendingLimit: 8, timeout: 5_000 });
try {
  const terminal = workspace(session).terminal;
  const context = await terminal.readAll({ lines: 80 });
  const selected = context.panes.find(({ pane }) => pane.slot === configuration.slot);
  if (!selected || selected.pane.kind !== 'terminal')
    throw new Error('terminal pane is not available');
  const pane = selected.pane;
  const observed = selected.readable;
  if (observed.kind !== 'terminal') throw new Error('pane changed occupant before observation');
  const result = await terminal.writeAndWait(
    pane.slot,
    observed.snapshot.generation ?? pane.generation,
    observed.snapshot.revision ?? pane.revision,
    `${configuration.prompt}\n`,
    { lines: 80, timeoutMs: 2_000 },
  );
  process.stdout.write(
    `${JSON.stringify({ context: context.panes.map(({ pane, readable }) => ({ slot: pane.slot, kind: readable.kind, text: readable.text })), incomplete: !context.complete, before: observed.text, after: result.changed ? result.after.lines.join('\n') : null })}\n`,
  );
} finally {
  await session.close();
}
