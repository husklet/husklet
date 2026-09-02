import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { InMemoryTransport } from '@modelcontextprotocol/sdk/inMemory.js';
import { connect } from '@husklet/react';
import { createServer } from '../src/index.js';

const socket = process.argv[2];
const terminalSlot = process.argv[3];
if (!socket || !terminalSlot) throw new Error('usage: native-socket-e2e.mjs <extension-socket> <terminal-slot>');

const session = await connect({ path: socket, pendingLimit: 8, timeout: 5_000 });
const server = createServer(session);
const client = new Client({ name: 'husklet-native-e2e', version: '1' });
const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
await Promise.all([server.connect(serverTransport), client.connect(clientTransport)]);

const listed = await client.callTool({ name: 'husklet_pane_list', arguments: {} });
if (listed.isError) throw new Error(`pane discovery failed: ${listed.content?.[0]?.text}`);
const inventory = JSON.parse(listed.content[0].text);
if (!inventory.panes?.some((pane) => pane.slot === 'workspace' && pane.kind === 'native')) {
  throw new Error(`native workspace absent from pane discovery: ${listed.content[0].text}`);
}
if (!inventory.panes?.some((pane) => pane.slot === terminalSlot && pane.kind === 'terminal')) {
  throw new Error(`real terminal absent from pane discovery: ${listed.content[0].text}`);
}
const surface = inventory.panes?.find((pane) => pane.kind === 'surface');
if (!surface) throw new Error(`reference extension surface absent from pane discovery: ${listed.content[0].text}`);
const extensionSnapshot = await client.callTool({
  name: 'husklet_pane_snapshot', arguments: { slot: surface.slot },
});
if (!extensionSnapshot.content?.[0]?.text?.includes('<label>Containers</label>')) {
  throw new Error(`reference extension semantics absent: ${extensionSnapshot.content?.[0]?.text}`);
}

const snapshot = await client.callTool({
  name: 'husklet_pane_snapshot',
  arguments: { slot: 'workspace' },
});
const xml = snapshot.content[0].text;
const settings = xml.match(/<node id="(\d+)"[^>]*><label>Settings<\/label>/);
if (!settings) throw new Error(`Settings semantic node absent from ${xml}`);
const revision = Number(xml.match(/revision="(\d+)"/)?.[1]);
if (!Number.isSafeInteger(revision)) throw new Error(`semantic revision absent from ${xml}`);

const acted = await client.callTool({
  name: 'husklet_pane_action',
  arguments: { slot: 'workspace', revision, node: Number(settings[1]), action: 'invoke' },
});
if (acted.isError) throw new Error(`semantic action failed: ${acted.content?.[0]?.text}`);

const initial = await client.callTool({
  name: 'husklet_pane_read',
  arguments: { slot: terminalSlot, lines: 100 },
});
if (initial.isError || !initial.content?.[0]?.text?.includes('agent-ready')) {
  throw new Error(`real terminal pane did not expose guest output: ${initial.content?.[0]?.text}`);
}
const written = await client.callTool({
  name: 'husklet_terminal_write',
  arguments: { slot: terminalSlot, input: 'agent-status\n' },
});
if (written.isError) throw new Error(`terminal input failed: ${written.content?.[0]?.text}`);

const deadline = Date.now() + 5_000;
let terminalXml = '';
while (Date.now() < deadline) {
  const observed = await client.callTool({
    name: 'husklet_pane_read',
    arguments: { slot: terminalSlot, lines: 100 },
  });
  terminalXml = observed.content?.[0]?.text ?? '';
  if (!observed.isError && terminalXml.includes('agent-received:agent-status')) break;
  await new Promise((resolve) => setTimeout(resolve, 10));
}
if (!terminalXml.includes('agent-received:agent-status')) {
  throw new Error(`real terminal pane never exposed the guest response: ${terminalXml}`);
}
process.stdout.write(`${xml}\n${extensionSnapshot.content[0].text}\n${terminalXml}`);

await client.close();
await server.close();
session.close();
