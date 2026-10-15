## What kind of PR is this?

<!-- Exactly one. Mixed PRs are rejected — see CONTRIBUTING.md. -->

- [ ] Submission — a new app (`apps/<name>.rs` + one entry each in `manual-tab.json` and `apps-catalog.json`)
- [ ] App fix — only existing `apps/<name>.rs` files
- [ ] Manual edit — only existing entries in `manual-tab.json`
- [ ] Version bump — only `version` in `apps-catalog.json` (list the PRs it covers below)
- [ ] Maintainer change — CI, scripts, tools, schemas, `CONTRIBUTING.md`, `README.md`

## What and why

## Checklist

- [ ] `make fpapps` builds every app I touched
- [ ] Tested on hardware (or: say here why not)
- [ ] The manual still matches what the app does
- [ ] Title follows the kind: `feat: add <App>` / `fix: <App> — …` / `docs: <App> manual …` / `chore: bump <Apps> …`

## Overlap and ordering

<!-- If this touches the same file as another open PR, say which merges first
     ("merge after #NN"). The check lists overlapping PRs for you. -->

## Version bump (if that's what this is)

<!-- Which merged PRs / changes does each bump cover? -->
