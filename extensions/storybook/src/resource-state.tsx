import React, { useState } from 'react';
import { Button, Column, Heading, List, ListItemText, ListRow, ResourceState, Row, Text } from '@husklet/react';

export const RESOURCE_STATE_STORY = 'Container inventory states';
type InventoryState = 'loading' | 'empty' | 'error' | 'ready';
export function ResourceStateStory() {
  const [state, setState] = useState<InventoryState>('loading');
  return (
    <Column gap={2} grow={true}>
      <Heading label={'Container inventory states'} scale={'title'} />
      <Text
        label={'Loading, empty, failure, and ready are mutually exclusive; only failure offers retry.'} />
      <Row gap={1}>
        {(['loading', 'empty', 'error', 'ready'] satisfies InventoryState[]).map((next) => <Button
          key={next}
          label={next}
          enabled={state !== next}
          onInvoke={() => setState(next)} />)}
      </Row>
      <ResourceState
        state={state}
        loadingLabel={'Loading workspace containers…'}
        emptyLabel={'No containers'}
        emptyDetail={'Create one to begin running a service.'}
        error={'Container inventory is temporarily unavailable.'}
        retryLabel={'Retry inventory'}
        onRetry={() => setState('loading')}>
        <List>
          <ListRow>
            <ListItemText label={'api · running'} detail={'sha256:3f9a…'} />
          </ListRow>
          <ListRow>
            <ListItemText label={'worker · paused'} detail={'sha256:7c20…'} />
          </ListRow>
        </List>
      </ResourceState>
    </Column>
  );
}
