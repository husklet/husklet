import { readdirSync } from 'node:fs';
import { basename, resolve } from 'node:path';
import { defineConfig } from 'vite';

const entries = Object.fromEntries(
  readdirSync('src')
    .filter((file) => /\.(?:ts|tsx)$/.test(file))
    .map((file) => [basename(file, file.endsWith('.tsx') ? '.tsx' : '.ts'), resolve('src', file)]),
);

export default defineConfig({
  build: {
    emptyOutDir: true,
    outDir: 'dist',
    ssr: true,
    target: 'node20',
    rollupOptions: {
      input: entries,
      output: { entryFileNames: '[name].js' },
      external: ['react', 'react/jsx-runtime', '@husklet/client', '@husklet/react'],
    },
  },
});
