## What kind of PR is this?

<!-- Exactly one. Mixed PRs are rejected — see CONTRIBUTING.md. -->

- [ ] Submission — a new app (`apps/<name>.rs` + one entry each in `manual-tab.json` and `apps-catalog.json`)
- [ ] App fix — only existing `apps/<name>.rs` files
- [ ] Manual edit — only existing entries in `manual-tab.json`
- [ ] Version bump — only `version` in `apps-catalog.json` (normally the generated `chore: prepare release` PR, not hand-written)
- [ ] Maintainer change — CI, scripts, tools, schemas, `CONTRIBUTING.md`, `README.md`

## What and why

## Checklist

- [ ] `make fpapps` builds every app I touched
- [ ] Tested on hardware (or: say here why not)
- [ ] The manual still matches what the app does
- [ ] Title follows the kind and is a conventional commit (`feat: add <App>`, `fix: <App> — …`, `docs: <App> manual …`, `chore: bump <Apps> …`): `fix:`/`feat:`/`fix!:` decide the version bump at the next release

## Overlap and ordering

<!-- If this touches the same file as another open PR, say which merges first
     ("merge after #NN"). The check lists overlapping PRs for you. -->

## Version bump (only if you are writing one by hand)

<!-- Which merged PRs / changes does each bump cover? Is a release PR already open? -->
