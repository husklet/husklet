// A host that keeps its frames instead of writing them to a socket.
//
// The reconciler and the property encoder are the React package's internals
// rather than its public surface, so they are reached by path: a test may look
// inside the package it is written against, while the playground itself uses
// only what the package exports.

import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);

/** The React package's own directory, wherever npm put it. */
export const PACKAGE = require.resolve('@husklet/react').replace(/src[/\\]index\.js$/, '');

const { Surface, reconciler } = await import(new URL('src/reconciler.js', `file://${PACKAGE}`));
export const { value, PROPS } = await import(new URL('src/protocol.js', `file://${PACKAGE}`));

/** A surface that collects frames, exactly as the React package's own tests do. */
export function host() {
  const frames = [];
  const surface = new Surface((frame) => frames.push(frame));
  const container = reconciler.createContainer(surface, 0, null, false, null, '', () => {}, null);
  return {
    frames,
    surface,
    render(element) {
      reconciler.updateContainer(element, container, null, null);
      return frames.at(-1) ?? null;
    },
    /** Every patch sent since the given frame count. */
    since(before) {
      return frames.slice(before).flatMap((frame) => frame.patches);
    },
  };
}
