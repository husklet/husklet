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
let stopping = false;

const stop = async ({ diagnostic, code = 0 } = {}) => {
  if (stopping) return;
  stopping = true;
  if (diagnostic) process.stderr.write(`husklet-mcp: ${diagnostic}\n`);
  const forced = setTimeout(() => process.exit(code), 1_000);
  forced.unref();
  try { await server?.close(); } catch { /* The MCP peer may already be gone. */ }
  session?.close();
  process.exitCode = code;
};

try {
  const options = parseCli(process.argv.slice(2));
  if (options.help) {
    process.stdout.write(`${usage}\n`);
  } else {
    let connected = false;
    session = await connect({
      path: options.socket,
      pendingLimit: 32,
      timeout: 30_000,
      connectTimeout: 5_000,
      onClose: (error) => {
        if (connected && !stopping) void stop({ diagnostic: `host authority connection ended: ${error.message}`, code: 1 });
      },
    });
    connected = true;
    const hosting = await workspace(session).info();
    assertWorkspace(hosting, options.workspace);
    server = createServer(session);
    await server.connect(new StdioServerTransport());
  }
} catch (error) {
  await stop({ code: 1 });
  process.stderr.write(`husklet-mcp: startup failed: ${error instanceof Error ? error.message : String(error)}\n`);
  process.stderr.write(`Run husklet-mcp --help for usage.\n`);
}

process.stdin.once('end', () => { void stop(); });
process.once('SIGINT', () => { void stop(); });
process.once('SIGTERM', () => { void stop(); });
