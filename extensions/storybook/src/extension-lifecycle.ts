// @ts-nocheck -- legacy story typing is migrated incrementally.
import React, { useState } from 'react';
import {
  Button, Card, CardActions, CardContent, CardHeader, Chip, Column, ConfirmAction,
  Heading, InlineMessage, List, ListItemButton, Row, Text,
} from '@husklet/react';

const { createElement: h } = React;

export const EXTENSION_LIFECYCLE_STORY = 'Installed extension lifecycle';
export const EXTENSION_LIMIT = 8;
export const GRANT_LIMIT = 12;
export const FIELD_LIMIT = 96;
const NAME = /^[a-z0-9][a-z0-9-]{0,62}$/;
const clean = (value) => String(value ?? '').replace(/[\r\n\t]/g, ' ').slice(0, FIELD_LIMIT);

export function boundedExtensions(extensions) {
  return extensions.slice(0, EXTENSION_LIMIT).map((extension) => ({
    name: NAME.test(extension.name ?? '') ? extension.name : '',
    version: clean(extension.version), digest: clean(extension.digest),
    status: ['running', 'stopped', 'failed'].includes(extension.status) ? extension.status : 'failed',
    grants: [...new Set((extension.grants ?? []).map(clean).filter(Boolean))].slice(0, GRANT_LIMIT),
    update: extension.update ? {
      version: clean(extension.update.version), digest: clean(extension.update.digest),
      requested: [...new Set((extension.update.requested ?? []).map(clean).filter(Boolean))].slice(0, GRANT_LIMIT),
    } : null,
  })).filter(({ name, digest }) => name && digest.startsWith('sha256:'));
}

const initial = boundedExtensions([
  { name: 'workspace-manager', version: '1.4.0', digest: 'sha256:manager-generation-14', status: 'running', grants: ['workspaces:read', 'workspaces:control', 'containers:read'], update: { version: '1.5.0', digest: 'sha256:manager-generation-15', requested: ['workspaces:read', 'workspaces:control', 'containers:read', 'containers:control'] } },
  { name: 'storybook', version: '1.4.0', digest: 'sha256:storybook-generation-9', status: 'stopped', grants: ['interface:render'] },
]);

export function ExtensionLifecycleStory() {
  const [extensions, setExtensions] = useState(initial);
  const [selectedName, setSelectedName] = useState(initial[0]?.name ?? '');
  const [review, setReview] = useState(false);
  const [status, setStatus] = useState('Select an installed extension generation.');
  const selected = extensions.find(({ name }) => name === selectedName) ?? extensions[0];
  const lifecycle = (next) => {
    setExtensions((current) => current.map((extension) => extension.name === selected.name
      ? { ...extension, status: next } : extension));
    setStatus(`${next === 'running' ? 'Started' : 'Stopped'} ${selected.name} at ${selected.digest}.`);
  };

  return h(Column, { gap: 2, grow: true },
    h(Heading, { label: 'Installed extension lifecycle', scale: 'title' }),
    h(Text, { label: 'Inspect the running digest and recorded grants before lifecycle control. Updates never inherit newly requested authority.', wrap: true }),
    h(Row, { gap: 2, wrap: true, grow: true },
      h(List, { label: 'Installed extensions' }, ...extensions.map((extension) => h(ListItemButton, {
        key: extension.name, label: `${extension.name} · ${extension.status}`, selected: extension.name === selected.name,
        onInvoke: () => { setSelectedName(extension.name); setReview(false); setStatus(`Selected ${extension.name}.`); },
      }))),
      selected ? h(Card, { label: selected.name, variant: 'outline', grow: true },
        h(CardHeader, { label: `${selected.name} ${selected.version}`, detail: selected.status }),
        h(CardContent, { gap: 2 },
          h(Text, { label: selected.digest, monospace: true, wrap: true }),
          h(Row, { gap: 1, wrap: true }, ...selected.grants.map((grant) => h(Chip, { key: grant, label: grant, variant: 'outline' }))),
          review && selected.update ? h(Column, { gap: 1 },
            h(Heading, { label: `Review update ${selected.update.version}`, scale: 'section' }),
            h(Text, { label: selected.update.digest, monospace: true, wrap: true }),
            h(Text, { label: `Requested: ${selected.update.requested.join(', ')}`, wrap: true }),
            h(InlineMessage, { label: 'containers:control is new and remains ungranted until explicit consent.', tone: 'warning' }),
            h(Button, { label: 'Keep current grants and update', onInvoke: () => { setReview(false); setStatus(`Update staged for ${selected.name} without widening grants.`); } })) : null),
        h(CardActions, {}, h(Row, { gap: 2, wrap: true },
          h(Button, { label: selected.status === 'running' ? 'Stop extension' : 'Start extension', onInvoke: () => lifecycle(selected.status === 'running' ? 'stopped' : 'running') }),
          ...(selected.update ? [h(Button, { key: 'review', label: 'Review update', onInvoke: () => setReview(true) })] : []),
          h(ConfirmAction, { authorityKey: selected.digest, label: 'Remove extension', confirmLabel: 'Confirm removal', question: `Remove ${selected.name} at ${selected.digest}?`, onConfirm: async () => setStatus(`Removal confirmed for ${selected.name} at ${selected.digest}.`) }))))
        : h(InlineMessage, { label: 'No installed extensions.', tone: 'neutral' })),
    h(InlineMessage, { label: status, tone: 'neutral' }));
}
