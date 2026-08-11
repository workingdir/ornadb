import { readdir, readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const outputRoot = resolve(projectRoot, "dist");
const sourceyRoot = resolve(outputRoot, "docs");
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

const files = await findHtmlFiles(sourceyRoot);
files.push(resolve(outputRoot, "docs.html"));

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

console.log(`[website] Normalised ${files.length} Sourcey HTML pages.`);
