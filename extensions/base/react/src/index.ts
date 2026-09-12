// The public API: connect to the host, render React into its tab.

import { Session } from '@husklet/client';
import type {
  ConnectOptions,
  Frame,
  DataRow,
  RowRequest,
  SourceMutation,
  SurfaceBootstrap,
} from '@husklet/client';
import type { ReactNode } from 'react';
import { Surface, reconciler } from './reconciler.js';
import { PROPS, TRIGGERS, sourceMutation } from './protocol.js';
export { TABLE_COLUMN_LIMIT, COLUMN_KEY_BYTE_LIMIT, COLUMN_TITLE_BYTE_LIMIT } from './protocol.js';

// Match the declaration surface below: React extensions get the complete
// framework-neutral SDK, while this module's explicit `connect` export remains
// the React-aware override.
export * from '@husklet/client';
// Explicit component exports resolve names also used by client model types.
export { Row, Column, Entry } from './components.js';
export * from './components.js';
export * from './hooks.js';
export * from './terminal-transcript.js';
export * from './command-palette.js';
export * from './json-tree.js';
export * from './confirm-action.js';
export * from './resource-state.js';
export * from './recovery-state.js';
export * from './window-cache.js';
export * from './composite-catalogue.js';

type RenderFrame = Parameters<ConstructorParameters<typeof Surface>[0]>[0];
export interface RowProviderContext {
  /** Aborted when the window is superseded or its surface closes. */
  signal: AbortSignal;
}
export type RowProvider = (
  request: RowRequest,
  context: RowProviderContext,
) => readonly DataRow[] | Promise<readonly DataRow[]>;
interface RenderOptions {
  title?: string;
  split?: { slot: string; division: 'beside' | 'below' } | null;
  bootstrap?: SurfaceBootstrap | null;
  /** Supplies only the bounded table windows this surface asks for. */
  rows?: RowProvider | null;
}
interface RenderHandle {
  surface: Surface;
  ready: Promise<string>;
  readonly slot: string | null;
  update(next: ReactNode): void;
  flush(): Promise<void>;
  source(mutation: SourceMutation): Promise<void>;
  /** Replaces the window producer and cancels work owned by its predecessor. */
  setRowProvider(provider: RowProvider | null): void;
  close(): Promise<void>;
  rowProvider: RenderOptions['rows'];
  rowActive: Set<RowTask>;
  rowQueue: RowPending[];
  /** Highest host revision observed for each source on this surface. */
  rowVersions: Map<number, number>;
  /** Exact source revisions successfully published by this extension. */
  publishedRowVersions: Map<number, number>;
}
interface RowTask {
  request: RowRequest;
  channel: number;
  controller: AbortController;
  answered: boolean;
}
interface RowPending {
  request: RowRequest;
  channel: number;
}
interface Registry {
  handles: Set<RenderHandle>;
  slots: Map<string, RenderHandle>;
  retiredSlots: Set<string>;
  routesEvents: boolean;
}
const attached = new WeakMap<Session, Registry>();
const SURFACE_LIMIT = 32;
const FRAME_BUFFER_LIMIT = 64;
/** Maximum database/file window producers allowed to run concurrently per surface. */
export const ROW_PROVIDER_CONCURRENCY = 4;
/** Maximum pending windows retained while a producer is saturated. */
export const ROW_PROVIDER_QUEUE_LIMIT = 32;

export async function connect({
  path,
  onRows,
  onReply,
  onEvent,
  onEventError,
  onClose,
  pendingLimit,
  timeout,
  connectTimeout,
}: ConnectOptions = {}) {
  const connected: { session?: Session } = {};
  const pendingRows: Array<{ request: RowRequest; channel: number }> = [];
  const session = await Session.connect(path, {
    onRows: (request, channel) => {
      if (connected.session === undefined) {
        pendingRows.push({ request, channel });
        return;
      }
      if (!deliverRows(connected.session, request, channel, onEventError) && onRows)
        onRows(request, channel);
    },
    pendingLimit,
    timeout,
    connectTimeout,
    onEventError,
    onClose,
    onEvent: (payload, channel) => {
      if (connected.session !== undefined) deliver(connected.session, payload);
      if (onEvent) onEvent(payload, channel);
    },
    onReply: (payload) => {
      if ((connected.session === undefined || !deliver(connected.session, payload)) && onReply)
        onReply(payload);
    },
  });
  connected.session = session;
  attached.set(session, {
    handles: new Set(),
    slots: new Map(),
    retiredSlots: new Set(),
    routesEvents: true,
  });
  for (const { request, channel } of pendingRows) {
    if (!deliverRows(session, request, channel, onEventError) && onRows) onRows(request, channel);
  }
  return session;
}

function deliverRows(
  session: Session,
  request: RowRequest,
  channel: number,
  onError: ConnectOptions['onEventError'],
): boolean {
  const registry = attached.get(session);
  const handle = request.slot === undefined ? undefined : registry?.slots.get(request.slot);
  const provider = handle?.rowProvider;
  if (!registry || !handle || !provider) return false;
  const publishedVersion = handle.publishedRowVersions.get(request.source);
  if (publishedVersion !== undefined && request.version !== publishedVersion) {
    answerRows(session, { request, channel }, []);
    return true;
  }
  const latestVersion = handle.rowVersions.get(request.source);
  if (latestVersion !== undefined && request.version < latestVersion) {
    answerRows(session, { request, channel }, []);
    return true;
  }
  if (latestVersion === undefined || request.version > latestVersion) {
    handle.rowVersions.set(request.source, request.version);
    const retained = [];
    for (const pending of handle.rowQueue) {
      if (pending.request.source === request.source && pending.request.version < request.version)
        answerRows(session, pending, []);
      else retained.push(pending);
    }
    handle.rowQueue = retained;
    for (const task of handle.rowActive) {
      if (task.request.source === request.source && task.request.version < request.version) {
        task.controller.abort(new Error('row source version was superseded'));
        answerRows(session, task, []);
      }
    }
  }
  const sameWindow = (candidate: RowRequest) =>
    candidate.source === request.source &&
    candidate.version === request.version &&
    candidate.range.start === request.range.start &&
    candidate.range.count === request.range.count;
  const retained = [];
  for (const pending of handle.rowQueue) {
    if (sameWindow(pending.request)) answerRows(session, pending, []);
    else retained.push(pending);
  }
  handle.rowQueue = retained;
  for (const task of handle.rowActive) {
    if (sameWindow(task.request)) {
      task.controller.abort(new Error('row window was superseded'));
      answerRows(session, task, []);
    }
  }
  handle.rowQueue.push({ request, channel });
  if (handle.rowQueue.length > ROW_PROVIDER_QUEUE_LIMIT) {
    const dropped = handle.rowQueue.shift();
    if (dropped) answerRows(session, dropped, []);
  }
  drainRows(session, registry, handle, onError);
  return true;
}

function answerRows(session: Session, pending: RowPending | RowTask, rows: readonly DataRow[]) {
  if ('answered' in pending) {
    if (pending.answered) return;
    pending.answered = true;
  }
  session.answer(pending.channel, {
    source: pending.request.source,
    version: pending.request.version,
    request: pending.request.id,
    range: pending.request.range,
    rows: [...rows],
  });
}

function publishRowVersion(
  session: Session,
  handle: RenderHandle,
  source: number,
  version: number,
) {
  handle.publishedRowVersions.set(source, version);
  handle.rowVersions.set(source, version);
  const retained = [];
  for (const pending of handle.rowQueue) {
    if (pending.request.source === source && pending.request.version !== version)
      answerRows(session, pending, []);
    else retained.push(pending);
  }
  handle.rowQueue = retained;
  for (const task of handle.rowActive) {
    if (task.request.source === source && task.request.version !== version) {
      task.controller.abort(new Error('row source version was superseded'));
      answerRows(session, task, []);
      handle.rowActive.delete(task);
    }
  }
}

function retireRowSource(session: Session, handle: RenderHandle, source: number) {
  handle.publishedRowVersions.delete(source);
  handle.rowVersions.delete(source);
  const retained = [];
  for (const pending of handle.rowQueue) {
    if (pending.request.source === source) answerRows(session, pending, []);
    else retained.push(pending);
  }
  handle.rowQueue = retained;
  for (const task of handle.rowActive) {
    if (task.request.source === source) {
      task.controller.abort(new Error('row source was retired'));
      answerRows(session, task, []);
      handle.rowActive.delete(task);
    }
  }
}

function drainRows(
  session: Session,
  registry: Registry,
  handle: RenderHandle,
  onError: ConnectOptions['onEventError'],
) {
  while (handle.rowActive.size < ROW_PROVIDER_CONCURRENCY && handle.rowQueue.length > 0) {
    const pending = handle.rowQueue.shift();
    if (!pending || !handle.rowProvider) return;
    const { request, channel } = pending;
    const controller = new AbortController();
    const task = { request, channel, controller, answered: false };
    handle.rowActive.add(task);
    void (async () => {
      const rows = await handle.rowProvider?.(request, { signal: controller.signal });
      if (controller.signal.aborted || rows === undefined) return;
      if (registry.slots.get(request.slot ?? '') !== handle || !registry.handles.has(handle))
        return;
      if (!Array.isArray(rows)) throw new TypeError('surface row provider must return an array');
      if (rows.length > request.range.count) {
        throw new RangeError('surface row provider returned more rows than the requested window');
      }
      answerRows(session, task, rows);
    })()
      .catch((error) => {
        if (!controller.signal.aborted) {
          try {
            answerRows(session, task, []);
          } catch (answerError) {
            onError?.(answerError);
          }
          onError?.(error);
        }
      })
      .finally(() => {
        handle.rowActive.delete(task);
        drainRows(session, registry, handle, onError);
      });
  }
}

/**
 * Renders an element tree into the extension's tab.
 *
 * The tab is opened first because the host refuses a render before one exists.
 * Returns a handle whose `update` re-renders and whose `close` tears down.
 */
export function render(
  element: ReactNode,
  session: Session,
  { title = 'Extension', split = null, bootstrap = null, rows = null }: RenderOptions = {},
): RenderHandle {
  let registry = attached.get(session);
  if (!registry && bootstrap !== null) {
    registry = {
      handles: new Set(),
      slots: new Map(),
      retiredSlots: new Set(),
      routesEvents: false,
    };
    attached.set(session, registry);
  }
  if (!registry) throw new Error('render requires a session returned by connect');
  if (!registry.routesEvents) {
    session.onEvent((payload) => {
      deliver(session, payload);
    });
    registry.routesEvents = true;
  }
  if (
    split !== null &&
    (typeof split !== 'object' ||
      typeof split.slot !== 'string' ||
      !['beside', 'below'].includes(split.division))
  ) {
    throw new TypeError('split requires a slot and a beside or below division');
  }
  if (registry.handles.size >= SURFACE_LIMIT) {
    throw new RangeError(`extension surface limit of ${SURFACE_LIMIT} is exhausted`);
  }
  const queued: RenderFrame[] = [];
  let slot = bootstrap?.slot ?? null;
  let closed = false;
  let failed: unknown = null;
  let withdrawal: Promise<void> | null = null;
  let delivery = Promise.resolve();
  const transmit = (frame: RenderFrame) => {
    if (closed || failed) return;
    if (slot === null) {
      if (queued.length >= FRAME_BUFFER_LIMIT) {
        failed = new Error(`surface frame buffer limit of ${FRAME_BUFFER_LIMIT} is exhausted`);
        return;
      }
      queued.push(frame);
      return;
    }
    const target = slot;
    delivery = delivery.then(async () => {
      if (closed || failed) return;
      try {
        const reply = await session.call('interface_render_at', {
          slot: target,
          frame: frame as Frame,
        });
        if (reply?.reply !== 'done') {
          throw new Error(`host replied ${reply?.reply ?? 'without a tag'}, expected done`);
        }
      } catch (error) {
        failed = error;
      }
    });
  };
  if (
    bootstrap !== null &&
    (typeof bootstrap !== 'object' ||
      typeof bootstrap.slot !== 'string' ||
      bootstrap.sequence !== 1 ||
      bootstrap.nextNode !== 2 ||
      bootstrap.bootstrapNode !== 1)
  )
    throw new TypeError('bootstrap must be a token returned by bootstrapSurface');
  const surface = new Surface(
    transmit,
    bootstrap === null
      ? undefined
      : {
          sequence: bootstrap.sequence,
          next: bootstrap.nextNode,
          patches: [{ Remove: { id: bootstrap.bootstrapNode } }],
        },
  );
  const handle = {
    surface,
    rowProvider: rows,
    rowActive: new Set<RowTask>(),
    rowQueue: [],
    rowVersions: new Map<number, number>(),
    publishedRowVersions: new Map<number, number>(),
  } as unknown as RenderHandle;
  registry.handles.add(handle);

  const opening =
    bootstrap !== null
      ? Promise.resolve({ reply: 'identity', with: bootstrap.slot })
      : split === null
        ? session.call('interface_open_tab', { title })
        : session.call('interface_split', { slot: split.slot, division: split.division });
  const ready = opening
    .then((reply) => {
      if (
        reply?.reply !== 'identity' ||
        typeof reply.with !== 'string' ||
        (bootstrap === null && reply.with.length === 0)
      ) {
        throw new Error(`host replied ${reply?.reply ?? 'without a tag'}, expected identity`);
      }
      if (closed) return reply.with;
      if (registry.slots.has(reply.with) || registry.retiredSlots.has(reply.with))
        throw new Error(`host reused surface slot ${reply.with} within one session`);
      slot = reply.with;
      registry.slots.set(slot, handle);
      for (const frame of queued.splice(0)) transmit(frame);
      if (failed) throw failed;
      return slot;
    })
    .catch((error: unknown) => {
      failed = error;
      registry.handles.delete(handle);
      if (slot !== null) {
        registry.slots.delete(slot);
        registry.retiredSlots.add(slot);
      }
      throw error;
    });
  // Existing fire-and-forget callers still get bounded cleanup on a refused
  // open; callers that need diagnostics await the same promise on the handle.
  void ready.catch(() => {});

  const container = reconciler.createContainer(surface, 0, null, false, null, '', () => {}, null);
  reconciler.updateContainer(element, container, null, null);
  Object.assign(handle, {
    ready,
    update(next: ReactNode) {
      reconciler.updateContainer(next, container, null, null);
    },
    async flush() {
      await ready;
      for (;;) {
        const current = delivery;
        await current;
        if (current === delivery) break;
      }
      if (failed) throw failed;
    },
    async source(mutation: SourceMutation) {
      const owned = await ready;
      const normalized = sourceMutation(mutation) as SourceMutation;
      const reply = await session.call('source_resize_at', {
        slot: owned,
        mutation: normalized,
      });
      if (reply?.reply !== 'done')
        throw new Error(`host replied ${reply?.reply ?? 'without a tag'}, expected done`);
      if ('Open' in normalized) {
        retireRowSource(session, handle, normalized.Open.source);
      } else if ('Close' in normalized) {
        retireRowSource(session, handle, normalized.Close.source);
      } else if ('Length' in normalized) {
        publishRowVersion(session, handle, normalized.Length.source, normalized.Length.version);
      } else if ('Invalidate' in normalized) {
        publishRowVersion(
          session,
          handle,
          normalized.Invalidate.source,
          normalized.Invalidate.version,
        );
      } else if ('Window' in normalized) {
        publishRowVersion(session, handle, normalized.Window.source, normalized.Window.version);
      }
    },
    setRowProvider(provider: RowProvider | null) {
      for (const pending of handle.rowQueue) answerRows(session, pending, []);
      handle.rowQueue.length = 0;
      handle.rowVersions.clear();
      for (const task of handle.rowActive) {
        task.controller.abort(new Error('row provider was replaced'));
        answerRows(session, task, []);
      }
      handle.rowActive.clear();
      handle.rowProvider = provider;
    },
    close() {
      if (closed) return withdrawal ?? Promise.resolve();
      closed = true;
      for (const pending of handle.rowQueue) answerRows(session, pending, []);
      handle.rowQueue.length = 0;
      handle.rowVersions.clear();
      handle.publishedRowVersions.clear();
      for (const task of handle.rowActive) {
        task.controller.abort(new Error('surface closed'));
        answerRows(session, task, []);
      }
      handle.rowActive.clear();
      reconciler.updateContainer(null, container, null, null);
      registry.handles.delete(handle);
      if (slot !== null) {
        registry.slots.delete(slot);
        registry.retiredSlots.add(slot);
      }
      withdrawal = ready.then(async (owned) => {
        if (owned === '') return;
        const reply = await session.call('interface_withdraw', { slot: owned });
        if (reply?.reply !== 'done') {
          throw new Error(`host replied ${reply?.reply ?? 'without a tag'}, expected done`);
        }
      });
      // Fire-and-forget callers retain the old ergonomics without creating an
      // unhandled rejection; callers that care await the returned promise.
      void withdrawal.catch(() => {});
      return withdrawal;
    },
  });
  Object.defineProperty(handle, 'slot', { enumerable: true, get: () => slot });
  return handle;
}

/**
 * Routes one host payload to the callback that asked for it.
 *
 * Returns whether it was an interface event at all, so anything else can go on
 * to the caller's own reply handler.
 */
export function deliver(session: Session, payload: unknown): boolean {
  const event = interpret(payload);
  if (event === null) return false;
  const registry = attached.get(session);
  if (!registry) return false;
  const slot =
    payload !== null &&
    typeof payload === 'object' &&
    'slot' in payload &&
    typeof payload.slot === 'string'
      ? payload.slot
      : null;
  if (slot !== null) {
    return registry.slots.get(slot)?.surface.dispatch(event) ?? false;
  }
  if (registry.handles.size !== 1) return false;
  let delivered = false;
  for (const handle of registry.handles) {
    delivered = handle.surface.dispatch(event) || delivered;
  }
  return delivered;
}

/**
 * Reads an interface event out of whatever the host pushed.
 *
 * TODO: narrow to the single spelling once the host side of `hl_gui::Event`
 * gains its wire derive; today only the identity and the trigger are certain.
 */
function interpret(
  payload: unknown,
): ({ id: string; [key: string]: unknown } & { value: unknown }) | null {
  if (!payload || typeof payload !== 'object') return null;
  if (
    !('interaction' in payload) ||
    typeof payload.interaction !== 'string' ||
    !('trigger' in payload) ||
    typeof payload.trigger !== 'string' ||
    !('id' in payload) ||
    typeof payload.id !== 'string'
  )
    return null;
  const wire = 'value' in payload ? payload.value : undefined;
  const value =
    wire && typeof wire === 'object'
      ? 'Text' in wire
        ? wire.Text
        : 'Number' in wire
          ? wire.Number
          : 'Integer' in wire
            ? wire.Integer
            : 'Flag' in wire
              ? wire.Flag
              : wire
      : (wire ?? null);
  return { ...payload, id: payload.id, value };
}

/** Every prop and handler name a component accepts, for tooling and tests. */
export const vocabulary = {
  props: [...PROPS.keys()],
  handlers: [...TRIGGERS.keys()],
};

/** Maximum Unicode characters retained by a LogView; Value patches append. */
export const LOG_VIEW_CHARACTER_LIMIT = 4_096;
