import { useEffect, useRef, useState } from 'react';
import type { HostEvent, PaneSelection, Session } from '@husklet/client';

/**
 * Observes host events for the lifetime of a component.
 *
 * Re-rendering updates the callback without briefly dropping the subscription;
 * changing sessions or unmounting disposes the old observer exactly once.
 */
export function useHostEvents(
  session: Session,
  listener: (event: HostEvent, channel: number) => void,
): void {
  if (!session || typeof session.onEvent !== 'function')
    throw new TypeError('useHostEvents needs a Session');
  if (typeof listener !== 'function') throw new TypeError('useHostEvents needs an event listener');
  const current = useRef(listener);
  current.current = listener;
  useEffect(() => {
    const unsubscribe = session.onEvent((event, channel) => current.current(event, channel));
    return () => {
      unsubscribe();
    };
  }, [session]);
}

/** The latest pane-chooser selection, optionally restricted to one provider. */
export function usePaneSelection(
  session: Session,
  provider: string | null = null,
): PaneSelection | null {
  const [selection, setSelection] = useState<PaneSelection | null>(null);
  useEffect(() => setSelection(null), [session, provider]);
  useHostEvents(session, (event) => {
    if (!event || typeof event !== 'object' || !('pane_provider' in event)) return;
    if (typeof event.pane_provider !== 'string' || typeof event.slot !== 'string') return;
    if (provider === null || event.pane_provider === provider) setSelection(event);
  });
  return selection;
}
