// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import { Column, FileBrowser, Heading, Text } from '@husklet/react';

const { createElement: h } = React;

export const FILE_BROWSER_STORY = 'Browse workspace files';
export const FILE_SOURCE = 104;
export const FILE_WINDOW_LIMIT = 32;
export const FILE_SCHEMA = Object.freeze([
  { key: 'name', title: 'Name', width: 'fill' },
  { key: 'kind', title: 'Kind', width: { chars: 12 } },
  { key: 'size', title: 'Size', width: { chars: 12 } },
]);

export class FileSource {
  constructor(send = async () => {}) {
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

  answer(request) {
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
  return h(Column, { gap: 2, grow: true },
    h(Heading, { label: 'Workspace files', scale: 'title' }),
    h(Text, { label: '128 logical entries; the host requests at most 32 visible rows.' }),
    h(FileBrowser, { source: FILE_SOURCE, schema: FILE_SCHEMA, grow: true }),
  );
}
