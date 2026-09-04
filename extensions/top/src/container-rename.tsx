import React from 'react';
import {
  Button, Column, Entry, Heading, Row, Spinner, Text,
  type ContainerSummary, type WorkspaceApi,
} from '@husklet/react';
import { boundedMessage, containerNameError, shortId } from './model.js';

const { useEffect, useState } = React;

type ContainerRenameProps = {
  api: WorkspaceApi;
  container: ContainerSummary;
  reload: () => void | Promise<void>;
  blocked: boolean;
};

type RenameResult =
  | { state: 'idle'; error: null; name: '' }
  | { state: 'loading'; error: null; name: string }
  | { state: 'success'; error: null; name: string }
  | { state: 'error'; error: unknown; name: string };

const idleResult = (): RenameResult => ({ state: 'idle', error: null, name: '' });

export function ContainerRename({ api, container, reload, blocked }: ContainerRenameProps) {
  const current = container.name ?? '';
  const [draft, setDraft] = useState(current);
  const [result, setResult] = useState<RenameResult>(idleResult);
  useEffect(() => {
    setDraft(current);
    setResult(idleResult());
  }, [container.id, current]);
  const validation = containerNameError(draft);
  const rename = async () => {
    if (validation || draft === current || result.state === 'loading') return;
    const requested = draft;
    const immutableId = container.id;
    setResult({ state: 'loading', error: null, name: requested });
    try {
      await api.containers.rename(immutableId, requested);
      setResult({ state: 'success', error: null, name: requested });
      await reload();
    } catch (error: unknown) {
      setResult({ state: 'error', error, name: requested });
    }
  };
  const changed = draft !== current;
  return (
    <Column gap={1}>
      <Heading label={'Rename container'} scale={'caption'} />
      <Text
        label={`Current name: ${current || '(unnamed)'}. Immutable ID: ${container.id}`}
        color={'text-dim'}
        wrap={true} />
      <Row gap={1} wrap={true} align={'center'}>
        <Entry
          value={draft}
          placeholder={`New name for ${shortId(container.id)}`}
          enabled={!blocked && result.state !== 'loading'}
          onChange={(event) => {
            setDraft(String(event.value ?? '').slice(0, 129));
            setResult(idleResult());
          }} />
        {result.state === 'loading' ? <Spinner /> : null}
        <Button
          label={result.state === 'loading' ? 'Renaming…' : result.state === 'error' ? 'Retry rename' : 'Rename'}
          enabled={!blocked && result.state !== 'loading' && changed && !validation}
          onInvoke={rename} />
      </Row>
      {changed && validation ? <Text label={validation} color={'danger'} wrap={true} /> : null}
      {result.state === 'error' ? <Text
        label={boundedMessage(result.error)}
        color={'danger'}
        wrap={true} /> : null}
      {result.state === 'success' ? <Text
        label={`Renamed to ${result.name}. Inventory identity will update after the authoritative refresh.`}
        color={'positive'}
        wrap={true} /> : null}
    </Column>
  );
}
