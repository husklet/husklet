#!/usr/bin/env node
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { connect } from '@husklet/react';
import { createServer } from './index.js';

const session = await connect({ pendingLimit: 32, timeout: 30_000 });
const server = createServer(session);
await server.connect(new StdioServerTransport());

const close = () => { session.close(); process.exit(0); };
process.once('SIGINT', close);
process.once('SIGTERM', close);
