import { connect, semanticXml, workspace } from '@husklet/client';

const [socket, terminalSlot] = process.argv.slice(2);
if (!socket || !terminalSlot) throw new Error('usage: native-socket-e2e.mjs <socket> <terminal-slot>');
const session = await connect({ path: socket, pendingLimit: 8, timeout: 5_000 });
try {
  const api = workspace(session);
  const inventory = await api.terminal.panes();
  const terminalPane = inventory.panes.find(({ slot }) => slot === terminalSlot);
  if (!terminalPane
    || !inventory.panes.some(({ slot }) => slot === 'workspace')) throw new Error('pane inventory incomplete');
  const screen = await api.terminal.read(terminalSlot, 40);
  if (!screen.lines.join('\n').includes('agent-ready')) throw new Error('terminal seed absent');
  await api.terminal.writeInput(
    terminalSlot,
    terminalPane.generation,
    terminalPane.revision,
    new TextEncoder().encode('agent-status\n'),
  );
  let nativeTree = await api.terminal.semantics('workspace');
  const nativeXml = semanticXml(nativeTree);
  if (!nativeXml.includes('<label>Extensions</label>')) throw new Error(`native semantics absent from ${nativeXml}`);
  const surface = inventory.panes.find(({ kind, provider }) => kind === 'surface' && provider?.extension === 'containers');
  if (!surface) throw new Error('extension surface absent from pane inventory');
  const tree = await api.terminal.semantics(surface.slot);
  const xml = semanticXml(tree);
  const find = (node) => node?.label === 'Extensions' ? node : (node?.children ?? []).map(find).find(Boolean);
  const nativeTarget = find(nativeTree.root);
  if (!nativeTarget) throw new Error(`native action absent from ${nativeXml}`);
  for (let attempt = 0; ; attempt += 1) {
    const currentTarget = find(nativeTree.root);
    try {
      await api.terminal.act('workspace', {
        generation: nativeTree.generation,
        revision: nativeTree.revision,
        node: currentTarget.id,
        action: 'invoke',
      });
      break;
    } catch (error) {
      if (error?.kind !== 'conflict' || attempt === 4) throw error;
      nativeTree = await api.terminal.semantics('workspace');
    }
  }
  const findAction = (node) => node?.actions?.includes('invoke') ? node : (node?.children ?? []).map(findAction).find(Boolean);
  const target = findAction(tree.root);
  if (!target) throw new Error(`invokable semantic node absent from ${xml}`);
  let terminalText = '';
  for (let attempt = 0; attempt < 100; attempt += 1) {
    terminalText = (await api.terminal.read(terminalSlot, 40)).lines.join('\n');
    if (terminalText.includes('agent-received:agent-status')) break;
  }
  if (!terminalText.includes('agent-received:agent-status')) throw new Error('terminal response absent');
  process.stdout.write(`${nativeXml}\n${xml}\n${terminalText}`);
} finally {
  session.close();
}
