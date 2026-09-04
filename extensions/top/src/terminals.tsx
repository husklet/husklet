import React from 'react';
import {
  Badge, Button, Card, CardActions, CardContent, CardHeader, Column, ConfirmAction, Entry, Heading, LogView,
  ResourceState, Row, Scroll, Spinner, Text, LOG_VIEW_CHARACTER_LIMIT,
  type PaneText, type ReadablePane, type TabSummary, type WorkspaceApi,
} from '@husklet/react';
import { bounded, boundedMessage } from './model.js';
import type { Resource } from './overview.js';

type TerminalCursor = { generation: number; revision: number };

export function Terminals({ api, resource }: { api: WorkspaceApi; resource: Resource<TabSummary> }) {
  const [busy, setBusy] = React.useState('');
  const [error, setError] = React.useState<unknown>(null);
  const [selected, setSelected] = React.useState('');
  const [readable, setReadable] = React.useState<ReadablePane | null>(null);
  const [input, setInput] = React.useState('');
  const [title, setTitle] = React.useState('');
  const paneRevision = React.useRef(0);
  const view = bounded(resource.data ?? []);
  const state: 'loading' | 'error' | 'empty' | 'ready' = resource.loading
    ? 'loading' : resource.error ? 'error' : view.records.length === 0 ? 'empty' : 'ready';
  const pin = async (tab: TabSummary) => {
    setBusy(tab.id); setError(null);
    try {
      await api.terminal.pinTab(tab.id, !tab.pinned);
      await resource.reload();
    } catch (cause) { setError(cause); } finally { setBusy(''); }
  };
  const focus = async (tab: TabSummary) => {
    const slot = tab.panes[0]?.slot;
    if (!slot) return;
    setBusy(tab.id); setError(null);
    try { await api.terminal.focus(slot); } catch (cause) { setError(cause); } finally { setBusy(''); }
  };
  const inspect = async (slot: string) => {
    const requested = ++paneRevision.current;
    setBusy(`read:${slot}`); setError(null);
    try {
      const next = await api.terminal.toText(slot, { lines: 200 });
      if (requested !== paneRevision.current) return;
      setReadable(next); setSelected(slot);
    } catch (cause) {
      if (requested === paneRevision.current) setError(cause);
    } finally {
      if (requested === paneRevision.current) setBusy('');
    }
  };
  const cursor = readable ? paneCursor(readable.snapshot) : null;
  const sendLine = async () => {
    if (!cursor || readable?.kind !== 'terminal' || selected === '' || input === '') return;
    const requested = ++paneRevision.current;
    setBusy(`write:${selected}`); setError(null);
    try {
      const result = await api.terminal.writeAndWait(selected, cursor.generation, cursor.revision, `${input}\n`, { lines: 200 });
      if (requested !== paneRevision.current) return;
      if (result.changed) setReadable({ kind: 'terminal', text: result.after.lines.join('\n'), snapshot: result.after });
      setInput('');
    } catch (cause) {
      if (requested === paneRevision.current) setError(cause);
    } finally {
      if (requested === paneRevision.current) setBusy('');
    }
  };
  const mutatePane = async (operation: 'split-beside' | 'split-below' | 'retitle' | 'close') => {
    if (!cursor || !selected) return;
    const slot = selected;
    const requested = ++paneRevision.current;
    setBusy(`${operation}:${slot}`); setError(null);
    try {
      if (operation === 'close') {
        const result = await api.terminal.closeAndWait(slot, cursor.generation, cursor.revision);
        if (!result.changed) throw new Error(`Pane ${slot} did not close before the observation window ended; refresh and try again.`);
        if (requested !== paneRevision.current) return;
        setSelected(''); setReadable(null); setInput(''); setTitle('');
      } else if (operation === 'retitle') {
        const requestedTitle = title.trim();
        if (!requestedTitle) return;
        const result = await api.terminal.retitleAndWait(slot, cursor.generation, cursor.revision, requestedTitle);
        if (!result.changed) throw new Error(`Pane ${slot} did not acquire the requested title before the observation window ended; refresh and try again.`);
        if (requested !== paneRevision.current) return;
        setTitle('');
      } else {
        const division = operation === 'split-beside' ? 'beside' : 'below';
        const result = await api.terminal.splitAndWait(slot, cursor.generation, cursor.revision, division);
        if (!result.changed) throw new Error(`Pane ${slot} did not split before the observation window ended; refresh and try again.`);
        if (requested !== paneRevision.current) return;
      }
      await resource.reload();
    } catch (cause) {
      if (requested === paneRevision.current) setError(cause);
    } finally {
      if (requested === paneRevision.current) setBusy('');
    }
  };
  React.useEffect(() => {
    if (!selected) return;
    const present = (resource.data ?? []).some((tab) => tab.panes.some((pane) => pane.slot === selected));
    if (present) return;
    paneRevision.current += 1;
    setSelected(''); setReadable(null); setInput(''); setTitle('');
  }, [resource.data, selected]);
  return <Page title="Terminal tabs" subtitle="Read terminal output or semantic UI text, send revision-bound input, focus panes, and pin tabs.">
    <Toolbar loading={resource.loading} onRefresh={resource.reload} />
    <ErrorText error={error} />
    <ResourceState
      state={state}
      loadingLabel="Reading terminal tabs…"
      emptyLabel="No terminal tabs"
      emptyDetail="Open a terminal tab to manage it here."
      error={boundedMessage(resource.error)}
      retryLabel="Retry terminal tabs"
      onRetry={resource.reload}>
      {view.records.map((tab) => <Card key={tab.id} variant={tab.pinned ? 'filled' : 'outline'}>
        <CardHeader label={tab.title} detail={tab.id} />
        <CardContent gap={1}>
          <Row gap={1} align="center">
            <Badge label={tab.pinned ? 'Pinned' : 'Unpinned'} tone={tab.pinned ? 'positive' : 'neutral'} />
            <Text label={`${tab.panes.length} pane${tab.panes.length === 1 ? '' : 's'}`} color="text-dim" />
          </Row>
          {tab.panes.map((pane) => <Row key={pane.slot} gap={1} align="center">
            <Text label={`${pane.slot} · ${pane.occupant}${pane.provider ? ` · ${pane.provider.extension}/${pane.provider.provider}` : ''}`} color="text-dim" />
            <Button
              label={`${selected === pane.slot ? 'Refresh' : 'Inspect'} ${pane.slot}`}
              enabled={busy === ''}
              variant="ghost"
              onInvoke={() => { void inspect(pane.slot); }} />
          </Row>)}
          {selected && tab.panes.some((pane) => pane.slot === selected) && readable ? <Card variant="filled">
            <CardHeader
              label={readable.kind === 'terminal' ? `Terminal ${selected}` : `Interface ${selected}`}
              detail={readable.kind === 'terminal' ? 'Bounded live screen text' : 'Bounded semantic XML'} />
            <CardContent gap={1}>
              <LogView value={readable.text.slice(-LOG_VIEW_CHARACTER_LIMIT) || 'Pane is empty.'} />
              {readable.kind === 'terminal' && !cursor
                ? <Text label="This host did not provide a writable pane revision; refresh before sending input." color="warning" wrap /> : null}
              {readable.kind === 'terminal' ? <Row gap={1}>
                <Entry
                  value={input}
                  placeholder="Send a line to this terminal"
                  grow
                  enabled={Boolean(cursor)}
                  onChange={(event) => setInput(String(event.value ?? ''))}
                  onSubmit={() => { void sendLine(); }} />
                <Button label="Send line" enabled={busy === '' && input.length > 0 && Boolean(cursor)} onInvoke={() => { void sendLine(); }} />
              </Row> : null}
              <Row gap={1} wrap>
                <Button label="Split beside" enabled={busy === '' && Boolean(cursor)}
                  onInvoke={() => { void mutatePane('split-beside'); }} />
                <Button label="Split below" enabled={busy === '' && Boolean(cursor)}
                  onInvoke={() => { void mutatePane('split-below'); }} />
              </Row>
              <Row gap={1} wrap>
                <Entry value={title} placeholder="New pane title" grow enabled={busy === '' && Boolean(cursor)}
                  onChange={(event) => setTitle(String(event.value ?? ''))}
                  onSubmit={() => { void mutatePane('retitle'); }} />
                <Button label="Rename pane" enabled={busy === '' && Boolean(cursor) && title.trim().length > 0}
                  onInvoke={() => { void mutatePane('retitle'); }} />
              </Row>
              <ConfirmAction authorityKey={`pane:${selected}:${cursor?.generation ?? 'unknown'}:close`}
                label="Close pane" confirmLabel="Confirm close pane" pendingLabel="Confirm close pane"
                question={`Close immutable pane ${selected} at generation ${cursor?.generation ?? 'unknown'}?`}
                enabled={busy === '' && Boolean(cursor)} onConfirm={() => mutatePane('close')} />
            </CardContent>
          </Card> : null}
        </CardContent>
        <CardActions gap={1}>
          {busy === tab.id ? <Spinner /> : null}
          <Button label={`${tab.pinned ? 'Unpin' : 'Pin'} ${tab.title}`} enabled={busy === ''} onInvoke={() => { void pin(tab); }} />
          <Button label={`Focus ${tab.title}`} enabled={busy === '' && Boolean(tab.panes[0])} onInvoke={() => { void focus(tab); }} />
        </CardActions>
      </Card>)}
      <Omitted count={view.omitted} />
    </ResourceState>
  </Page>;
}

function paneCursor(snapshot: Pick<PaneText, 'generation' | 'revision'>): TerminalCursor | null {
  return typeof snapshot.generation === 'number' && Number.isSafeInteger(snapshot.generation)
    && typeof snapshot.revision === 'number' && Number.isSafeInteger(snapshot.revision)
    ? { generation: snapshot.generation, revision: snapshot.revision } : null;
}

function Page({ title, subtitle, children }: { title: string; subtitle: string; children: React.ReactNode }) {
  return <Scroll grow height="fill"><Column pad={4} gap={2}>
    <Heading label={title} scale="title" /><Text label={subtitle} color="text-dim" wrap />{children}
  </Column></Scroll>;
}
function Toolbar({ loading, onRefresh }: { loading: boolean; onRefresh: () => void | Promise<void> }) {
  return <Row gap={1} align="center">{loading ? <Spinner /> : null}<Button label="Refresh" enabled={!loading} onInvoke={onRefresh} /></Row>;
}
function ErrorText({ error }: { error: unknown }) { return error ? <Text label={boundedMessage(error)} color="danger" wrap /> : null; }
function Omitted({ count }: { count: number }) { return count > 0 ? <Text label={`${count} more records omitted to keep this view bounded.`} color="text-dim" /> : null; }
