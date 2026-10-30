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
  EmbedPage,
  type AppGridItem,
} from "../.faderpunk/configurator/src/apps-embed/AppGrid";
import { Icon } from "../.faderpunk/configurator/src/components/Icon";
import {
  ManualApp,
  type ManualAppData,
} from "../.faderpunk/configurator/src/components/manual/ManualApp";
import { COLORS_CLASSES } from "../.faderpunk/configurator/src/utils/class-helpers";

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

// Display order of the categories is the schema's enum order, so adding or
// reordering one is a single edit there. Apps without a category fall into a
// trailing "Other" group rather than disappearing.
const OTHER = "Other";
const categoryOrder: string[] = [
  ...JSON.parse(
    fs.readFileSync(path.join(repoRoot, "manual-tab.schema.json"), "utf8"),
  ).items.properties.category.enum,
  OTHER,
];
const categoryOf = (appId: number) =>
  (manualById.get(appId) as { category?: string }).category ?? OTHER;

fs.mkdirSync(outDir, { recursive: true });

// Copy each app's built package in, so the catalogue can link straight to
// a download instead of telling people to build from source.
const downloadByApp = new Map<number, string>();
if (downloadsDir && fs.existsSync(downloadsDir)) {
  const downloadsOut = path.join(outDir, "downloads");
  fs.mkdirSync(downloadsOut, { recursive: true });
  const built = fs.readdirSync(downloadsDir).filter((f) => f.endsWith(".fpapp"));
  for (const entry of catalog) {
    // Accept both "<module>.fpapp" and "<module>-<version>.fpapp": the
    // version suffix only appears once ATOVproject/faderpunk#695 lands,
    // and this build shouldn't care which side merges first.
    const stem = entry.module.replace(/_/g, "-");
    const pattern = new RegExp(`^${stem}(-\\d+\\.\\d+\\.\\d+)?\\.fpapp$`);
    const file = built.find((f) => pattern.test(f));
    if (!file) continue;
    fs.copyFileSync(path.join(downloadsDir, file), path.join(downloadsOut, file));
    downloadByApp.set(entry.appId, `downloads/${file}`);
  }
}

const entries = catalog
  .filter((entry) => manualById.has(entry.appId))
  .sort(
    (a, b) =>
      categoryOrder.indexOf(categoryOf(a.appId)) -
        categoryOrder.indexOf(categoryOf(b.appId)) ||
      manualById.get(a.appId)!.title.localeCompare(manualById.get(b.appId)!.title),
  );

for (const entry of entries) {
  if (!categoryOrder.includes(categoryOf(entry.appId))) {
    throw new Error(
      `${entry.module}: unknown category "${categoryOf(entry.appId)}" — add it to the enum in manual-tab.schema.json`,
    );
  }
}
const uncategorised = entries.filter((e) => categoryOf(e.appId) === OTHER);
if (uncategorised.length) {
  console.warn(
    `warning: no category for: ${uncategorised.map((e) => e.module).join(", ")}`,
  );
}

const gridApps: (AppGridItem & { author: string })[] = entries.map((entry) => {
  const manual = manualById.get(entry.appId)!;
  return {
    id: entry.appId,
    author: entry.author,
    name: manual.title,
    description: manual.description,
    color: manual.color,
    icon: manual.icon,
    href: `${base}#app-${entry.appId}`,
  };
});

// Icon cards grouped under a heading per category, empty categories
// omitted. Same card as faderpunk's AppGrid, but that component is a fixed
// one/two-column list, so the layout is ours: columns follow the width of
// the *container* (an iframe is as wide as its embedder gives it, which
// says nothing about the viewport), up to three.
function CategorizedGrid({ newTab }: { newTab?: boolean }) {
  return (
    <>
      {categoryOrder.map((category) => {
        const apps = gridApps.filter((a) => categoryOf(a.id) === category);
        if (!apps.length) return null;
        return (
          <section key={category} className="@container mb-10">
            <h2 className="text-yellow-fp mb-4 text-xl font-bold uppercase">
              {category}
            </h2>
            <ul className="grid grid-cols-1 gap-x-8 gap-y-4 @xl:grid-cols-2 @4xl:grid-cols-3">
              {apps.map((app) => (
                <li key={app.id}>
                  <a
                    // In-page anchors must stay bare "#app-N": the iframe's
                    // resize script only forwards clicks on a[href^="#"] to
                    // the embedder, which does the scrolling.
                    href={newTab ? app.href : `#app-${app.id}`}
                    {...(newTab
                      ? { target: "_blank", rel: "noopener noreferrer" }
                      : {})}
                    className="flex items-center gap-4"
                  >
                    <div
                      className={`flex h-14 w-14 shrink-0 items-center justify-center rounded-sm p-2 ${
                        COLORS_CLASSES[app.color as keyof typeof COLORS_CLASSES]?.bg ?? ""
                      }`}
                    >
                      <Icon className="h-8 w-8 text-black" name={app.icon} />
                    </div>
                    <div>
                      <h3 className="text-yellow-fp font-bold uppercase">{app.name}</h3>
                      <p className="text-sm text-white/80">{app.description}</p>
                      <p className="text-xs text-white/50">by {app.author}</p>
                    </div>
                  </a>
                </li>
              ))}
            </ul>
          </section>
        );
      })}
    </>
  );
}

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
      the <strong>Community</strong> tab in the Configurator, or build
      from source with <code>fpapp build-community</code>. Unofficial,
      community-maintained, not affiliated with ATOV.
    </p>
    <h2 className="text-yellow-fp mt-8 mb-4 text-xl font-bold uppercase">
      Contents
    </h2>
    <CategorizedGrid />
    {entries.map((entry) => {
      const manual = manualById.get(entry.appId)!;
      const download = downloadByApp.get(entry.appId);
      return (
        <div key={entry.appId}>
          <div className="mt-12 mb-2">
            <span className="text-gray-400 text-sm">
              v{entry.version} · by {entry.author}
            </span>
          </div>
          <ManualApp
            app={manual}
            action={
              download ? (
                <a
                  className="bg-yellow-fp rounded-sm px-2 py-1.5 text-center text-xs font-semibold text-black"
                  href={download}
                >
                  Download
                </a>
              ) : null
            }
          />
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

// The Configurator embeds this page in an iframe, and an iframe can't
// size itself to its content — the embedder would have to guess a height
// and leave the reader scrolling a box inside a page. It's also
// cross-origin, so the parent can't measure us directly. So report our
// height instead and let it resize; see CommunityCatalogue.tsx in
// faderpunk, which listens for exactly these messages and checks the
// origin before trusting them. Standalone viewers just ignore it.
const RESIZE_SCRIPT = `
(function () {
  if (window.parent === window) return;
  var last = 0;
  function post() {
    last = document.documentElement.scrollHeight;
    window.parent.postMessage({ type: "fp-catalogue-height", height: last }, "*");
  }
  function postIfChanged() {
    if (document.documentElement.scrollHeight !== last) post();
  }
  // The embedder may mount its listener after we've already loaded, so
  // answering on request is what actually makes this reliable —
  // spontaneous reports alone lose the race and we'd never speak again.
  window.addEventListener("message", function (event) {
    if (event.data && event.data.type === "fp-catalogue-request-height") post();
  });
  // The frame is as tall as the whole page, so the browser has nothing to
  // scroll inside it and an in-page link (the contents list) would appear
  // to do nothing. Hand the target's position to the embedder, which
  // scrolls its own page instead.
  document.addEventListener("click", function (event) {
    var link = event.target && event.target.closest && event.target.closest('a[href^="#"]');
    if (!link) return;
    var target = document.getElementById(decodeURIComponent(link.getAttribute("href").slice(1)));
    if (!target) return;
    event.preventDefault();
    window.parent.postMessage({
      type: "fp-catalogue-scroll",
      top: target.getBoundingClientRect().top + window.pageYOffset
    }, "*");
  });
  if (window.ResizeObserver) new ResizeObserver(postIfChanged).observe(document.documentElement);
  window.addEventListener("load", post);
  document.addEventListener("DOMContentLoaded", post);
  // Fonts and images land after load and change the height; catch that
  // without leaving a timer running forever.
  [100, 500, 1500].forEach(function (d) { setTimeout(postIfChanged, d); });
  post();
})();
`.trim();

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
<script>${RESIZE_SCRIPT}</script>
</body>
</html>
`;

fs.writeFileSync(path.join(outDir, "index.html"), html);
console.log(
  `wrote ${path.join(outDir, "index.html")} (${entries.length} apps, ${downloadByApp.size} downloads)`,
);

// A compact icon grid of the same apps, for faderpunk.io to iframe at
// /embed/. It uses the same cards as faderpunk.io/apps, grouped by category
// like the catalogue page, and links each card to its entry on the
// catalogue page. Unlike that page there's no resize script: the embedder gives
// this one a fixed height.
const embedHtml = `<!doctype html>
<html lang="en" class="dark">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Faderpunk Community Apps</title>
<link rel="stylesheet" href="${base}styles.css">
</head>
<body>
${renderToStaticMarkup(
  <EmbedPage>
    <CategorizedGrid newTab />
  </EmbedPage>,
)}
</body>
</html>
`;

fs.mkdirSync(path.join(outDir, "embed"), { recursive: true });
fs.writeFileSync(path.join(outDir, "embed", "index.html"), embedHtml);
console.log(
  `wrote ${path.join(outDir, "embed", "index.html")} (${gridApps.length} apps)`,
);
