// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import { Column, CommandPaletteView, Heading, InlineMessage, Text } from '@husklet/react';

const { useState } = React;

export const COMMAND_PALETTE_STORY = 'Command palette workflow';

const commands = [
  { id: 'terminal.new', title: 'New terminal', group: 'Workspace', shortcut: '⌘T', keywords: ['shell', 'pane'] },
  { id: 'pane.split', title: 'Split pane right', group: 'Workspace', shortcut: '⌘D', keywords: ['layout'] },
  { id: 'container.restart', title: 'Restart selected container', group: 'Containers', keywords: ['reboot'] },
  { id: 'container.logs', title: 'Show container logs', group: 'Containers', keywords: ['stdout', 'stderr'] },
  { id: 'remote.locked', title: 'Open remote workspace', group: 'Remote', disabled: true, detail: 'No remote authority granted.' },
  { id: 'workspace.remove', title: 'Remove workspace', group: 'Danger', destructive: true, detail: 'Requires explicit confirmation.' },
];

export function CommandPaletteStory() {
  const [status, setStatus] = useState('Type “log”, use ↑/↓, then press Enter.');
  return (
    <Column gap={2} grow={true} width={{ maximum: { chars: 76 } }}>
      <Heading label={'Keyboard-first workspace commands'} scale={'title'} />
      <Text
        label={'Fuzzy matching keeps grouped results, stable identities, disabled authority, and destructive semantics visible.'}
        wrap={true} />
      <CommandPaletteView
        commands={commands}
        placeholder={'Search workspace commands…'}
        onQueryChange={(query) => setStatus(query ? `Filtering for “${query}”` : 'Showing all commands.')}
        onSelect={(command) => setStatus(command.destructive ? `Review required for ${command.title}.` : `Invoked ${command.title}.`)} />
      <InlineMessage label={status} tone={'neutral'} />
    </Column>
  );
}
