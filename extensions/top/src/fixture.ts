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
      id: 'e'.repeat(64),
      container_id: containerId,
      running: false,
      exit_code: 0,
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
      id: 'c'.repeat(64),
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

/** Adds deterministic process data while all mutations still cross the real host connection. */
export function fixtureApi(api: WorkspaceApi): WorkspaceApi {
  return {
    ...api,
    subscribe: async () => {},
    unsubscribe: async () => {},
    watchExecutions: async () => async () => {},
    containers: {
      ...api.containers,
      processes: async () => ({
        container_id: containerId,
        titles: ['PID', 'USER', 'COMMAND'],
        processes: [['412', 'developer', 'npm test']],
        observed_at_ms: 1_725_000_000_000,
        scope: 'namespace',
        pid_identity: 'snapshot',
        truncated: false,
      }),
    },
  };
}
