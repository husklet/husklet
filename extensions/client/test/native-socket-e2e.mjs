import { connect, semanticXml, workspace } from '@husklet/client';

const [socket, terminalSlot] = process.argv.slice(2);
if (!socket || !terminalSlot) throw new Error('usage: native-socket-e2e.mjs <socket> <terminal-slot>');
const session = await connect({ path: socket, pendingLimit: 8, timeout: 5_000 });
try {
  const api = workspace(session);
  const inventory = await api.terminal.panes();
  if (!inventory.panes.some(({ slot }) => slot === terminalSlot)
    || !inventory.panes.some(({ slot }) => slot === 'workspace')) throw new Error('pane inventory incomplete');
  const screen = await api.terminal.read(terminalSlot, 40);
  if (!screen.lines.join('\n').includes('agent-ready')) throw new Error('terminal seed absent');
  await api.terminal.writeInput(terminalSlot, new TextEncoder().encode('agent-status\n'));
  const tree = await api.terminal.semantics('workspace');
  const xml = semanticXml(tree);
  const find = (node) => node?.label === 'Extensions' ? node : (node?.children ?? []).map(find).find(Boolean);
  const target = find(tree.root);
  if (!target) throw new Error(`Extensions semantic node absent from ${xml}`);
  await api.terminal.act('workspace', {
    generation: tree.generation, revision: tree.revision, node: target.id, action: 'invoke',
  });
  process.stdout.write(xml);
} finally {
  session.close();
}

