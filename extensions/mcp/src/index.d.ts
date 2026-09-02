import type { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import type { Session } from '@husklet/react';
import type { ZodType } from 'zod';

export interface ToolResult { content: Array<{ type: 'text'; text: string }> }
export interface ToolDefinition {
  name: string;
  description: string;
  inputSchema: ZodType;
  run(input: unknown): Promise<ToolResult>;
}

export function tools(api: ReturnType<typeof import('@husklet/react').workspace>): ToolDefinition[];
export function createServer(session: Session): McpServer;
