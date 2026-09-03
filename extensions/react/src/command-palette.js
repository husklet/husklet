import React, { useMemo, useState } from 'react';

import { Button, Column, CommandPalette, EmptyState, List, ListSubheader } from './components.js';

export const COMMAND_PALETTE_ITEM_LIMIT = 256;
export const COMMAND_PALETTE_QUERY_BYTE_LIMIT = 128;
export const COMMAND_PALETTE_TEXT_BYTE_LIMIT = 256;

const encoder = new TextEncoder();

function bounded(value, limit) {
  let output = '';
  for (const character of String(value ?? '')) {
    if (encoder.encode(output + character).byteLength > limit) break;
    output += character;
  }
  return output;
}

function rank(command, query) {
  const needle = query.trim().toLocaleLowerCase();
  if (needle === '') return 0;
  const haystack = `${command.title} ${command.group ?? ''} ${(command.keywords ?? []).join(' ')}`.toLocaleLowerCase();
  let at = 0;
  let score = 0;
  let previous = -2;
  for (const character of needle) {
    const found = haystack.indexOf(character, at);
    if (found < 0) return null;
    score += found === previous + 1 ? 0 : found + 1;
    previous = found;
    at = found + 1;
  }
  return score;
}

export function filterCommands(commands, query) {
  if (!Array.isArray(commands)) throw new TypeError('CommandPaletteView commands must be an array');
  const safeQuery = bounded(query, COMMAND_PALETTE_QUERY_BYTE_LIMIT);
  return commands.slice(0, COMMAND_PALETTE_ITEM_LIMIT).map((command, order) => {
    if (command === null || typeof command !== 'object' || String(command.id ?? '').length === 0) {
      throw new TypeError('every command requires a stable nonblank id');
    }
    const normalized = {
      ...command,
      id: bounded(command.id, COMMAND_PALETTE_TEXT_BYTE_LIMIT),
      title: bounded(command.title, COMMAND_PALETTE_TEXT_BYTE_LIMIT),
      group: bounded(command.group || 'Commands', COMMAND_PALETTE_TEXT_BYTE_LIMIT),
      detail: bounded(command.detail, COMMAND_PALETTE_TEXT_BYTE_LIMIT),
      shortcut: bounded(command.shortcut, 64),
    };
    if (normalized.title.trim() === '') throw new TypeError('every command requires a nonblank title');
    return { command: normalized, score: rank(normalized, safeQuery), order };
  }).filter(({ score }) => score !== null).sort((left, right) => left.score - right.score || left.order - right.order)
    .map(({ command }) => command);
}

/** A bounded keyboard-first command picker composed entirely from native nodes. */
export function CommandPaletteView({ commands = [], initialQuery = '', placeholder = 'Type a command…', emptyLabel = 'No matching commands', onQueryChange, onSelect, ...props }) {
  const [query, setQuery] = useState(() => bounded(initialQuery, COMMAND_PALETTE_QUERY_BYTE_LIMIT));
  const [active, setActive] = useState(0);
  const matches = useMemo(() => filterCommands(commands, query), [commands, query]);
  const selectable = matches.filter((command) => !command.disabled);
  const chosen = selectable[Math.min(active, Math.max(0, selectable.length - 1))];
  const update = (value) => {
    const next = bounded(value, COMMAND_PALETTE_QUERY_BYTE_LIMIT);
    setQuery(next); setActive(0); onQueryChange?.(next);
  };
  const invoke = (command = chosen) => {
    if (!command || command.disabled) return;
    onSelect?.(command); command.onInvoke?.(command);
  };
  const key = (event) => {
    const pressed = event?.key ?? event?.value?.key ?? event?.value;
    if (pressed === 'ArrowDown') setActive((current) => selectable.length === 0 ? 0 : (current + 1) % selectable.length);
    else if (pressed === 'ArrowUp') setActive((current) => selectable.length === 0 ? 0 : (current + selectable.length - 1) % selectable.length);
    else if (pressed === 'Enter') invoke();
  };
  const groups = new Map();
  for (const command of matches) {
    const held = groups.get(command.group) ?? [];
    held.push(command);
    groups.set(command.group, held);
  }
  return React.createElement(Column, { gap: 1, ...props },
    React.createElement(CommandPalette, {
      value: query, placeholder: bounded(placeholder, COMMAND_PALETTE_TEXT_BYTE_LIMIT),
      onChange: (event) => update(event?.value ?? ''), onKey: key, onSubmit: () => invoke(),
    }),
    matches.length === 0
      ? React.createElement(EmptyState, { label: bounded(emptyLabel, COMMAND_PALETTE_TEXT_BYTE_LIMIT) })
      : React.createElement(List, { grow: true }, ...[...groups].flatMap(([group, commandsInGroup]) => [
        React.createElement(ListSubheader, { key: `group:${group}`, label: group }),
        ...commandsInGroup.map((command) => React.createElement(Button, {
          key: `command:${command.id}`,
          label: `${command.title}${command.shortcut ? `  ${command.shortcut}` : ''}`,
          tooltip: command.detail || command.title,
          enabled: !command.disabled,
          destructive: Boolean(command.destructive),
          tone: command.destructive ? 'danger' : command.tone,
          variant: command.id === chosen?.id ? 'filled' : 'ghost',
          onInvoke: () => invoke(command),
        })),
      ])),
  );
}
