#!/usr/bin/env node
import { connect, workspace } from '@husklet/client';

const configuration = JSON.parse(process.argv[2] ?? 'null');
if (!configuration || typeof configuration.path !== 'string'
  || typeof configuration.terminalSlot !== 'string' || typeof configuration.uiSlot !== 'string'
  || !Number.isSafeInteger(configuration.node) || configuration.node < 0
  || !Array.isArray(configuration.input)) {
  throw new TypeError('usage: agent-control.mjs JSON(path, terminalSlot, uiSlot, node, input[])');
}

const session = await connect({ path: configuration.path });
try {
  const host = workspace(session);
  const inventory = await host.terminal.panes();
  const terminalPane = inventory.panes.find((pane) => pane.slot === configuration.terminalSlot);
  const uiPane = inventory.panes.find((pane) => pane.slot === configuration.uiSlot);
  if (!terminalPane || terminalPane.kind !== 'terminal' || !uiPane || uiPane.kind === 'terminal') {
    throw new Error('configured terminal and UI panes are not present in bounded inventory');
  }
  const terminal = await host.terminal.toText(terminalPane.slot, { lines: 40 });
  const written = await host.terminal.writeAndWait(
    terminalPane.slot, terminal.snapshot.generation, terminal.snapshot.revision,
    configuration.input, { lines: 40, timeoutMs: 1_000 },
  );
  const ui = await host.terminal.toText(uiPane.slot);
  const acted = await host.terminal.inspectAndAct(
    uiPane.slot, { node: configuration.node, action: 'invoke' }, { timeoutMs: 1_000 },
  );
  process.stdout.write(`${JSON.stringify({
    terminal: terminal.text, terminalAfter: written.changed ? written.after.lines.join('\n') : null,
    ui: ui.text, uiAfter: acted.changed ? acted.after.text : null,
  })}\n`);
} finally {
  await session.close();
}
