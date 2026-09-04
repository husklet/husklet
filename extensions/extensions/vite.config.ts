import { defineConfig } from 'vite';

export default defineConfig({
  build: {
    emptyOutDir: true,
    outDir: 'dist',
    ssr: 'src/main.tsx',
    target: 'node20',
    rollupOptions: { external: ['react', 'react/jsx-runtime', '@husklet/client', '@husklet/react'] },
  },
});
