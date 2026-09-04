// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import { Accordion, AccordionDetails, AccordionSummary, Button, Column, CommandPalette, Heading, InlineMessage, Menu, MenuItem, Popover, Row, Text } from '@husklet/react';

const { createElement: h, useState } = React;

export const NAVIGATION_STORY = 'Navigation and transient UI';

/** Stateful navigation and dialog interactions that isolated component previews cannot teach. */
export function NavigationDialogsStory() {
  const [expanded, setExpanded] = useState(true);
  const [open, setOpen] = useState(false);
  const [event, setEvent] = useState('No navigation event yet.');
  const [query, setQuery] = useState('');
  return h(Column, { gap: 3 },
    h(Heading, { label: 'Navigation and transient UI', scale: 'title', wrap: true }),
    h(Text, { label: 'Expand details, open the action menu, choose an item, or dismiss it.', wrap: true }),
    h(CommandPalette, {
      value: query,
      placeholder: 'Run a command…',
      gap: 1,
      onChange: (report) => setQuery(String(report.value ?? '')),
      onSubmit: () => setEvent(`Command submitted: ${query || 'none'}.`),
    },
    h(MenuItem, { label: 'Open terminal', icon: 'utilities-terminal-symbolic', onInvoke: () => setEvent('Open terminal selected.') }),
    h(MenuItem, { label: 'Create workspace', icon: 'list-add-symbolic', onInvoke: () => setEvent('Create workspace selected.') })),
    h(Accordion, {
      label: 'Deployment details',
      expanded,
      onExpand: (next) => {
        const value = Boolean(next.expanded ?? next.value);
        setExpanded(value);
        setEvent(value ? 'Deployment details expanded.' : 'Deployment details collapsed.');
      },
    },
    h(AccordionSummary, { label: 'Deployment details', icon: 'info' }),
    h(AccordionDetails, { gap: 1 },
      h(Text, { label: 'Three replicas · healthy', wrap: true }),
      h(Button, { label: 'Open actions', onInvoke: () => { setOpen(true); setEvent('Action menu opened.'); } }),
    )),
    ...(open ? [h(Popover, {
      key: 'actions',
      visible: true,
      onClose: () => { setOpen(false); setEvent('Action menu dismissed.'); },
    }, h(Menu, { gap: 1 },
      h(MenuItem, { label: 'View logs', icon: 'document-open', onInvoke: () => { setOpen(false); setEvent('View logs selected.'); } }),
      h(MenuItem, { label: 'Restart unavailable', enabled: false }),
    ))] : []),
    h(Row, { gap: 1, wrap: true }, h(InlineMessage, { label: event, tone: 'neutral' })),
  );
}
