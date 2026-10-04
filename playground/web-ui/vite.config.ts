import { defineConfig } from 'vite';

export default defineConfig({
  base: '/playground/',
  publicDir: false,
  build: {
    rollupOptions: {
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
