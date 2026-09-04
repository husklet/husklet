import React from 'react';
import { Column, FileBrowser, Heading, Text } from '@husklet/react';
import type { ColumnSpec, InterfaceSourceMutation } from '@husklet/react';


export const FILE_BROWSER_STORY = 'Browse workspace files';
export const FILE_SOURCE = 104;
export const FILE_WINDOW_LIMIT = 32;
export const FILE_SCHEMA: readonly ColumnSpec[] = Object.freeze([
  { key: 'name', title: 'Name', width: 'fill' },
  { key: 'kind', title: 'Kind', width: { chars: 12 } },
  { key: 'size', title: 'Size', width: { chars: 12 } },
]);

type SourceSender = (_call: string, argument: { mutation: InterfaceSourceMutation }) => Promise<void>;
type WindowRequest = { source: number; version: number; id: number; range: { start: number; count: number } };
type FileRecord = { name: string; kind: string; size: string };

function windowRequest(value: unknown): WindowRequest | null {
  if (value === null || typeof value !== 'object') return null;
  const request = value as Record<string, unknown>;
  if (request.range === null || typeof request.range !== 'object') return null;
  const range = request.range as Record<string, unknown>;
  return Number.isSafeInteger(request.source) && Number.isSafeInteger(request.version)
    && Number.isSafeInteger(request.id) && Number.isSafeInteger(range.start) && Number(range.start) >= 0
    && Number.isSafeInteger(range.count) && Number(range.count) >= 0
    ? request as WindowRequest
    : null;
}

export class FileSource {
  readonly send: SourceSender;
  readonly version: number;
  readonly files: readonly FileRecord[];

  constructor(send: SourceSender = async () => {}) {
    this.send = send;
    this.version = 1;
    this.files = Array.from({ length: 128 }, (_, index) => ({
      name: index === 0 ? 'src/' : `src/module-${index}.rs`,
      kind: index === 0 ? 'directory' : 'Rust source',
      size: index === 0 ? '—' : `${index + 1} KiB`,
    }));
  }

  async publish() {
    await this.send('source_resize', { mutation: { Length: { source: FILE_SOURCE, version: this.version, rows: this.files.length } } });
  }

  answer(value: unknown) {
    const request = windowRequest(value);
    if (!request) return null;
    if (request.source !== FILE_SOURCE || request.version !== this.version) return null;
    const count = Math.min(request.range.count, FILE_WINDOW_LIMIT, Math.max(0, this.files.length - request.range.start));
    const rows = this.files.slice(request.range.start, request.range.start + count).map((file, offset) => ({
      id: request.range.start + offset + 1,
      cells: [{ Text: file.name }, { Text: file.kind }, { Code: file.size }],
    }));
    return { source: FILE_SOURCE, version: this.version, request: request.id, range: request.range, rows };
  }
}

export function FileBrowserStory() {
  return (
    <Column gap={2} grow={true}>
      <Heading label={'Workspace files'} scale={'title'} />
      <Text label={'128 logical entries; the host requests at most 32 visible rows.'} />
      <FileBrowser source={FILE_SOURCE} schema={FILE_SCHEMA} grow={true} />
    </Column>
  );
}
