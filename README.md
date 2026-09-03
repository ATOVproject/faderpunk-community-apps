# faderpunk-community-apps

Community-submitted apps for [Faderpunk](https://github.com/ATOVproject/faderpunk) — unofficial, unmaintained by the core team, and without guarantees.

Faderpunk firmware with FPApp support can install these apps individually from
the Configurator. Installing an app does not replace the firmware.

The repository keeps app source, manual entries, and the catalogue together,
plus the small build and review tools needed to keep them trustworthy. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the submission format and rules.

Browse what each community app does before building anything: **[the community manual](https://atovproject.github.io/faderpunk-community-apps/#/manual)** — same look as the official manual, deployed straight from this repo's current catalog on every merge to `main`.

## Layout

- `apps/<name>.rs` — one file per app, same pattern as official Faderpunk apps.
- `manual-tab.json` — one entry per app, matching the same `ManualAppData` shape the official configurator uses for its manual tab, stored as data (JSON) rather than `.tsx` source — never executable code. Validated against `manual-tab.schema.json`.
- `apps-catalog.json` — one entry per app (`appId`, `module`, `author`), validated against `apps-catalog.schema.json`. Its `appId` links to the matching `manual-tab.json` entry.

Community app IDs start at 100 (official apps use 1–99) and are permanent once assigned.

## Build installable apps

Clone this repository and Faderpunk next to one another, check out the firmware
revision that will run on the device, then build every catalogued app:

```sh
make fpapps
```

The `.fpapp` files are written to `build/fpapps/`. Each package includes the
app, its manual, setup notes, and Configurator metadata. It is deliberately
matched to the checked-out firmware revision; rebuild the packages after
updating firmware.

If the repositories are not siblings, provide the Faderpunk checkout path:

```sh
make fpapps FADERPUNK_DIR=/path/to/faderpunk
```

Connect Faderpunk normally, open **Apps**, scroll to **Installed Apps**, and
install a package in one of the four slots. The installed app then appears in
the normal app catalogue above and can be added to a layout.

## Status

Private for now — not yet open to public PRs, though the mechanical review gate is live: `.github/workflows/pr-scope.yml` runs on every PR (scope, API boundary, panic/unsafe rules, catalog + manual-tab validation, real solo-app compile check against the actual `App<N>` API). The AI first-pass review step isn't wired up yet — advisory only, not a merge gate, per `CONTRIBUTING.md`.

The current catalogue contains Heat Pump, Grooves, and Sift.
