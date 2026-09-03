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
let terminal = inventory.panes?.find((pane) => pane.slot === terminalSlot && pane.kind === 'terminal');
if (!terminal) {
  throw new Error(`real terminal absent from pane discovery: ${listed.content[0].text}`);
}
const advertised = inventory.panes?.find((pane) => pane.kind === 'surface' && pane.provider?.extension && pane.provider?.provider);
if (!advertised) throw new Error(`no discoverable provider identity in pane inspection: ${JSON.stringify(inventory)}`);
const discovered = { extension: advertised.provider.extension, provider: advertised.provider.provider };
const topologyBefore = await client.callTool({ name: 'husklet_terminal_topology', arguments: {} });
const settledInventory = JSON.parse((await client.callTool({ name: 'husklet_pane_list', arguments: {} })).content[0].text);
terminal = settledInventory.panes.find((pane) => pane.slot === terminalSlot && pane.kind === 'terminal');
const switched = await client.callTool({
  name: 'husklet_terminal_switch_occupant',
  arguments: { slot: terminalSlot, generation: terminal.generation, target: { kind: 'surface', ...discovered } },
});
if (switched.isError) throw new Error(`surface switch failed: ${switched.content?.[0]?.text}; terminal=${JSON.stringify(terminal)} inventory=${JSON.stringify(inventory)}`);
const afterSwitch = JSON.parse((await client.callTool({ name: 'husklet_pane_list', arguments: {} })).content[0].text);
const mounted = afterSwitch.panes.find((pane) => pane.slot === terminalSlot);
if (mounted.kind !== 'surface' || mounted.provider?.extension !== discovered.extension || mounted.provider?.provider !== discovered.provider) {
  throw new Error(`wrong switched occupant: ${JSON.stringify(mounted)}`);
}
const mountedSnapshot = await client.callTool({ name: 'husklet_pane_snapshot', arguments: { slot: terminalSlot } });
if (mountedSnapshot.isError || !mountedSnapshot.content?.[0]?.text?.includes(`<pane slot="${terminalSlot}"`)
  || !mountedSnapshot.content?.[0]?.text?.includes('generation="')) {
  throw new Error(`discovered provider semantics absent after switch: ${mountedSnapshot.content?.[0]?.text}`);
}
const stale = await client.callTool({
  name: 'husklet_terminal_switch_occupant',
  arguments: { slot: terminalSlot, generation: terminal.generation, target: { kind: 'terminal' } },
});
if (!stale.isError) throw new Error('stale generation unexpectedly switched the pane');
const afterStale = JSON.parse((await client.callTool({ name: 'husklet_pane_list', arguments: {} })).content[0].text);
const stillMounted = afterStale.panes.find((pane) => pane.slot === terminalSlot);
if (stillMounted.kind !== 'surface' || stillMounted.provider?.extension !== discovered.extension) {
  throw new Error(`stale refusal mutated occupant: ${JSON.stringify(stillMounted)}`);
}
const restored = await client.callTool({
  name: 'husklet_terminal_switch_occupant',
  arguments: { slot: terminalSlot, generation: stillMounted.generation, target: { kind: 'terminal' } },
});
if (restored.isError) throw new Error(`terminal restore failed: ${restored.content?.[0]?.text}`);
const topologyAfter = await client.callTool({ name: 'husklet_terminal_topology', arguments: {} });
const stableTopology = (result) => JSON.stringify(JSON.parse(result.content[0].text), (key, value) => (
  key === 'ratio_per_mille' || key === 'focused' ? undefined : value
));
if (stableTopology(topologyAfter) !== stableTopology(topologyBefore)) {
  throw new Error(`topology changed across occupant switch: ${topologyBefore.content?.[0]?.text} => ${topologyAfter.content?.[0]?.text}`);
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
if (!xml.includes('<label>agent-extension</label>') || !xml.includes('<label>Granted capabilities</label>')) {
  throw new Error(`native lifecycle card or consent context absent from ${xml}`);
}
if (!xml.includes('<value>container-read, interface</value>') && !xml.includes('<value>interface, container-read</value>')) {
  throw new Error(`native lifecycle grants absent from ${xml}`);
}
const extensions = xml.match(/<node id="(\d+)"[^>]*><label>Extensions<\/label>/);
if (!extensions) throw new Error(`Extensions semantic node absent from ${xml}`);
const generation = Number(xml.match(/generation="(\d+)"/)?.[1]);
const revision = Number(xml.match(/revision="(\d+)"/)?.[1]);
if (!Number.isSafeInteger(generation)) throw new Error(`pane generation absent from ${xml}`);
if (!Number.isSafeInteger(revision)) throw new Error(`semantic revision absent from ${xml}`);

const acted = await client.callTool({
  name: 'husklet_pane_action',
  arguments: { slot: 'workspace', generation, revision, node: Number(extensions[1]), action: 'invoke' },
});
if (acted.isError) throw new Error(`semantic action failed: ${acted.content?.[0]?.text}`);

const initial = await client.callTool({
  name: 'husklet_pane_read',
  arguments: { slot: terminalSlot, lines: 100 },
});
if (initial.isError || !initial.content?.[0]?.text?.includes('agent-ready')) {
  throw new Error(`real terminal pane did not expose guest output: ${initial.content?.[0]?.text}`);
}
const initialXml = initial.content[0].text;
const outerIdentity = initialXml.match(/<husklet-pane[^>]*generation="(\d+)"[^>]*revision="(\d+)"/);
if (!outerIdentity || Number(outerIdentity[1]) < 1 || !Number.isSafeInteger(Number(outerIdentity[2]))) {
  throw new Error(`terminal pane read did not preserve its observed immutable identity: ${initialXml}`);
}
let writeIdentity = outerIdentity;
let written;
const writeDeadline = Date.now() + 5_000;
do {
  written = await client.callTool({
    name: 'husklet_terminal_write',
    arguments: { slot: terminalSlot, generation: Number(writeIdentity[1]), revision: Number(writeIdentity[2]), input: 'agent-status\n' },
  });
  if (!written.isError || !written.content?.[0]?.text?.includes('stale pane identity')) break;
  const refreshed = await client.callTool({ name: 'husklet_pane_read', arguments: { slot: terminalSlot, lines: 100 } });
  writeIdentity = refreshed.content?.[0]?.text?.match(/<husklet-pane[^>]*generation="(\d+)"[^>]*revision="(\d+)"/);
  if (!writeIdentity) throw new Error(`terminal identity refresh failed: ${refreshed.content?.[0]?.text}`);
} while (Date.now() < writeDeadline);
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
