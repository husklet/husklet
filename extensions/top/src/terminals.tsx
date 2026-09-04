import React from 'react';
import {
  Badge, Button, Card, CardActions, CardContent, CardHeader, Column, ConfirmAction, Entry, Heading, LogView,
  ResourceState, Row, Scroll, Select, Spinner, Text, LOG_VIEW_CHARACTER_LIMIT,
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
  const [newTabTitle, setNewTabTitle] = React.useState('');
  const [command, setCommand] = React.useState('');
  const [columns, setColumns] = React.useState('');
  const [rows, setRows] = React.useState('');
  const [ratio, setRatio] = React.useState('50');
  const [providers, setProviders] = React.useState<{ extension: string; id: string; title: string }[]>([]);
  const [provider, setProvider] = React.useState('terminal');
  const [providerError, setProviderError] = React.useState<unknown>(null);
  const [providersTruncated, setProvidersTruncated] = React.useState(false);
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
  const openTab = async () => {
    const requestedTitle = newTabTitle.trim();
    if (!requestedTitle || busy) return;
    setBusy('open-tab'); setError(null);
    try {
      const result = await api.terminal.openTabAndWait(requestedTitle);
      if (!result.changed) throw new Error(`Tab ${result.tab} was created, but its initial pane was not observed; refresh before acting on it.`);
      setNewTabTitle('');
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
      const summary = (resource.data ?? []).flatMap((tab) => tab.panes).find((pane) => pane.slot === slot);
      setProvider(summary?.provider ? `${summary.provider.extension}/${summary.provider.provider}` : 'terminal');
      if (next.kind === 'terminal') {
        setColumns(next.snapshot.columns == null ? '' : String(next.snapshot.columns));
        setRows(next.snapshot.rows == null ? '' : String(next.snapshot.rows));
      } else { setColumns(''); setRows(''); }
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
  const spawnCommand = async () => {
    if (!cursor || readable?.kind !== 'terminal' || !selected || !command.trim()) return;
    const slot = selected;
    const requested = ++paneRevision.current;
    setBusy(`spawn:${slot}`); setError(null);
    try {
      const argv = commandArgv(command);
      const result = await api.terminal.spawnAndWait(slot, cursor.generation, cursor.revision, argv, { lines: 200 });
      if (!result.changed) throw new Error(`Pane ${slot} did not advance after spawning the command; refresh before retrying.`);
      if (requested !== paneRevision.current) return;
      setReadable({ kind: 'terminal', text: result.after.lines.join('\n'), snapshot: result.after });
      setCommand('');
    } catch (cause) {
      if (requested === paneRevision.current) setError(cause);
    } finally {
      if (requested === paneRevision.current) setBusy('');
    }
  };
  const resizeGrid = async () => {
    if (!cursor || readable?.kind !== 'terminal' || !selected) return;
    const nextColumns = gridDimension(columns, 'Columns');
    const nextRows = gridDimension(rows, 'Rows');
    const slot = selected;
    const requested = ++paneRevision.current;
    setBusy(`resize:${slot}`); setError(null);
    try {
      const result = await api.terminal.resizeGridAndWait(
        slot, cursor.generation, cursor.revision, nextColumns, nextRows, { lines: 200 },
      );
      if (!result.changed) throw new Error(`Pane ${slot} did not reach the requested ${nextColumns}×${nextRows} grid; refresh before retrying.`);
      if (requested !== paneRevision.current) return;
      setReadable({ kind: 'terminal', text: result.after.lines.join('\n'), snapshot: result.after });
    } catch (cause) {
      if (requested === paneRevision.current) setError(cause);
    } finally {
      if (requested === paneRevision.current) setBusy('');
    }
  };
  const resizeSplit = async () => {
    if (!cursor || !selected) return;
    if (!/^(?:[5-9]|[1-8][0-9]|9[0-5])$/.test(ratio)) {
      setError(new TypeError('Pane share must be an integer percentage from 5 to 95.'));
      return;
    }
    const slot = selected;
    const requested = ++paneRevision.current;
    const share = Number(ratio) / 100;
    setBusy(`ratio:${slot}`); setError(null);
    try {
      const result = await api.terminal.ratioAndWait(slot, cursor.generation, cursor.revision, share);
      if (!result.changed) throw new Error(`Pane ${slot} did not reach the requested ${ratio}% split share; refresh before retrying.`);
      const next = await api.terminal.toText(slot, { lines: 200 });
      if (requested !== paneRevision.current) return;
      setReadable(next);
      setRatio(String(Math.round(result.actual * 100)));
    } catch (cause) {
      if (requested === paneRevision.current) setError(cause);
    } finally {
      if (requested === paneRevision.current) setBusy('');
    }
  };
  const switchOccupant = async () => {
    if (!cursor || !selected) return;
    const target = provider === 'terminal' ? { kind: 'terminal' as const } : providerTarget(provider);
    const slot = selected;
    const requested = ++paneRevision.current;
    setBusy(`occupant:${slot}`); setError(null);
    try {
      const result = await api.terminal.switchOccupantAndWait(slot, cursor.generation, cursor.revision, target);
      if (!result.changed) throw new Error(`Pane ${slot} did not switch occupant before the observation window ended; refresh before retrying.`);
      const next = await api.terminal.toText(slot, { lines: 200 });
      if (requested !== paneRevision.current) return;
      setReadable(next);
      await resource.reload();
    } catch (cause) {
      if (requested === paneRevision.current) setError(cause);
    } finally {
      if (requested === paneRevision.current) setBusy('');
    }
  };
  React.useEffect(() => {
    if (!api.extensions?.providers) return;
    let disposed = false;
    void api.extensions.providers().then((catalogue) => {
      if (!disposed) {
        setProviders(catalogue.providers.map(({ extension, id, title }) => ({ extension, id, title })));
        setProvidersTruncated(catalogue.truncated);
      }
    }).catch((cause: unknown) => { if (!disposed) setProviderError(cause); });
    return () => { disposed = true; };
  }, [api]);
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
        setSelected(''); setReadable(null); setInput(''); setTitle(''); setCommand(''); setColumns(''); setRows('');
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
    setSelected(''); setReadable(null); setInput(''); setTitle(''); setCommand(''); setColumns(''); setRows('');
  }, [resource.data, selected]);
  return <Page title="Terminal tabs" subtitle="Read terminal output or semantic UI text, send revision-bound input, focus panes, and pin tabs.">
    <Row gap={1} wrap>
      <Entry value={newTabTitle} placeholder="New tab title" grow enabled={busy === ''}
        onChange={(event) => setNewTabTitle(String(event.value ?? ''))} onSubmit={() => { void openTab(); }} />
      <Button label={busy === 'open-tab' ? 'Opening…' : 'Open tab'}
        enabled={busy === '' && newTabTitle.trim().length > 0} onInvoke={() => { void openTab(); }} />
    </Row>
    <Toolbar loading={resource.loading} onRefresh={resource.reload} />
    <ErrorText error={error} />
    <ErrorText error={providerError} />
    {providersTruncated ? <Text label="The enabled pane-provider catalogue was truncated at its safety limit." color="warning" wrap /> : null}
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
              {readable.kind === 'terminal' ? <Row gap={1} wrap>
                <Entry value={command} placeholder={'Command argv, e.g. ["sh","-lc","make test"]'} grow
                  enabled={busy === '' && Boolean(cursor)} onChange={(event) => setCommand(String(event.value ?? ''))}
                  onSubmit={() => { void spawnCommand(); }} />
                <Button label="Spawn command" enabled={busy === '' && Boolean(cursor) && command.trim().length > 0}
                  onInvoke={() => { void spawnCommand(); }} />
              </Row> : null}
              {readable.kind === 'terminal' ? <Row gap={1} wrap>
                <Entry value={columns} placeholder="Columns (1–1000)" enabled={busy === '' && Boolean(cursor)}
                  onChange={(event) => setColumns(String(event.value ?? ''))} />
                <Entry value={rows} placeholder="Rows (1–1000)" enabled={busy === '' && Boolean(cursor)}
                  onChange={(event) => setRows(String(event.value ?? ''))} />
                <Button label="Resize grid" enabled={busy === '' && Boolean(cursor) && columns.length > 0 && rows.length > 0}
                  onInvoke={() => { void resizeGrid(); }} />
              </Row> : null}
              <Row gap={1} wrap>
                <Button label="Split beside" enabled={busy === '' && Boolean(cursor)}
                  onInvoke={() => { void mutatePane('split-beside'); }} />
                <Button label="Split below" enabled={busy === '' && Boolean(cursor)}
                  onInvoke={() => { void mutatePane('split-below'); }} />
                <Entry value={ratio} placeholder="Pane share % (5–95)" enabled={busy === '' && Boolean(cursor)}
                  onChange={(event) => setRatio(String(event.value ?? ''))} />
                <Button label="Set pane share" enabled={busy === '' && Boolean(cursor) && ratio.length > 0}
                  onInvoke={() => { void resizeSplit(); }} />
              </Row>
              <Row gap={1} wrap>
                <Select value={provider} choices={providerChoices(providers, provider)} enabled={busy === '' && Boolean(cursor)}
                onChange={(event) => setProvider(String(event.value ?? 'terminal'))} />
                <Button label="Switch pane content" enabled={busy === '' && Boolean(cursor)}
                  onInvoke={() => { void switchOccupant(); }} />
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

function commandArgv(value: string): string[] {
  let parsed: unknown;
  try { parsed = JSON.parse(value); } catch { throw new TypeError('Command must be a JSON array of argument strings.'); }
  const encoder = new TextEncoder();
  if (!Array.isArray(parsed) || parsed.length === 0 || parsed.length > 64
    || parsed.some((argument) => typeof argument !== 'string')
    || parsed[0].length === 0
    || parsed.some((argument) => argument.includes('\0') || encoder.encode(argument).length > 4_096)
    || parsed.reduce((total, argument) => total + encoder.encode(argument).length, 0) > 32_768) {
    throw new TypeError('Command must contain 1–64 NUL-free arguments, with a non-empty program, at most 4096 UTF-8 bytes each and 32768 bytes total.');
  }
  return parsed;
}

function gridDimension(value: string, label: string): number {
  if (!/^[1-9][0-9]{0,3}$/.test(value)) throw new TypeError(`${label} must be an integer from 1 to 1000.`);
  const dimension = Number(value);
  if (dimension > 1_000) throw new TypeError(`${label} must be an integer from 1 to 1000.`);
  return dimension;
}

function providerTarget(value: string): { kind: 'surface'; extension: string; provider: string } {
  const separator = value.indexOf('/');
  if (separator < 1 || separator === value.length - 1) throw new TypeError('Select an exact extension pane provider.');
  return { kind: 'surface', extension: value.slice(0, separator), provider: value.slice(separator + 1) };
}

function providerChoices(providers: { extension: string; id: string; title: string }[], current: string) {
  const choices = [
    { value: 'terminal', label: 'Terminal' },
    ...providers.map((item) => ({ value: `${item.extension}/${item.id}`, label: `${item.title} · ${item.extension}/${item.id}` })),
  ];
  if (current !== 'terminal' && !choices.some(({ value }) => value === current)) {
    choices.splice(1, 0, { value: current, label: `Current · ${current}` });
  }
  return choices;
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
