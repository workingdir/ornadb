import { defineConfig } from "astro/config";
import sourcey from "sourcey/astro";

export default defineConfig({
    integrations: [
        sourcey({
            config: "./docs/sourcey.config.ts",
            routeBase: "/docs",
            build: { generateOgImages: false },
            dev: { generateOgImages: false },
        }),
    ],
});
