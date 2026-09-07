import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
const root = require.resolve('@husklet/react').replace(/dist[/\\]index\.js$/, '');
const { Surface, reconciler } = await import(new URL('dist/reconciler.js', `file://${root}`));

export function host() {
  const frames = [];
  const surface = new Surface((frame) => frames.push(frame));
  const container = reconciler.createContainer(surface, 0, null, false, null, '', () => {}, null);
  return {
    frames,
    surface,
    render(element) {
      reconciler.updateContainer(element, container, null, null);
      return frames.at(-1);
    },
  };
}
