import { defineConfig } from 'vite';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const webUi = fileURLToPath(new URL('.', import.meta.url));

export default defineConfig({
  base: '/playground/',
  publicDir: 'public',
  build: {
    rollupOptions: {
      input: {
        index: resolve(webUi, 'index.html'),
        embed: resolve(webUi, 'src/embed.ts'),
      },
      output: {
        entryFileNames: 'assets/[name].js',
        chunkFileNames: 'assets/[name].js',
        assetFileNames: 'assets/[name][extname]',
      },
    },
  },
  server: {
    host: '0.0.0.0',
    proxy: {
      '/api': {
        target: process.env.ORNA_SERVE_URL ?? 'http://127.0.0.1:8181',
      },
      '/orna': {
        target: process.env.ORNA_SERVE_URL ?? 'http://127.0.0.1:8181',
        ws: true,
      },
    },
  },
});
