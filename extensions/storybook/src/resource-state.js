import React, { useState } from 'react';
import { Button, Column, Heading, List, ListItemText, ListRow, ResourceState, Row, Text } from '@husklet/react';

export const RESOURCE_STATE_STORY = 'Container inventory states';
const h = React.createElement;
export function ResourceStateStory() {
  const [state, setState] = useState('loading');
  return h(Column, { gap: 2, grow: true },
    h(Heading, { label: 'Container inventory states', scale: 'title' }),
    h(Text, { label: 'Loading, empty, failure, and ready are mutually exclusive; only failure offers retry.' }),
    h(Row, { gap: 1 }, ...['loading', 'empty', 'error', 'ready'].map((next) => h(Button, { key: next, label: next, enabled: state !== next, onInvoke: () => setState(next) }))),
    h(ResourceState, { state, loadingLabel: 'Loading workspace containers…', emptyLabel: 'No containers', emptyDetail: 'Create one to begin running a service.', error: 'Container inventory is temporarily unavailable.', retryLabel: 'Retry inventory', onRetry: () => setState('loading') },
      h(List, { label: 'Workspace containers' },
        h(ListRow, null, h(ListItemText, { label: 'api · running', detail: 'sha256:3f9a…' })),
        h(ListRow, null, h(ListItemText, { label: 'worker · paused', detail: 'sha256:7c20…' })))));
}
