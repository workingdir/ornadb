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

const files = await findHtmlFiles(sourceyRoot);
files.push(resolve(outputRoot, "docs.html"));

for (const file of files) {
    const html = await readFile(file, "utf8");
    if (!html.includes(bodyMarker)) {
        throw new Error(`[website] Sourcey body marker missing in ${file}`);
    }

    const updated = html
        .replace('<html lang="en">', '<html lang="en-GB">')
        .replace(bodyMarker, `${bodyMarker}${skipLink}`);
    await writeFile(file, updated, "utf8");
}

console.log(`[website] Added the documentation skip link to ${files.length} pages.`);
