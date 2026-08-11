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
                            pages: ["index", "getting-started", "status"],
                        },
                        {
                            group: "Language",
                            pages: ["object-model", "functions"],
                        },
                        {
                            group: "Execution",
                            pages: ["invocation", "ui-and-runtimes", "architecture"],
                        },
                        {
                            group: "Trust and tools",
                            pages: ["security-and-inspection"],
                        },
                        {
                            group: "Reference",
                            pages: ["examples", "glossary"],
                        },
                    ],
                }),
            },
        ],
    },
    navbar: {
        links: [
            { type: "link", label: "Status", href: "/status/" },
            { type: "github", href: "https://github.com/workingdir/ornadb" },
        ],
    },
    footer: {
        links: [
            { type: "link", label: "Getting started", href: "/getting-started/" },
            { type: "github", href: "https://github.com/workingdir/ornadb" },
        ],
    },
    search: {
        featured: ["index"],
    },
});
