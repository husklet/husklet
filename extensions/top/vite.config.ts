import { defineConfig } from 'vite';

export default defineConfig({
  build: {
    emptyOutDir: true,
    outDir: 'dist',
    ssr: true,
    target: 'node20',
    rollupOptions: {
      input: {
        main: 'src/main.tsx',
        app: 'src/app.tsx',
        model: 'src/model.ts',
        selection: 'src/selection.ts',
      },
      output: { entryFileNames: '[name].js' },
      external: ['react', 'react/jsx-runtime', '@husklet/client', '@husklet/react'],
    },
  },
});
