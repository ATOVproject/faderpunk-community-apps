# faderpunk-community-apps

Community-submitted apps for [Faderpunk](https://github.com/ATOVproject/faderpunk) — unofficial, unmaintained by the core team, and without guarantees.

Faderpunk firmware with FPApp support can install these apps individually from
the Configurator. Installing an app does not replace the firmware.

The repository keeps app source, manual entries, and the catalogue together,
plus the small build and review tools needed to keep them trustworthy. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the submission format and rules.

Browse what each community app does before building anything: **[the community manual](https://atovproject.github.io/faderpunk-community-apps/#/manual)** — same look as the official manual, built from this repo's catalog.

## Layout

- `apps/<name>.rs` — one file per app, same pattern as official Faderpunk apps.
- `manual-tab.json` — one entry per app, matching the same `ManualAppData` shape the official configurator uses for its manual tab, stored as data (JSON) rather than `.tsx` source — never executable code. Validated against `manual-tab.schema.json`.
- `apps-catalog.json` — one entry per app (`appId`, `module`, `author`, `version`), validated against `apps-catalog.schema.json`. Its `appId` links to the matching `manual-tab.json` entry; `author` is the contributor's GitHub login.

Community app IDs start at 100 (official apps use 1–99) and are permanent once assigned.

## Build installable apps

Clone this repository and Faderpunk next to one another, then build every
catalogued app:

```sh
make fpapps
```

The `.fpapp` files are written to `build/fpapps/`. Each package includes the
app, its manual, setup notes, and Configurator metadata. A package's
compatibility with a given firmware build is decided by an ABI contract, not
by matching revisions — a package keeps working across firmware updates that
don't break the FPApp ABI, so there's no need to rebuild after every update.

If the repositories are not siblings, provide the Faderpunk checkout path:

```sh
make fpapps FADERPUNK_DIR=/path/to/faderpunk
```

Prebuilt packages: each [release](https://github.com/ATOVproject/faderpunk-community-apps/releases) carries a ZIP with every app as a `.fpapp`, extract it and install the ones you want. Releases are cut by the maintainer with the **Prepare release** workflow; see [CONTRIBUTING.md](CONTRIBUTING.md#releases).

Connect Faderpunk normally, open **Apps**, scroll to **Installed Apps**, and
install a package in one of the available slots. The installed app then appears in
the normal app catalogue above and can be added to a layout.

## Status

Open to pull requests from anyone. Every PR is exactly one kind — a new app, an app fix, a manual edit, a version bump, or a maintainer change — and `.github/workflows/pr-scope.yml` checks it on every PR (scope and ownership, API boundary, panic/unsafe rules, catalog + manual-tab validation, and a real FPApp build of each touched app, the same `make fpapps` the downloads site runs). A maintainer then reviews and merges. The AI first-pass review step isn't wired up yet — advisory only, not a merge gate. See [CONTRIBUTING.md](CONTRIBUTING.md) for the kinds, the rules and the review process.

See [`apps-catalog.json`](apps-catalog.json) for the current catalogue.
