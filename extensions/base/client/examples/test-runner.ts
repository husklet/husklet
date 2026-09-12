import type { WorkspaceApi } from '@husklet/client';

export type TestRunEvent =
  | { kind: 'started'; executionId: string }
  | { kind: 'stdout' | 'stderr'; text: string }
  | { kind: 'finished'; executionId: string; exitCode: number | null }
  | { kind: 'failed'; executionId: string | null; message: string };

/**
 * Watch a source tree and keep exactly one test execution current. Slow output consumers apply
 * backpressure; a newer filesystem revision aborts and host-cancels the stale execution.
 */
export async function watchTests(
  host: WorkspaceApi,
  container: { id: string; generation: number },
  options: {
    cursor: { journal: string; revision: number };
    command: string[];
    signal: AbortSignal;
    report(event: TestRunEvent): void | Promise<void>;
  },
) {
  const launch = async (signal: AbortSignal) => {
    let executionId: string | null = null;
    try {
      const result = await host.containers.execLines(
        container.id,
        container.generation,
        {
          command: options.command,
          maxLineBytes: 256 * 1024,
          pageLimit: 8,
          signal,
          onStarted: (started) => {
            executionId = started;
            return options.report({ kind: 'started', executionId: started });
          },
          onStderr: (text) => options.report({ kind: 'stderr', text }),
        },
        (text) => options.report({ kind: 'stdout', text }),
      );
      await options.report({
        kind: 'finished',
        executionId: result.executionId,
        exitCode: result.execution.exit_code,
      });
    } catch (cause) {
      if (!signal.aborted) {
        await options.report({
          kind: 'failed',
          executionId,
          message: cause instanceof Error ? cause.message : String(cause),
        });
      }
    }
  };

  const initial = new AbortController();
  const abortInitial = () => initial.abort(options.signal.reason);
  if (options.signal.aborted) abortInitial();
  else options.signal.addEventListener('abort', abortInitial, { once: true });
  const first = launch(initial.signal);
  void first.catch(() => {});
  let initialActive = true;
  const stop = await host.files.watchLatestChanges(
    async (page, signal) => {
      if (page.truncated)
        throw new Error('filesystem history is incomplete; rescan before testing');
      if (page.changes.length > 0) {
        if (initialActive) {
          initialActive = false;
          initial.abort('superseded by a newer source revision');
          await first.catch(() => {});
        }
        await launch(signal);
      }
    },
    { cursor: options.cursor, signal: options.signal },
  );
  return async () => {
    initial.abort('test watcher stopped');
    try {
      await stop();
    } finally {
      options.signal.removeEventListener('abort', abortInitial);
      await first.catch(() => {});
    }
  };
}
