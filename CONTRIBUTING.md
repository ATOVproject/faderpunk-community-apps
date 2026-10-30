# Contributing to the community apps

Pull requests from anyone are welcome. Mechanical checks (scope, API boundary,
panic/unsafe rules, catalog and manual validity, a real FPApp build) run on
every PR — see `.github/workflows/pr-scope.yml` and `.github/scripts/`. They
are filters, not the final word: the repo owner makes every merge decision.
The AI first-pass review isn't wired up yet (it needs a provider/model and API
key decision) — advisory only, not a blocker.

Principle: **red CI always means a real problem.** Every legitimate kind of PR
has a name and a scope below, so nothing is meant to fail "by design" and be
merged by override. If you hit a case that none of these covers, open an issue
instead of a PR.

## Which kind of PR is it?

A PR is exactly **one** of these kinds. Mixing kinds is rejected — split it.

| Kind | What it changes | Who may open it | Title |
|---|---|---|---|
| [Submission](#a-submission) | adds one app: `apps/<name>.rs` + one entry each in `manual-tab.json` and `apps-catalog.json` | anyone | `feat: add <App>` |
| [App fix](#an-app-fix) | only existing `apps/<name>.rs` files | anyone (the app's author is flagged for review) | `fix: <App> — …` / `refactor: …` |
| [Manual edit](#a-manual-edit) | only existing entries in `manual-tab.json` | the app's author, or a maintainer | `docs: <App> manual …` |
| [Version bump](#a-version-bump) | only `version` on one or more `apps-catalog.json` entries | the app's author, or a maintainer. Normally it is the generated release PR | `chore: prepare release` (generated), or `chore: bump <App(s)> …` |
| [Maintainer change](#a-maintainer-change) | CI, scripts, tools, schemas, `CONTRIBUTING.md`, `README.md`, `.gitignore` | anyone may propose; a maintainer must review | `ci:` / `chore:` / `docs:` |

"Maintainer" means the repo owner(s), listed in `.github/MAINTAINERS`. The list
is read from the PR's base commit, never from the PR itself, so a PR can't add
itself. An app's author is the `author` field of its `apps-catalog.json` entry,
which is its GitHub login.

Any path outside this table (a stray `apps/Foo.rs`, `apps/sub/x.rs`, a new file
in the repo root, …) is rejected.

### A submission

Exactly three things, added in one PR, and nothing else:

1. `apps/<name>.rs` — your app, following the exact same pattern as
   official Faderpunk apps: a `CHANNELS` constant, a `CONFIG` constant, a
   `wrapper` task, and a `run` function. See any app in the main
   [`faderpunk`](https://github.com/ATOVproject/faderpunk) repo's
   `faderpunk/src/apps/` for the pattern, or that repo's `AGENTS.md`
   ("Creating a New App"). There is no separate API for community apps —
   it's the same `App<N>` facade official apps use.
2. One appended entry in `manual-tab.json`, matching
   `manual-tab.schema.json` — the manual entry: title, description,
   params, storage, the long-form write-up, and per-channel jack/fader/LED
   documentation. Same shape the official apps' manual entries use in the
   configurator (`ManualAppData` in `configurator/src/components/manual/
   ManualApp.tsx`), stored here as plain JSON data rather than `.tsx`
   source — this file only ever holds descriptive text, never executable
   code, which matters once a desktop app consumes it directly. Set its
   `category` to one of the values in the enum in `manual-tab.schema.json`
   — that is the group the app is listed under on the catalogue page. An
   app without one is listed under "Other" until a maintainer files it.
3. One appended entry in `apps-catalog.json`, matching
   `apps-catalog.schema.json`. Its `appId` must match the `appId` used in
   your `manual-tab.json` entry — that's how the two are linked. Its
   `version` starts at whatever you're releasing as — `0.1.0` for a first
   submission is the norm. Its `author` must be your GitHub login: the check
   enforces it, because it decides who may later edit this app's manual and
   version.

Touching anything else in this repo — including another contributor's
existing app — gets a submission rejected automatically.

### An app fix

A PR that only modifies one or more existing `apps/<name>.rs` files — a bug
fix, a refactor, or an API migration across several apps. It must not add,
remove or rename an app, and must not touch `apps-catalog.json` or
`manual-tab.json`; those changes go in their own PR. Every rule below still
applies to each app it touches, and each one gets its own build check.

You can fix someone else's app. The check flags it for the reviewer, who will
@-mention the app's author so they get a chance to look. Prefer one PR per app:
several fixes to the same app belong together unless they are genuinely
independent.

A fix does not bump the version. Versions are bumped in a [separate PR](#a-version-bump).

### A manual edit

A PR that only changes the content of **existing** entries in
`manual-tab.json` — a wording fix, restructuring the write-up, correcting a
description that no longer matches the code. It must not add, remove or
reorder entries, and an entry's `appId` and `title` must not change. Every
changed entry must still be complete (all required fields, at least one
channel with all its fields) and valid against `manual-tab.schema.json`.

Only the app's author or a maintainer may edit an app's manual entry. One PR
may change several entries as long as you are allowed to change each of them.

### A version bump

A PR that changes **only** the `version` field of **one or more** existing
entries in `apps-catalog.json`, and nothing else — no other field, no other
entry, no `apps/<name>.rs` or `manual-tab.json` changes. Each new value must
be a strict semver increase over the old one (`0.1.0` → `0.2.0` is fine;
`0.1.0` → `0.1.0` or `0.1.0` → `0.0.9` is rejected). Only the app's author or
a maintainer may bump it.

The version is what `fpapp build-community` stamps into the downloadable
package's filename and manifest, so it's how anyone can tell which build they
have. It is a label a person sets: nothing derives it from the code, so a
fix that merges without a bump is rebuilt and served under the old version
until the next release.

**Normally you don't write a bump PR.** Fixes and features land without
touching `version`, and the maintainer periodically runs the **Prepare
release** workflow, which finds every app with unreleased changes and opens
one `chore: prepare release` PR bumping all of them (see
[Releases](#releases)). That PR is a version-bump PR like any other and passes
the same check. You may still open a bump PR by hand as the app's author, but
look for an open release PR first: two bumps of the same app will conflict.

### A maintainer change

A PR that only touches files the maintainers own, and nothing in `apps/`,
`apps-catalog.json` or `manual-tab.json`:

`.github/`, `scripts/`, `tools/`, `Makefile`, `*.schema.json`,
`CONTRIBUTING.md`, `README.md`, `.gitignore`.

The list is the same as `.github/CODEOWNERS` — a change to any of these needs
a code-owner (maintainer) approval before it can merge. Anyone may propose
one; from a non-maintainer it passes the check with a flag saying a maintainer
must review it carefully. A change to a workflow deserves particular care: a
fork PR runs *its own* version of the workflow files.

## The rules (enforced mechanically)

These apply to every app file a PR adds or modifies.

- **Only the sanctioned API.** Everything — faders, buttons, LEDs, jacks,
  MIDI, storage/scenes — goes through `use crate::app::{...}`. No direct
  FRAM/flash access, no direct MAX11300 access, nothing that reaches around
  the facade.
- **No `unsafe`.** Not needed for anything the `App<N>` API doesn't already
  cover.
- **No `panic!()`/`unreachable!()`/`todo!()`.** The firmware halts the
  whole device on panic, not just one app's channels — this isn't a style
  preference, it's a hard rule. `.unwrap()`/`.expect()` aren't auto-banned
  (the official codebase uses them too), but add a comment justifying why
  the case is actually infallible.
- **No busy-loops.** Any `loop {}` needs a reachable `.await` inside it —
  Core 1 is cooperatively scheduled, so a loop that never yields starves
  every other app.
- **Declared parameters need a `ParamStore`.** If `CONFIG` adds any
  parameters, load them into a `ParamStore` and run its `param_handler()`
  alongside `run()`, as the existing apps do. Without it the Configurator can't
  show or change them, and they stay at the defaults compiled into the app.
- **A free ID in the 100+ range.** Community IDs are permanent once
  assigned, even if the app is later removed or adopted more officially —
  never reused, so saved layouts referencing it never silently break. IDs go
  to whichever submission merges first; the check catches a collision and the
  later PR picks another.

## Several PRs at once

- **Keep it to what a reviewer can hold in their head:** at most 5 open PRs per
  contributor, unless a maintainer agrees to more. This isn't enforced
  mechanically.
- **If PRs touch the same file**, the check lists the other open PRs that do.
  Say in each description which merges first; maintainers merge oldest first,
  and the later PR rebases. Submissions all append to the end of
  `apps-catalog.json` and `manual-tab.json`, so expect to rebase those after each
  merge.
- **A PR with no activity for 30 days** gets a ping, and is closed after 60 (it
  can be reopened).

## Releases

A release is cut by hand, by the maintainer:

1. **Actions → Prepare release → Run workflow**, on `main`. Tick *Dry run* first
   to see the plan in the job summary without pushing anything.
2. For every app, the workflow compares `apps/<name>.rs` with what it was when
   the app's current version was set. A difference means unreleased changes. A
   change that was reverted in between is not a change. **The version tracks the
   app's code, so a manual-only edit does not release an app:** the catalogue
   site always shows the latest manual, and the copy inside an installed
   `.fpapp` catches up the next time that app is released. Manual edits that
   landed meanwhile are still listed in the table for an app being bumped.
3. The bump is the highest of the commit subjects that touched the app's
   file: a breaking change (`type!:` or `BREAKING CHANGE`) is **major** (only
   **minor** while the app is `0.x`), `feat` is **minor**, and anything else
   is **patch**. PR titles are squash-merge
   subjects, so **write them as conventional commits** (`fix:`, `feat:`,
   `fix!:`).
4. The workflow commits the new versions to the `release` branch and opens,
   or updates, the `chore: prepare release` PR with a table of what changes.
5. GitHub doesn't run workflows for PRs opened by Actions, so **close and
   reopen the PR once** to run the required checks. (Storing a fine-grained
   PAT or GitHub App token as the `RELEASE_TOKEN` secret makes the PR open
   under that identity with the checks running straight away.)
6. The PR is a snapshot of the plan. If an app or the catalog is merged to
   `main` before it, **run Prepare release again** (the branch is
   rebuilt from `main`), then close and reopen the PR once more. Otherwise the
   check on the PR fails, or the release refuses to publish: a ZIP must never
   contain code its versions don't describe.
7. Merging it publishes a GitHub release `release-YYYY-MM-DD` with
   `faderpunk-community-apps-<tag>.zip`, which holds every app as a `.fpapp`
   built against faderpunk's current `main` (the commit is in the release
   notes), and the catalogue site redeploys with the new versions.

**New apps.** An app that isn't in the last published release (the latest
`release-*` tag) ships for the first time at its current version, without a
bump, and is listed as `new`. If its code changed after it was merged, it is
bumped like any other app instead: the catalogue site has offered that
version's download since the merge. New apps ride along in the next release
PR. If they are all there is, there is no version to bump, so Prepare release
opens no PR and says so in its summary: run **Actions → Release → Run
workflow** on `main` instead, with *Dry run* ticked first. That run refuses if
any app has a bump pending, builds the commit it runs on, and publishes the
release directly.

## Developing locally

The registration file (`apps/mod.rs` in `faderpunk`, generated by
`register_apps!`) is never part of a submission. The FPApp builder reads the
catalog and compiles the same source against the public app facade, so an app
does not need a second implementation or a permanent firmware edit.

1. Clone [`faderpunk`](https://github.com/ATOVproject/faderpunk) locally.
2. Add the app source and its catalog and manual entries here.
3. Run `make fpapps FADERPUNK_DIR=/path/to/faderpunk`. Install the resulting
   package from **Apps > Installed Apps** in the Configurator and test it on
   hardware.
4. Once it works, open a PR here with just the app file plus one entry
   each in `manual-tab.json` and `apps-catalog.json`.

The builder extracts the app's real `CONFIG` parameter definitions and packages
them with its manual and setup notes. Package files are build artifacts and are
not committed; reviewed source remains the source of truth.

## Before you open a PR

- `make fpapps` builds every app you touched.
- You tested the change on hardware (or say in the PR that you couldn't).
- The manual still matches what the app does — if the behaviour changed, say
  whether the manual edit is a separate PR.
- The PR is **one** kind from the table, with a matching conventional-commit
  title: `fix:`, `feat:` or `fix!:` decide the version bump at the next release.
- If it overlaps another open PR, the description says which merges first.

## Review and merge

Mechanical checks run first and catch most problems before a human looks:
scope, ownership, API boundary, panic/unsafe rules, catalog and manual
validity, and a real FPApp build of each touched app — the same
`make fpapps` you run locally. They roll up into one required status, `gate`,
which must be green to merge. *Hard-fails* block the PR; *soft-flags* are
notes for the reviewer and never block on their own.

A maintainer then reviews and merges. PRs are squash-merged with a
`type: summary (#N)` title, and the branch is deleted afterwards.

An AI first-pass review is planned to flag what those checks can't (misuse of
the API that's technically within bounds, a manual entry that doesn't match the
code). It is a filter, not the final word — a human maintainer always makes
the actual merge decision.

**A note on safety:** the build check compiles and runs submitted code (with a
read-only token and no secrets), so GitHub asks a maintainer to approve the
workflow run for a first-time contributor's PR before any of it runs.
