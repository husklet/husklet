import type { WorkspaceApi } from '@husklet/client';

export type TestRunEvent =
  | { kind: 'started'; executionId: string }
  | { kind: 'stdout' | 'stderr'; text: string }
  | { kind: 'finished'; executionId: string; exitCode: number | null };

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
  let active: AbortController | undefined;
  const launch = async () => {
    active?.abort('superseded by a newer source revision');
    const controller = new AbortController();
    active = controller;
    const stop = () => controller.abort(options.signal.reason);
    options.signal.addEventListener('abort', stop, { once: true });
    try {
      const result = await host.containers.execLines(
        container.id,
        container.generation,
        {
          command: options.command,
          maxLineBytes: 256 * 1024,
          pageLimit: 8,
          signal: controller.signal,
          onStarted: (executionId) => options.report({ kind: 'started', executionId }),
          onStderr: (text) => options.report({ kind: 'stderr', text }),
        },
        (text) => options.report({ kind: 'stdout', text }),
      );
      await options.report({
        kind: 'finished',
        executionId: result.executionId,
        exitCode: result.execution.exit_code,
      });
    } finally {
      options.signal.removeEventListener('abort', stop);
      if (active === controller) active = undefined;
    }
  };

  let current = launch();
  void current.catch(() => {});
  const stop = await host.files.watchChanges(
    async (page) => {
      if (page.truncated)
        throw new Error('filesystem history is incomplete; rescan before testing');
      if (page.changes.length > 0) {
        active?.abort('superseded by a newer source revision');
        await current.catch(() => {});
        current = launch();
        await current;
      }
    },
    { cursor: options.cursor, signal: options.signal },
  );
  return async () => {
    active?.abort('test watcher stopped');
    await current.catch(() => {});
    await stop();
  };
}
