import { readFile, readdir } from 'node:fs/promises';
import { extname } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { Plugin } from 'vite';
import { defineConfig } from 'vite';

const wasmPackageDirectory = fileURLToPath(new URL('../orna-wasm/pkg/', import.meta.url));

function wasmPackageAssets(): Plugin {
  return {
    name: 'orna-wasm-package-assets',
    configureServer(server) {
      server.middlewares.use('/orna-wasm/pkg', (request, response, next) => {
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

export default defineConfig({
  base: './',
  publicDir: false,
  plugins: [wasmPackageAssets()],
  server: { host: '0.0.0.0' },
});
