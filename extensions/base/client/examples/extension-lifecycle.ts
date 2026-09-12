import { connect, workspace, type ExtensionAcquisitionStatus } from '@husklet/client';
declare const process: { argv: string[]; stdout: { write(value: string): void } };

type Configuration = { path: string; reference: string; retryReference: string };
const configuration = JSON.parse(process.argv[2] ?? 'null') as Configuration | null;
if (!configuration?.path || !configuration.reference || !configuration.retryReference)
  throw new TypeError('usage: extension-lifecycle.ts JSON(path, reference, retryReference)');

const ready = async (host: ReturnType<typeof workspace>, job: string) => {
  let status: ExtensionAcquisitionStatus = await host.extensions.acquisition(job);
  while (status.state !== 'ready') {
    if (status.state === 'failed' || status.state === 'cancelled')
      throw new Error(status.error ?? `acquisition ${status.state}`);
    const next = await host.extensions.waitForAcquisition(job, status.revision);
    if (!next.changed) throw new Error('acquisition made no progress');
    status = next.status;
  }
  if (!status.candidate) throw new Error('ready acquisition omitted its candidate');
  return status;
};

let session = await connect({ path: configuration.path, pendingLimit: 8, timeout: 5_000 });
let host = workspace(session);
const catalogue = await host.extensions.catalogue();
const cancelled = await host.extensions.startAcquisition(configuration.reference);
const cancellation = await host.extensions.acquisition(cancelled.job);
const cancellationResult = await host.extensions.cancelAcquisitionAndWait(
  cancelled.job,
  cancellation.revision,
);
if (!cancellationResult.changed) throw new Error('acquisition cancellation was not observed');

// Retrying is deliberately a new immutable acquisition job: the old cancelled
// job remains inspectable and cannot be confused with later progress.
const acquisition = await host.extensions.startAcquisition(configuration.retryReference);
const candidate = await ready(host, acquisition.job);
const installed = await host.extensions.installAndWait(
  candidate.job,
  candidate.revision,
  {
    capabilities: candidate.candidate!.requested,
    containers: { selectors: [{ name: 'postgres' }], create: false },
    filesystem: {
      read: [{ subtree: 'data' }],
      write: [],
      create: [],
      delete: [],
      rename: [],
    },
  },
);
if (!installed.changed) throw new Error('install was not observed');
const identity = installed.extension;
await host.extensions.disableAndWait(identity.name, identity.image_digest);
await host.extensions.retryAndWait(identity.name, identity.image_digest);

await session.close();
session = await connect({ path: configuration.path, pendingLimit: 8, timeout: 5_000 });
host = workspace(session);
const persisted = await host.extensions.inspect(identity.name);
if (persisted.image_digest !== identity.image_digest || !persisted.enabled)
  throw new Error('installed lifecycle state did not survive host restart');
await host.extensions.removeAndWait(identity.name, identity.image_digest);
await session.close();
process.stdout.write(`${JSON.stringify({ catalogue: catalogue.entries.length, removed: identity.name })}\n`);
