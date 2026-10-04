import { readFile, readdir } from 'node:fs/promises';
import { extname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { Plugin } from 'vite';
import { defineConfig } from 'vite';

const webUi = fileURLToPath(new URL('.', import.meta.url));

const lspPackageDirectory = fileURLToPath(new URL('./public/lsp-wasm/', import.meta.url));
const monacoEsmDirectory = fileURLToPath(
  new URL('./node_modules/monaco-editor/esm', import.meta.url),
);

function lspPackageAssets(): Plugin {
  return {
    name: 'orna-lsp-wasm-package-assets',
    configureServer(server) {
      server.middlewares.use('/playground/assets/lsp-wasm', (request, response, next) => {
        let requestedName: string;
        try {
          requestedName = decodeURIComponent((request.url ?? '').split('?')[0].replace(/^\/+/, ''));
        } catch {
          next();
          return;
        }
        if (!requestedName || requestedName.includes('/') || requestedName.includes('\\')) {
          next();
          return;
        }

        void readFile(new URL(requestedName, `file://${lspPackageDirectory}/`))
          .then((content) => {
            response.statusCode = 200;
            response.setHeader(
              'Content-Type',
              extname(requestedName) === '.wasm' ? 'application/wasm' : 'text/javascript; charset=utf-8',
            );
            response.setHeader('Cache-Control', 'no-cache');
            response.end(content);
          })
          .catch(() => {
            response.statusCode = 503;
            response.setHeader('Content-Type', 'text/plain; charset=utf-8');
            response.end('LSP package is not built. Run npm run lsp:build in playground/web-ui.');
          });
      });
    },
    async generateBundle() {
      let files: string[];
      try {
        files = await readdir(lspPackageDirectory);
      } catch {
        return;
      }

      for (const fileName of files) {
        if (!['.js', '.wasm'].includes(extname(fileName))) continue;
        this.emitFile({
          type: 'asset',
          fileName: `assets/lsp-wasm/${fileName}`,
          source: await readFile(new URL(fileName, `file://${lspPackageDirectory}/`)),
        });
      }
    },
  };
}

export default defineConfig({
  base: '/playground/',
  publicDir: 'public',
  resolve: {
    alias: [{ find: 'monaco-editor/esm', replacement: monacoEsmDirectory }],
  },
  build: {
    rollupOptions: {
      input: {
        index: resolve(webUi, 'index.html'),
        embed: resolve(webUi, 'src/embed.ts'),
      },
      output: {
        manualChunks(id) {
          const modulePath = id.replaceAll('\\', '/');
          if (modulePath.includes('/monaco-editor/esm/vs/base/')) return 'monaco-base';
          if (modulePath.includes('/monaco-editor/esm/vs/editor/')) return 'monaco-editor';
          return undefined;
        },
        entryFileNames: 'assets/[name].js',
        chunkFileNames: 'assets/[name].js',
        assetFileNames: 'assets/[name][extname]',
      },
    },
  },
  plugins: [lspPackageAssets()],
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
