import { mkdir, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const outputRoot = resolve(projectRoot, "dist");
const legacyDocsRoot = resolve(outputRoot, "docs");
const legacyDocsAlias = resolve(outputRoot, "docs.html");
const bodyMarker = '<body id="sourcey">';
const skipLink = '<a class="orna-skip-link" href="#docs">Skip to content</a>';

async function findHtmlFiles(directory) {
    const entries = await readdir(directory, { withFileTypes: true });
    const files = [];

    for (const entry of entries) {
        const path = join(directory, entry.name);
        if (entry.isDirectory()) {
            files.push(...(await findHtmlFiles(path)));
        } else if (entry.isFile() && entry.name.endsWith(".html")) {
            files.push(path);
        }
    }

    return files;
}

function normaliseSearchButton(html, file) {
    const searchButton = /<button id="search-open"[\s\S]*?<\/button>/;
    if (!searchButton.test(html)) {
        throw new Error(`[website] Sourcey search button missing in ${file}`);
    }

    return html.replace(searchButton, (button) =>
        button.replaceAll("<div", "<span").replaceAll("</div>", "</span>"),
    );
}

function publicPathFor(file) {
    const outputPath = relative(outputRoot, file).split(sep).join("/");
    if (outputPath === "index.html") {
        return "/";
    }
    if (outputPath.endsWith("/index.html")) {
        return `/${outputPath.slice(0, -"index.html".length)}`;
    }
    return `/${outputPath}`;
}

function renderRedirect(target) {
    return `<!doctype html>
<html lang="en-GB">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <meta http-equiv="refresh" content="0; url=${target}">
    <link rel="canonical" href="${target}">
    <title>Moved - OrnaDB</title>
</head>
<body>
    <p>This page moved to <a href="${target}">${target}</a>.</p>
</body>
</html>
`;
}

await rm(legacyDocsRoot, { recursive: true, force: true });
await rm(legacyDocsAlias, { force: true });

const files = await findHtmlFiles(outputRoot);

for (const file of files) {
    const html = await readFile(file, "utf8");
    if (!html.includes(bodyMarker)) {
        throw new Error(`[website] Sourcey body marker missing in ${file}`);
    }

    const withSkipLink = html.includes(skipLink)
        ? html
        : html.replace(bodyMarker, `${bodyMarker}${skipLink}`);
    const updated = normaliseSearchButton(withSkipLink, file)
        .replace('<html lang="en">', '<html lang="en-GB">')
        .replace('<nav id="nav" role="navigation">', '<nav id="nav">')
        .replaceAll("strokeWidth=", "stroke-width=")
        .replaceAll("strokeLinecap=", "stroke-linecap=")
        .replaceAll("strokeLinejoin=", "stroke-linejoin=")
        .replaceAll(
            'xmlns="http://www.w3.org/2000/svg">Copy<path',
            'xmlns="http://www.w3.org/2000/svg"><path',
        )
        .replace(/<a href(?=[ >])/g, '<a href=""');
    await writeFile(file, updated, "utf8");
}

for (const file of files) {
    const target = publicPathFor(file);
    const legacyPath = resolve(legacyDocsRoot, relative(outputRoot, file));
    await mkdir(dirname(legacyPath), { recursive: true });
    await writeFile(legacyPath, renderRedirect(target), "utf8");
}

await writeFile(legacyDocsAlias, renderRedirect("/"), "utf8");

console.log(
    `[website] Normalised ${files.length} Sourcey pages and wrote ${files.length + 1} legacy redirects.`,
);
