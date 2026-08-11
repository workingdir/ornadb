import { defineConfig, markdown } from "sourcey";

export default defineConfig({
    name: "OrnaDB",
    prettyUrls: "slash",
    theme: {
        preset: "default",
        colors: {
            primary: "#326946",
            light: "#4d8c60",
            dark: "#b7d986",
        },
        fonts: {
            sans: "Public Sans",
            mono: "Martian Mono",
        },
        layout: {
            sidebar: "17rem",
            toc: "17rem",
            content: "46rem",
        },
        css: ["./theme.css"],
    },
    favicon: "../public/favicon.svg",
    repo: "https://github.com/workingdir/ornadb",
    editBranch: "main",
    editBasePath: "website/docs",
    navigation: {
        tabs: [
            {
                tab: "Documentation",
                slug: "",
                source: markdown({
                    groups: [
                        {
                            group: "Start here",
                            pages: ["index"],
                        },
                    ],
                }),
            },
        ],
    },
    navbar: {
        links: [
            { type: "link", label: "Frontpage", href: "/" },
            { type: "github", href: "https://github.com/workingdir/ornadb" },
        ],
    },
    footer: {
        links: [
            { type: "link", label: "OrnaDB", href: "/" },
            { type: "github", href: "https://github.com/workingdir/ornadb" },
        ],
    },
    search: {
        featured: ["index"],
    },
});
