#!/usr/bin/env node
import { connect, workspace } from '@husklet/client';

const configuration = JSON.parse(process.argv[2] ?? 'null');
if (
  !configuration ||
  typeof configuration.path !== 'string' ||
  typeof configuration.terminalSlot !== 'string' ||
  typeof configuration.uiSlot !== 'string' ||
  !Number.isSafeInteger(configuration.node) ||
  configuration.node < 0 ||
  !Array.isArray(configuration.input)
) {
  throw new TypeError('usage: agent-control.mjs JSON(path, terminalSlot, uiSlot, node, input[])');
}

const session = await connect({ path: configuration.path });
try {
  const host = workspace(session);
  const inventory = await host.terminal.readAll({ lines: 40 });
  const terminal = inventory.panes.find((entry) => entry.pane.slot === configuration.terminalSlot);
  const ui = inventory.panes.find((entry) => entry.pane.slot === configuration.uiSlot);
  if (!inventory.complete || terminal?.readable.kind !== 'terminal' || ui?.readable.kind !== 'ui') {
    throw new Error('configured terminal and UI panes are not present in bounded inventory');
  }
  const written = await host.terminal.writeAndWait(
    terminal.pane.slot,
    terminal.readable.snapshot.generation,
    terminal.readable.snapshot.revision,
    configuration.input,
    { lines: 40, timeoutMs: 1_000 },
  );
  const acted = await host.terminal.inspectAndAct(
    ui.pane.slot,
    { node: configuration.node, action: 'invoke' },
    { timeoutMs: 1_000 },
  );
  process.stdout.write(
    `${JSON.stringify({
      terminal: terminal.readable.text,
      terminalAfter: written.changed ? written.after.lines.join('\n') : null,
      ui: ui.readable.text,
      uiAfter: acted.changed ? acted.after.text : null,
    })}\n`,
  );
} finally {
  await session.close();
}
