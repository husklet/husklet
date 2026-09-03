import { connect, workspace } from '@husklet/client';

let session;
let stopping = false;
let connected = false;
const report = (kind, error) => process.stderr.write(`client-starter: ${kind}: ${error instanceof Error ? error.message : String(error)}\n`);
const stop = () => {
  if (stopping) return;
  stopping = true;
  session?.close();
};

try {
  session = await connect({
    onClose: (error) => {
      if (!connected || stopping) return;
      stopping = true;
      process.exitCode = 1;
      report('host connection ended', error);
    },
  });
  connected = true;
  process.once('SIGINT', stop);
  process.once('SIGTERM', stop);
  const information = await workspace(session).info();
  process.stdout.write(`${JSON.stringify(information)}\n`);
} catch (error) {
  if (!stopping) {
    stopping = true;
    process.exitCode = 1;
    report('startup failed', error);
  }
  session?.close();
}
