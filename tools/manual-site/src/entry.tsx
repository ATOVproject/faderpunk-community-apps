/**
 * Renders the Community App Catalogue to static HTML using the *real*
 * Configurator manual components from a sibling faderpunk checkout
 * (reached via the .faderpunk symlink build.sh creates).
 *
 * Reusing ManualApp rather than reimplementing it is the whole point: the
 * catalogue page and the in-app manual render the same data through the
 * same component, so they can't drift apart visually. An earlier pass
 * hand-wrote a lookalike renderer here and it was never quite right.
 *
 * The coupling this introduces is to faderpunk's file *paths* — if those
 * move, this build fails loudly at import time rather than silently
 * producing something wrong, which is the failure mode the previous
 * forge-based build had (it string-matched markers inside ManualTab.tsx).
 *
 * Usage: node dist/entry.js <repo-root> <output-dir> [downloads-dir]
 */
import fs from "node:fs";
import path from "node:path";
import { renderToStaticMarkup } from "react-dom/server";

import {
  ManualApp,
  type ManualAppData,
} from "../.faderpunk/configurator/src/components/manual/ManualApp";

interface CatalogEntry {
  appId: number;
  module: string;
  author: string;
  version: string;
}

const [, , repoRootArg, outDirArg, downloadsArg] = process.argv;
if (!repoRootArg || !outDirArg) {
  console.error("usage: node dist/entry.js <repo-root> <output-dir> [downloads-dir]");
  process.exit(1);
}
const repoRoot = path.resolve(repoRootArg);
const outDir = path.resolve(outDirArg);
const downloadsDir = downloadsArg ? path.resolve(downloadsArg) : undefined;
const base = import.meta.env.BASE_URL ?? "/";

const catalog: CatalogEntry[] = JSON.parse(
  fs.readFileSync(path.join(repoRoot, "apps-catalog.json"), "utf8"),
);
const manuals: ManualAppData[] = JSON.parse(
  fs.readFileSync(path.join(repoRoot, "manual-tab.json"), "utf8"),
);
const manualById = new Map(manuals.map((m) => [m.appId, m]));

fs.mkdirSync(outDir, { recursive: true });

// Copy each app's built package in, so the catalogue can link straight to
// a download instead of telling people to build from source.
const downloadByApp = new Map<number, string>();
if (downloadsDir && fs.existsSync(downloadsDir)) {
  const downloadsOut = path.join(outDir, "downloads");
  fs.mkdirSync(downloadsOut, { recursive: true });
  const built = fs.readdirSync(downloadsDir).filter((f) => f.endsWith(".fpapp"));
  for (const entry of catalog) {
    const prefix = `${entry.module.replace(/_/g, "-")}-`;
    const file = built.find((f) => f.startsWith(prefix));
    if (!file) continue;
    fs.copyFileSync(path.join(downloadsDir, file), path.join(downloadsOut, file));
    downloadByApp.set(entry.appId, `downloads/${file}`);
  }
}

const entries = catalog
  .filter((entry) => manualById.has(entry.appId))
  .sort((a, b) =>
    manualById.get(a.appId)!.title.localeCompare(manualById.get(b.appId)!.title),
  );

const missing = catalog.filter((entry) => !manualById.has(entry.appId));
if (missing.length) {
  console.warn(
    `warning: no manual-tab.json entry for: ${missing.map((m) => m.module).join(", ")}`,
  );
}

const Page = () => (
  <>
    <h1 className="text-yellow-fp mb-4 text-xl font-bold uppercase">
      Community App Catalogue
    </h1>
    <p className="mb-6 max-w-[60ch]">
      Every app submitted to faderpunk-community-apps. Download a{" "}
      <code>.fpapp</code> and install it from{" "}
      <strong>Apps &gt; Installed Apps</strong> in the Configurator, or build
      from source with <code>fpapp build-community</code>. Unofficial,
      community-maintained, not affiliated with ATOV.
    </p>
    <h2 className="text-yellow-fp mt-8 mb-4 text-xl font-bold uppercase">
      Contents
    </h2>
    <ul className="my-3 ml-3 list-inside list-disc">
      {entries.map((entry) => (
        <li key={entry.appId}>
          <a className="font-semibold underline" href={`#app-${entry.appId}`}>
            {manualById.get(entry.appId)!.title}
          </a>
        </li>
      ))}
    </ul>
    {entries.map((entry) => {
      const manual = manualById.get(entry.appId)!;
      const download = downloadByApp.get(entry.appId);
      return (
        <div key={entry.appId}>
          <div className="mt-12 mb-2 flex items-center justify-between gap-4">
            <span className="text-gray-400 text-sm">
              v{entry.version} · by {entry.author}
            </span>
            {download ? (
              <a
                className="bg-yellow-fp rounded-sm px-3 py-1.5 text-sm font-semibold text-black"
                href={download}
              >
                Download .fpapp
              </a>
            ) : null}
          </div>
          <ManualApp app={manual} />
        </div>
      );
    })}
  </>
);

// ManualApp hardcodes root-absolute "/img/*.svg" for the jack, fader and
// button glyphs (it only routes the per-app icons through BASE_URL).
// That's fine in the Configurator, which is served from the domain root,
// but this is a project Pages site under /<repo>/ — so rebase them here
// rather than patching upstream for our benefit.
const body = renderToStaticMarkup(<Page />).replaceAll('src="/img/', `src="${base}img/`);

const html = `<!doctype html>
<html lang="en" class="dark">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Faderpunk Community App Catalogue</title>
<link rel="stylesheet" href="${base}styles.css">
</head>
<body>
<div class="mx-auto max-w-5xl px-4 py-10">
${body}
</div>
</body>
</html>
`;

fs.writeFileSync(path.join(outDir, "index.html"), html);
console.log(
  `wrote ${path.join(outDir, "index.html")} (${entries.length} apps, ${downloadByApp.size} downloads)`,
);
