import { defineConfig } from "@playwright/test";

const basePath = process.env.VITE_BASE_PATH;
const mountPath = basePath?.startsWith("/") ? `/${basePath.split("/").filter(Boolean).join("/")}/` : "/";
const previewUrl = `http://127.0.0.1:4173${mountPath}`;

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  reporter: "list",
  use: {
    baseURL: previewUrl,
    browserName: "chromium",
    channel: "chromium",
  },
  webServer: {
    command: "npm run preview -- --host 127.0.0.1 --port 4173 --strictPort",
    url: previewUrl,
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
  },
});
