# faderpunk-community-apps

Community-submitted apps for [Faderpunk](https://github.com/ATOVproject/faderpunk) — unofficial, unmaintained by the core team, and without guarantees. Building a firmware that includes any of these is opt-in, via [`faderpunk-store`](https://github.com/ATOVproject/faderpunk-store).

This repo contains *only* apps, their manual entries, and a catalog — nothing else. See [CONTRIBUTING.md](CONTRIBUTING.md) for the submission format and rules.

## Layout

- `apps/<name>.rs` — one file per app, same pattern as official Faderpunk apps.
- `manual-tab.json` — one entry per app, matching the same `ManualAppData` shape the official configurator uses for its manual tab, stored as data (JSON) rather than `.tsx` source — never executable code. Validated against `manual-tab.schema.json`.
- `apps-catalog.json` — one entry per app (`appId`, `module`, `author`), validated against `apps-catalog.schema.json`. Its `appId` links to the matching `manual-tab.json` entry.

Community app IDs start at 100 (official apps use 1–99) and are permanent once assigned.

## Status

Private for now — not yet open to public PRs, though the mechanical review gate is live: `.github/workflows/pr-scope.yml` runs on every PR (scope, API boundary, panic/unsafe rules, catalog + manual-tab validation, real solo-app compile check against the actual `App<N>` API). The AI first-pass review step isn't wired up yet — advisory only, not a merge gate, per `CONTRIBUTING.md`.

`apps/`, `apps-catalog.json`, and `manual-tab.json` start empty — no seeded entries.
