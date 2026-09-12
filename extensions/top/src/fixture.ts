import type {
  ContainerSummary,
  ExecutionSummary,
  ImageSummary,
  NetworkSummary,
  TabSummary,
  VolumeSummary,
  WorkspaceApi,
} from '@husklet/react';

const containerId = 'a'.repeat(64);
const unavailableProcessContainerId = 'd'.repeat(64);

export const populatedFixture = {
  containers: [
    {
      id: containerId,
      name: 'api-worker',
      image: 'alpine:3.20',
      state: 'exited',
      created: 1_725_000_000,
      generation: 7,
    } satisfies ContainerSummary,
  ],
  executions: [
    {
      id: 'e'.repeat(32),
      container_id: containerId,
      running: false,
      exit_code: 0,
      result: { kind: 'code', value: 0 },
      created_at_ms: 1_000,
      started_at_ms: null,
      finished_at_ms: 1_100,
      pid: 412,
      command: ['/bin/sh', '-lc', 'npm test'],
      user: 'developer',
    } satisfies ExecutionSummary,
  ],
  images: [
    {
      id: `sha256:${'b'.repeat(64)}`,
      reference: 'alpine:3.20',
      references: ['alpine:3.20'],
      size: 8_192_000,
      created: 1_725_000_000,
    } satisfies ImageSummary,
  ],
  volumes: [
    { name: 'workspace-cache', driver: 'local', generation: 'fixture-7' } satisfies VolumeSummary,
  ],
  networks: [
    {
      id: 'c'.repeat(32),
      name: 'development',
      driver: 'bridge',
      scope: 'local',
      kind: 'custom',
      endpoints: { containers: [containerId], truncated: false },
    } satisfies NetworkSummary,
  ],
  terminals: [
    {
      id: 'daily',
      title: 'Daily work',
      pinned: true,
      panes: [
        {
          slot: 'shell',
          working_directory: '/workspace',
          command: '/bin/sh',
          occupant: 'terminal',
          provider: null,
        },
      ],
    } satisfies TabSummary,
  ],
};

export const partialProcessFixture = {
  ...populatedFixture,
  containers: [
    ...populatedFixture.containers,
    {
      id: unavailableProcessContainerId,
      name: 'job-worker',
      image: 'alpine:3.20',
      state: 'running',
      created: 1_725_000_100,
      generation: 4,
    } satisfies ContainerSummary,
  ],
};

/** Adds deterministic process data while all mutations still cross the real host connection. */
export function fixtureApi(api: WorkspaceApi, mode = 'populated'): WorkspaceApi {
  const unavailable = mode === 'error';
  const partialProcesses = mode === 'partial-processes';
  let catalogueAttempts = 0;
  return {
    ...api,
    inspect: unavailable
      ? async () => {
          throw new Error('Workspace settings could not be loaded from the extension host.');
        }
      : api.inspect,
    subscribe: async () => {},
    unsubscribe: async () => {},
    watchExecutions: async () => async () => {},
    networks: unavailable
      ? {
          ...api.networks,
          list: async () => {
            throw Object.assign(new Error('workspace daemon socket refused the connection'), {
              kind: 'unavailable',
            });
          },
        }
      : api.networks,
    containers: {
      ...api.containers,
      processes: async (id) => {
        if (partialProcesses && id === unavailableProcessContainerId) {
          throw new Error('container process endpoint did not respond');
        }
        return {
          container_id: containerId,
          titles: ['PID', 'USER', 'CPU', 'MEMORY', 'COMMAND'],
          processes: [['412', 'developer', '2.4', '64 MiB', 'npm test']],
          snapshot: 'f'.repeat(64),
          next: null,
          more: false,
          observed_at_ms: 1_725_000_000_000,
          scope: 'namespace',
          pid_identity: 'snapshot',
          truncated: false,
        };
      },
    },
    terminal: {
      ...api.terminal,
      toText: async (slot, options) =>
        !unavailable && slot === 'shell'
          ? {
              kind: 'terminal' as const,
              text: '$ npm test\n123 tests passed\n$',
              snapshot: {
                slot,
                generation: 7,
                revision: 11,
                columns: 100,
                rows: 30,
                lines: ['$ npm test', '123 tests passed', '$'],
                truncated: false,
              },
            }
          : api.terminal.toText(slot, options),
    },
    extensions: unavailable
      ? {
          ...api.extensions,
          catalogue: async () => {
            catalogueAttempts += 1;
            if (catalogueAttempts === 1) {
              throw new Error(
                'expected frame 9, received 12: extension catalogue transport closed',
              );
            }
            return api.extensions.catalogue();
          },
        }
      : api.extensions,
  };
}
