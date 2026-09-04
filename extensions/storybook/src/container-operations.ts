// @ts-nocheck -- legacy story typing is migrated incrementally.
import React, { useState } from 'react';
import {
  Button, Card, CardActions, CardContent, CardHeader, Column, ConfirmAction, Heading,
  InlineMessage, List, ListItemButton, LogView, Row, Table, TableBody, TableCell,
  TableHead, TableRow, Text,
} from '@husklet/react';

const { createElement: h } = React;

export const CONTAINER_OPERATIONS_STORY = 'Container operations console';
export const CONTAINER_LIMIT = 8;
export const PROCESS_LIMIT = 8;
export const LOG_LIMIT = 1_024;

const text = (value, limit) => String(value ?? '').replace(/[\r\n\t]/g, ' ').slice(0, limit);

export function boundedContainers(containers) {
  return containers.slice(0, CONTAINER_LIMIT).map((container) => ({
    id: text(container.id, 80), name: text(container.name, 64), image: text(container.image, 120),
    state: ['running', 'stopped', 'paused'].includes(container.state) ? container.state : 'unknown',
    logs: text(container.logs, LOG_LIMIT),
    processes: (container.processes ?? []).slice(0, PROCESS_LIMIT).map((process) => ({
      pid: Number.isSafeInteger(process.pid) && process.pid >= 0 ? process.pid : 0,
      command: text(process.command, 160), user: text(process.user, 48),
    })),
  })).filter(({ id, name }) => id && name);
}

const sample = boundedContainers([
  { id: 'sha256:api-generation-42', name: 'api', image: 'team/api:1.8.2', state: 'running', logs: 'ready on :8080\nGET /health 200\n', processes: [{ pid: 1, user: 'node', command: 'node server.js' }, { pid: 31, user: 'node', command: 'worker --queue events' }] },
  { id: 'sha256:worker-generation-19', name: 'worker', image: 'team/worker:1.8.2', state: 'paused', logs: 'queue lag: 14\n', processes: [{ pid: 1, user: 'node', command: 'worker --queue default' }] },
  { id: 'sha256:db-generation-7', name: 'database', image: 'postgres:16-alpine', state: 'running', logs: 'database system is ready\n', processes: [{ pid: 1, user: 'postgres', command: 'postgres' }] },
]);

export function ContainerOperationsStory({ containers = sample }) {
  const inventory = boundedContainers(containers);
  const [selectedId, setSelectedId] = useState(inventory[0]?.id ?? '');
  const [inspected, setInspected] = useState(false);
  const [status, setStatus] = useState('Select a container, then inspect its bounded runtime state.');
  const selected = inventory.find(({ id }) => id === selectedId) ?? inventory[0];

  return h(Column, { gap: 2, grow: true },
    h(Heading, { label: 'Container operations console', scale: 'title' }),
    h(Text, { label: 'Immutable container identity stays visible across inspection and control. Destructive stop requires a separate confirmation.', wrap: true }),
    h(Row, { gap: 2, wrap: true, grow: true },
      h(List, { label: 'Workspace containers' }, ...inventory.map((container) => h(ListItemButton, {
        key: container.id, label: `${container.name} · ${container.state}`, selected: container.id === selected?.id,
        onInvoke: () => { setSelectedId(container.id); setInspected(false); setStatus(`Selected ${container.name}.`); },
      }))),
      selected ? h(Card, { label: selected.name, variant: 'outline', grow: true },
        h(CardHeader, { label: selected.name, detail: selected.state }),
        h(CardContent, { gap: 2 },
          h(Text, { label: selected.image }),
          h(Text, { label: selected.id, monospace: true, color: 'text-dim', wrap: true }),
          inspected ? h(Column, { gap: 2 },
            h(Table, { label: 'Initial processes' },
              h(TableHead, {}, h(TableRow, {}, h(TableCell, { label: 'PID' }), h(TableCell, { label: 'User' }), h(TableCell, { label: 'Command' }))),
              h(TableBody, {}, ...selected.processes.map((process) => h(TableRow, { key: `${process.pid}:${process.command}` },
                h(TableCell, { label: String(process.pid) }), h(TableCell, { label: process.user }), h(TableCell, { label: process.command }))))) ,
            h(LogView, { value: selected.logs, monospace: true, grow: true })) : null),
        h(CardActions, {}, h(Row, { gap: 2, wrap: true },
          h(Button, { label: 'Inspect processes and logs', onInvoke: () => { setInspected(true); setStatus(`Loaded ${selected.processes.length} bounded processes for ${selected.name}.`); } }),
          h(Button, { label: 'Restart safely', onInvoke: () => setStatus(`Restart requested for immutable ${selected.id}.`) }),
          h(ConfirmAction, { authorityKey: selected.id, label: 'Stop container', confirmLabel: 'Confirm stop', question: `Stop ${selected.name} at ${selected.id}?`, onConfirm: async () => setStatus(`Stop confirmed for immutable ${selected.id}.`) }),
        ))) : h(InlineMessage, { label: 'No containers are available.', tone: 'neutral' })),
    h(InlineMessage, { label: status, tone: 'neutral' }));
}
