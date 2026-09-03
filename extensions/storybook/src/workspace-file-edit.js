import React, { useEffect, useState } from 'react';
import {
  Button, CodeView, Column, ConfirmAction, Heading, InlineMessage, List,
  ListItemButton, Row, Text, TextArea,
} from '@husklet/react';

const { createElement: h } = React;

export const WORKSPACE_FILE_EDIT_STORY = 'Workspace file change review';
export const FILE_LIMIT = 16;
export const PATH_LIMIT = 160;
export const CONTENT_LIMIT = 4_096;

const cleanText = (value, limit) => String(value ?? '').slice(0, limit);
const confined = (value) => {
  const path = cleanText(value, PATH_LIMIT).replace(/\\/g, '/');
  return path && !path.startsWith('/') && path.split('/').every((part) => part && part !== '.' && part !== '..');
};

export function boundedFiles(files) {
  return files.slice(0, FILE_LIMIT).map((file) => ({
    path: cleanText(file.path, PATH_LIMIT),
    content: cleanText(file.content, CONTENT_LIMIT),
  })).filter(({ path }) => confined(path));
}

const initial = boundedFiles([
  { path: 'src/server.js', content: "import http from 'node:http';\n\nhttp.createServer(handler).listen(8080);\n" },
  { path: 'config/runtime.json', content: '{\n  "workers": 4,\n  "logLevel": "info"\n}\n' },
  { path: 'README.md', content: '# API service\n\nRun `npm start`.\n' },
]);

export function WorkspaceFileEditStory() {
  const [files, setFiles] = useState(initial);
  const [selectedPath, setSelectedPath] = useState(initial[0]?.path ?? '');
  const selected = files.find(({ path }) => path === selectedPath) ?? files[0];
  const [draft, setDraft] = useState(selected?.content ?? '');
  const [status, setStatus] = useState('Select a confined workspace-relative path.');
  useEffect(() => setDraft(selected?.content ?? ''), [selected?.path]);
  const save = () => {
    setFiles((current) => current.map((file) => file.path === selected.path ? { ...file, content: draft } : file));
    setStatus(`Wrote ${draft.length} bounded bytes to ${selected.path}.`);
  };
  const rename = () => {
    const next = selected.path.replace(/(\.[^./]+)?$/, '.review$1');
    setFiles((current) => current.map((file) => file.path === selected.path ? { ...file, path: next } : file));
    setSelectedPath(next);
    setStatus(`Renamed ${selected.path} to ${next}.`);
  };

  return h(Column, { gap: 2, grow: true },
    h(Heading, { label: 'Workspace file change review', scale: 'title' }),
    h(Text, { label: 'Every operation stays inside the selected workspace and uses a normalized relative path. Content and inventory are independently bounded.', wrap: true }),
    h(Row, { gap: 2, wrap: true, grow: true },
      h(List, { label: 'Workspace files' }, ...files.map((file) => h(ListItemButton, {
        key: file.path, label: `${file.path} · ${file.content.length} bytes`, selected: file.path === selected.path,
        onInvoke: () => { setSelectedPath(file.path); setStatus(`Read ${file.content.length} bounded bytes from ${file.path}.`); },
      }))),
      selected ? h(Column, { gap: 2, grow: true },
        h(Heading, { label: selected.path, scale: 'body' }),
        h(CodeView, { value: selected.content, monospace: true, grow: true }),
        h(TextArea, { value: draft, monospace: true, grow: true, onChange: ({ value }) => setDraft(cleanText(value, CONTENT_LIMIT)) }),
        h(Row, { gap: 2, wrap: true },
          h(Button, { label: 'Save bounded draft', onInvoke: save }),
          h(Button, { label: 'Rename for review', onInvoke: rename }),
          h(ConfirmAction, { authorityKey: selected.path, label: 'Delete file', confirmLabel: 'Confirm deletion', question: `Delete ${selected.path}?`, onConfirm: async () => setStatus(`Deletion confirmed for confined path ${selected.path}.`) })))
        : h(InlineMessage, { label: 'No confined files are available.', tone: 'neutral' })),
    h(InlineMessage, { label: status, tone: 'neutral' }));
}
