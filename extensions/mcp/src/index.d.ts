import type { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import type { PaneSemanticTree, Session } from '@husklet/react';
import type { ZodType } from 'zod';

export interface ToolResult { content: Array<{ type: 'text'; text: string }> }
/** Arguments for `husklet_terminal_write_bytes`. Base64 must be canonical and decode to at most 65,536 bytes. */
export interface TerminalWriteBytesInput { slot: string; input_base64: string }
export interface ToolDefinition {
  name: string;
  description: string;
  inputSchema: ZodType;
  run(input: unknown): Promise<ToolResult>;
}

export function tools(api: ReturnType<typeof import('@husklet/react').workspace>): ToolDefinition[];
export function createServer(session: Session): McpServer;
export function semanticXml(tree: PaneSemanticTree): string;
export function paneXml(
  terminal: ReturnType<typeof import('@husklet/react').workspace>['terminal'],
  slot: string,
  lines?: number,
): Promise<string>;
