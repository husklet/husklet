#!/usr/bin/env node
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { connect, workspace } from '@husklet/react';
import { createServer } from './index.js';
import { assertWorkspace, parseCli } from './cli-options.js';

const usage = `Usage: husklet-mcp --socket PATH --workspace NAME

Serve bounded Husklet tools over MCP stdio. PATH must be an extension socket
credential for NAME; the server refuses a socket belonging to another workspace.`;

let session;
let server;
try {
  const options = parseCli(process.argv.slice(2));
  if (options.help) {
    process.stdout.write(`${usage}\n`);
  } else {
    session = await connect({ path: options.socket, pendingLimit: 32, timeout: 30_000 });
    const hosting = await workspace(session).info();
    assertWorkspace(hosting, options.workspace);
    server = createServer(session);
    await server.connect(new StdioServerTransport());
  }
} catch (error) {
  session?.close();
  process.stderr.write(`husklet-mcp: ${error instanceof Error ? error.message : String(error)}\n`);
  process.stderr.write(`Run husklet-mcp --help for usage.\n`);
  process.exitCode = 1;
}

const close = async () => {
  try { await server?.close(); } catch { /* The transport may already be gone. */ }
  session?.close();
};
process.once('SIGINT', () => { void close(); });
process.once('SIGTERM', () => { void close(); });
