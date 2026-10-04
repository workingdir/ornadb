import { readFile, readdir } from 'node:fs/promises';
import { extname } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { Plugin } from 'vite';
import { defineConfig } from 'vite';

const wasmPackageDirectory = fileURLToPath(new URL('../orna-wasm/pkg/', import.meta.url));
const lspPackageDirectory = fileURLToPath(new URL('./public/lsp-wasm/', import.meta.url));
const monacoEsmDirectory = fileURLToPath(
  new URL('./node_modules/monaco-editor/esm', import.meta.url),
);

function wasmPackageAssets(): Plugin {
  return {
    name: 'orna-wasm-package-assets',
    configureServer(server) {
      server.middlewares.use('/playground/orna-wasm/pkg', (request, response, next) => {
        const requestedName = decodeURIComponent((request.url ?? '').split('?')[0].replace(/^\/+/, ''));
        if (!requestedName || requestedName.includes('/') || requestedName.includes('\\')) {
          next();
          return;
        }

        void readFile(new URL(requestedName, `file://${wasmPackageDirectory}/`))
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
            response.end('WASM package is not built. Run npm run wasm:build in playground/web-ui.');
          });
      });
    },
    async generateBundle() {
      let files: string[];
      try {
        files = await readdir(wasmPackageDirectory);
      } catch {
        return;
      }

      for (const fileName of files) {
        if (!['.js', '.wasm'].includes(extname(fileName))) continue;
        this.emitFile({
          type: 'asset',
          fileName: `orna-wasm/pkg/${fileName}`,
          source: await readFile(new URL(fileName, `file://${wasmPackageDirectory}/`)),
        });
      }
    },
  };
}

function lspPackageAssets(): Plugin {
  return {
    name: 'orna-lsp-wasm-package-assets',
    configureServer(server) {
      server.middlewares.use('/playground/lsp-wasm', (request, response, next) => {
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
          fileName: `lsp-wasm/${fileName}`,
          source: await readFile(new URL(fileName, `file://${lspPackageDirectory}/`)),
        });
      }
    },
  };
}

export default defineConfig({
  base: '/playground/',
  publicDir: false,
  resolve: {
    alias: [{ find: 'monaco-editor/esm', replacement: monacoEsmDirectory }],
  },
  plugins: [wasmPackageAssets(), lspPackageAssets()],
  server: {
    host: '0.0.0.0',
    proxy: {
      '/api': {
        target: process.env.ORNA_SERVE_URL ?? 'http://127.0.0.1:8181',
      },
    },
  },
});
