import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { InMemoryTransport } from '@modelcontextprotocol/sdk/inMemory.js';
import { connect } from '@husklet/react';
import { createServer } from '../src/index.js';

const socket = process.argv[2];
if (!socket) throw new Error('usage: native-socket-e2e.mjs <extension-socket>');

const session = await connect({ path: socket, pendingLimit: 8, timeout: 5_000 });
const server = createServer(session);
const client = new Client({ name: 'husklet-native-e2e', version: '1' });
const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
await Promise.all([server.connect(serverTransport), client.connect(clientTransport)]);

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
process.stdout.write(xml);

await client.close();
await server.close();
session.close();
