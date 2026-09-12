import { connect, workspace } from '@husklet/client';
declare const process: { argv: string[]; stdout: { write(value: string): void } };

type Configuration = { path: string; slot: string; prompt: string; deadlineMs?: number };
const configuration = JSON.parse(process.argv[2] ?? 'null') as Configuration | null;
if (!configuration?.path || !configuration.slot || !configuration.prompt) {
  throw new TypeError('usage: llm-terminal-agent.ts JSON(path, slot, prompt)');
}

const session = await connect({ path: configuration.path, pendingLimit: 8, timeout: 5_000 });
try {
  const terminal = workspace(session).terminal;
  const context = await terminal.readAllStable({ lines: 80, attempts: 3 });
  const contextIncomplete =
    !context.complete ||
    context.panes.some(({ readable }) => readable.kind === 'ui' && !readable.complete);
  const selected = context.panes.find(({ pane }) => pane.slot === configuration.slot);
  if (!selected) throw new Error('pane is not available in the bounded inventory');
  const observed = selected.readable;
  if (observed.kind === 'ui') {
    process.stdout.write(
      `${JSON.stringify({ context: context.panes.map(({ pane, readable }) => ({ slot: pane.slot, kind: readable.kind, text: readable.text })), incomplete: contextIncomplete, selected: { kind: 'ui', text: observed.text, complete: observed.complete } })}\n`,
    );
  } else {
    const deadlineMs = configuration.deadlineMs ?? 2_000;
    if (!Number.isSafeInteger(deadlineMs) || deadlineMs < 1 || deadlineMs > 30_000) {
      throw new RangeError('deadlineMs must be between 1 and 30000ms');
    }
    const cancellation = new AbortController();
    const deadline = setTimeout(() => cancellation.abort('agent interaction deadline'), deadlineMs);
    try {
      const result = await terminal.writeObservedAndWaitForQuietText(
        observed.snapshot,
        `${configuration.prompt}\n`,
        {
          lines: 80,
          quietMs: 150,
          timeoutMs: 5_000,
          signal: cancellation.signal,
        },
      );
      process.stdout.write(
        `${JSON.stringify({ context: context.panes.map(({ pane, readable }) => ({ slot: pane.slot, kind: readable.kind, text: readable.text })), incomplete: contextIncomplete || !result.settled || result.replaced || (result.changed && result.after.kind === 'ui' && !result.after.complete), selected: { kind: 'terminal', before: observed.text, after: result.changed ? result.after.text : null, afterKind: result.changed ? result.after.kind : null, replacement: result.replaced } })}\n`,
      );
    } finally {
      clearTimeout(deadline);
    }
  }
} finally {
  await session.close();
}
