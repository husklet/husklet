import React from 'react';
import { Badge, Code, Column, Heading, Text, connect, render, workspace } from '@husklet/react';

declare const process: { argv: string[]; stdout: { write(value: string): void } };
type Configuration = {
  path: string;
  container: string;
  generation: number;
  source: string;
  helper: string[];
};
const configuration = JSON.parse(process.argv[2] ?? 'null') as Configuration;
const session = await connect({ path: configuration.path, pendingLimit: 8 });
try {
  const host = workspace(session);
  const source = await host.files.stat(configuration.source);
  if (!source.identity) throw new Error('language source has no stable identity');
  const document = await host.files.readText(configuration.source, {
    observed: source.identity,
    maxBytes: 1024 * 1024,
  });
  const request = JSON.stringify({
    id: 1,
    method: 'document/open',
    params: { path: configuration.source, identity: document.identity, text: document.text },
  });
  async function* messages() {
    yield `Content-Length: ${new TextEncoder().encode(request).byteLength}\r\n\r\n${request}`;
    // A real editor keeps this source open for later edits. Helper EOF must release this pending
    // iterator rather than leaving the extension lifecycle stuck.
    await new Promise(() => {});
  }

  let buffered: number[] = [];
  const diagnostics: string[] = [];
  const result = await host.containers.execStreaming(
    configuration.container,
    configuration.generation,
    { command: configuration.helper, input: messages(), pageLimit: 4 },
    async (page) => {
      for (const entry of page.entries) {
        if (entry.stream !== 'stdout') continue;
        buffered.push(...entry.bytes);
        if (buffered.length > 256 * 1024)
          throw new RangeError('language helper response exceeds 256 KiB');
        for (;;) {
          const boundary = buffered.findIndex(
            (byte, index) =>
              byte === 13 &&
              buffered[index + 1] === 10 &&
              buffered[index + 2] === 13 &&
              buffered[index + 3] === 10,
          );
          if (boundary < 0) break;
          const header = new TextDecoder('ascii', { fatal: true }).decode(
            Uint8Array.from(buffered.slice(0, boundary)),
          );
          const match = /^Content-Length: (\d+)$/im.exec(header);
          if (!match) throw new Error('language helper returned malformed framing');
          const length = Number(match[1]);
          if (!Number.isSafeInteger(length) || length > 256 * 1024)
            throw new RangeError('language helper message is not bounded');
          const start = boundary + 4;
          if (buffered.length - start < length) break;
          diagnostics.push(
            new TextDecoder('utf-8', { fatal: true }).decode(
              Uint8Array.from(buffered.slice(start, start + length)),
            ),
          );
          buffered = buffered.slice(start + length);
        }
      }
    },
  );
  if (buffered.length > 0) throw new Error('language helper ended with a partial message');
  const surface = render(
    React.createElement(
      Column,
      { grow: true, gap: 2, pad: 3 },
      React.createElement(Heading, { label: 'Language diagnostics', scale: 'title' }),
      React.createElement(Badge, {
        label: result.execution.exit_code === 0 ? 'Analysis complete' : 'Helper failed',
        tone: result.execution.exit_code === 0 ? 'positive' : 'danger',
      }),
      React.createElement(Text, { label: configuration.source }),
      React.createElement(Code, {
        label: diagnostics.join('\n').slice(0, 4096) || 'No diagnostics',
      }),
    ),
    session,
    { title: 'Language' },
  );
  await surface.ready;
  await surface.flush();
  process.stdout.write(
    JSON.stringify({ executionId: result.executionId, diagnostics: diagnostics.length }),
  );
} finally {
  await session.close();
}
