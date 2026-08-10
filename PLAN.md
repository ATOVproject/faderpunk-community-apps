# Faderpunk App Store — Roadmap

## Context

Faderpunk currently ships one firmware image containing all 27 built-in apps
(`faderpunk/src/apps/mod.rs:1-29`, wired up by the `register_apps!` macro in
`faderpunk/src/macros.rs`). The goal is a way for users to select which apps
end up on their device — each app described by its `app.rs` plus a manual
entry — including apps that aren't officially maintained by the project.

Research into the current architecture ruled out dynamic runtime loading of
new app code: the firmware is `no_std` with no heap allocator, apps are
const-generic modules resolved by a macro-generated `match`
(`macros.rs:36-58`), and Embassy task pools are sized statically per app at
compile time. There's no vtable/fn-pointer ABI to hang a plugin system off
without a from-scratch rewrite. So the approach is: **selection →
custom-compiled firmware → user flashes the resulting UF2**, same as any
firmware update today.

Kept deliberately simple per direction from this session:

- **No backend service, no cloud build, no configurator changes.** A local
  CLI tool does the building, on the user's own machine. This avoids any
  standing infrastructure, credential custody, or artifact-hosting surface.
- **No configurator integration for community apps.** The community catalog
  and manual live entirely in the community repo and the CLI tool. The
  configurator keeps working exactly as it does today, untouched by this
  plan.
- **Community apps aren't tied to `faderpunk`'s release schedule.** They
  live in their own repo, merged independently, available to the CLI the
  moment a PR lands — no firmware release required to add or update one.
- **PR-gating mechanism reuses, rather than reinvents,** the diff-based
  scope-check approach already designed for `faderpunk` itself (a separate,
  parallel effort, same author) — same core technique (GitHub API diff
  inspection, no fork checkout, safety-API-bypass detection via grepping
  added lines in the `patch` field), adapted to a repo that contains
  nothing but apps.

## Repo layout

- **`faderpunk`** (existing) — read-only for Phases 1–4: the CLI only reads
  from a local clone (parses the existing `register_apps!` list, runs the
  existing build path) and never commits, pushes, or opens a PR against it.
  Its own external-PR scope policy is being built separately (see reference
  above) and isn't part of this plan either. **Possible future exception**:
  Phase 5's optional, not-yet-decided flash-from-the-app feature would need
  one small, additive firmware change (a new protocol message) — called out
  explicitly there, undecided, rather than silently breaking the
  "unchanged" rule that otherwise holds throughout.
- **`faderpunk-community-apps`** (new) — contains *only* community app
  `.rs` files, their manual entries, and a catalog file. Nothing else. PRs
  here are merged on their own schedule, independent of `faderpunk`
  releases.
- **`faderpunk-store`** (new, its own standalone repo — not a crate added
  to `faderpunk`) — the CLI (Phase 3), the only thing that ever produces a
  custom UF2 today. Runs entirely on the user's machine, mutating only its
  own local working copy/checkouts, never the upstream repos. Structured
  internally so its clone/catalog/build logic is a clean library, not
  tangled into the binary — not because a second consumer is committed to
  yet, but so it isn't a rewrite if one shows up later.

**New repos: 2 now** — `faderpunk-community-apps` and `faderpunk-store`.
Phase 5's desktop app is still optional/undecided; if it happens, it's a
**third** repo depending on `faderpunk-store`'s library logic as a git
dependency, not a workspace member of it — keeping the CLI a complete,
independent tool on its own.

## Phase 1 — Community apps repo

- Layout: `apps/<name>.rs` (exact same pattern as official apps —
  `CHANNELS`, `CONFIG`, `wrapper`, `run`, per AGENTS.md's "Creating a New
  App" — no new app API surface), `docs/<name>.md` (the manual entry),
  `apps-catalog.json` (one entry per app: `appId`, `module`, `author`,
  `manual` path).
- The catalog lives *only* in this repo — no mirrored/official catalog file
  in `faderpunk`, and nothing for the configurator to fetch. The CLI (Phase
  3) reads official app id/module pairs directly from the existing
  `register_apps!` call in `faderpunk/src/apps/mod.rs`, so there's no
  second source of truth to keep in sync for official apps either.
- Reserve `100+` for community `appId`s (official stays `1–99`), validated
  by catalog schema checks in this repo's own CI.
- Anyone can open a PR here at any time; a merge is immediately usable by
  the CLI on its next pull — no coupling to `faderpunk`'s release cadence.

## Phase 2 — Automated submission safety gate

A PR to `faderpunk-community-apps` runs a layered, mostly-mechanical gate
before a human ever needs to look at it — adapted from the mechanism being
built for `faderpunk` itself (see reference above), not reinvented:

- **Fetch via GitHub API, never checkout fork code.**
  `gh api repos/.../pulls/<n>/files` gives filename/status/additions/
  deletions and the unified-diff `patch` text for each file — enough to
  classify and grep without ever cloning or executing the submitter's code.
  Safe on forks' default read-only `GITHUB_TOKEN`; verdict written to
  `$GITHUB_STEP_SUMMARY` (no write permission required).
- **Path-scope hard-fail.** Since this repo contains nothing but
  `apps/`, `docs/`, and the catalog, the rule collapses to: a PR may only
  add one new `apps/<name>.rs`, one new `docs/<name>.md`, and append one
  entry to `apps-catalog.json`. Touching anything else — including another
  contributor's existing app — hard-fails.
- **Safety-API-bypass hard-fail**, carried over directly: scan added
  (`+`-prefixed) lines of the app file's patch for `crate::storage::`
  imports, raw system-storage function names, `MAX_CHANNEL`/`MaxCmd`/
  `MaxSender`, or `crate::tasks::max` — everything must go through the
  `App<N>` facade (`use crate::app::{...}`) for faders, buttons, LEDs,
  jacks (MAX in/out), MIDI, and storage/scenes. No memory (FRAM/flash) or
  MAX I/O access outside those API calls. `crate::`, not a dependency path
  — see the build-check note below on why. If the patch is too large for
  GitHub to return, don't silently pass — soft-flag "couldn't statically
  verify, needs manual review."
- **Panic/unsafe hard-fails, scoped to what's actually unambiguous**: any
  `unsafe` block or function (hard-fail); `panic!()`/`unreachable!()`/
  `todo!()` anywhere (hard-fail — the firmware builds with
  `panic_immediate_abort`, so one bad community app can hang the whole
  device, not just its channel). `.unwrap()`/`.expect()` are **not** a
  blanket hard-fail — the codebase itself uses them (`macros.rs:51`,
  several call sites in `lfo.rs`), so banning them outright would be
  unusable and the first contributor would rightly point at existing code.
  Instead: soft-flag any `.unwrap()`/`.expect()` without an adjacent
  justification comment, for human judgment. A `loop {}` with no reachable
  `.await` is also a hard-fail (starves the cooperative scheduler for
  every other app).
- **ID/catalog validation.** `appId` unused and in the `100+` range, entry
  schema-valid, `author` set, referenced manual file exists. Community IDs
  are permanent — if an app is later adopted as effectively official, only
  its tier/metadata changes, never its numeric ID, so existing saved
  layouts referencing it never break.
- **AI agent first-pass review (advisory, not a merge gate).** Runs only
  once the mechanical checks pass. Given the diff and the written rules,
  flags anything the mechanical checks can't — code that stays within the
  `App<N>` API but misuses it, obfuscated logic, a manual entry that
  doesn't match the code. A human maintainer still makes the actual call.
- **Build/compile check.** `faderpunk` is a `#![no_std]`/`#![no_main]`
  binary crate with no `[lib]` target — nothing can depend on it as a
  library, so this can't be "pull it as a dependency." Instead CI does
  exactly what the Phase 3 CLI does: clone `faderpunk` at a pinned tag,
  copy the submitted file into `apps/community/`, generate a solo-app
  `register_apps!` containing just this one app, and build the real
  binary — confirming it compiles standalone against the actual `App<N>`
  API — plus `cargo fmt`/`clippy -D warnings` on the submitted file. This
  clone-and-inject step is the same logic the CLI needs anyway, so it's
  worth sharing rather than reimplementing.
- **Local dev loop for contributors**: since the path-scope rule forbids
  touching `apps/mod.rs`/`register_apps!` (that's generated, never
  hand-submitted), a contributor develops against their own local
  `faderpunk` clone with a throwaway local registration edit to build and
  flash while iterating, then submits only the app file, manual entry, and
  one catalog line — never the registration itself.

## Phase 3 — Local build CLI tool

A small CLI (e.g. `faderpunk-store`) a user downloads and runs locally —
the only thing that ever compiles a custom firmware, and the only thing
that ever needs the community catalog:

1. Clones/updates a local checkout of `faderpunk` (pinned to the latest
   release tag by default, overridable) and `faderpunk-community-apps` at
   its current `main` — always the latest merged state, so new community
   apps show up the moment they're merged, with no firmware release in the
   loop.
2. Reads official app id/module pairs straight from `faderpunk/src/apps/
   mod.rs`'s existing `register_apps!` call — **always all of them, with
   no option to leave one out**; the picker only ever chooses *optional*
   community apps to add on top, which simplifies the UI to a single
   "add extras" prompt (or a flag for scripting, e.g. `faderpunk-store
   build --apps my_community_app`) instead of a two-part selection.
3. Copies selected community app files into `faderpunk/src/apps/` under a
   collision-safe renamed filename (`community_<module>.rs` — not a
   `community/` subdirectory: `register_apps!` itself does `mod $app_mod;`
   for every entry, so a separate `#[path = ...]` pre-declaration would
   just collide with that; renaming the file instead needs no changes to
   `macros.rs` at all) and generates `faderpunk/src/apps/mod.rs` as
   `register_apps!(id => module, ...)` — every official entry, plus one
   renamed entry per selected community app.
4. Runs the existing local build path (wraps `build-uf2.sh`: `cargo build
   --release` + `picotool uf2 convert`), producing a UF2 named with a short
   hash of the selected app-ID set for traceability. If the build fails on
   a `flip-link`/linker overflow (RAM, not flash, is the more likely
   ceiling — each app carries a static Embassy task pool and Core 1 has a
   131 KB stack), surface "this app selection doesn't fit" rather than a
   raw linker dump.
5. Checks/guides toolchain prerequisites (Rust nightly + target +
   `flip-link` + `picotool`; prefers `devenv`/`direnv` per AGENTS.md, falls
   back to plain `rustup`/manual instructions).

**Saved device state across different app selections**: confirmed safe by
reading `storage.rs` directly — per-app FRAM records are addressed by
`layout_id` (channel slot), not `app_id`, and each record self-tags its
`app_id` in the first byte, validated on load (`data[0] != self.app_id` →
treated as absent). So flashing a build with a different app set than a
previous one doesn't misread stale data as belonging to the new app —
worst case, a slot's saved scene/params silently reset to defaults if the
app occupying that slot changed between builds.

Trade-off versus a cloud build, accepted deliberately: users need a local
toolchain and the build runs on their own machine. In exchange there is no
server anywhere in the pipeline, and `faderpunk` itself is never touched
(previous section).

### Limitations, named plainly

- **Toolchain burden.** Users need Rust nightly + the `thumbv8m.main-none-
  eabihf` target + `flip-link` + `picotool` (which itself needs `libusb`/
  `cmake`/build-essential to build, per `beta.yml`). Real friction for
  eurorack hobbyists who aren't Rust developers — the main cost versus a
  "click a button, get a file" flow.
- **Not actually zero-network.** "Local" means no server *we* host, not
  "works with no connection ever" — the first build (and any cache-cleared
  rebuild) needs network to clone both repos and fetch ~30 crates.io
  dependencies.
- **Build time.** Release profile uses LTO + `codegen-units=1` — several
  minutes on a first build. Local cargo caching speeds up rebuilds, but
  there's no shared cache across users the way CI gets.
- **Cross-platform packaging.** The CLI has to work on macOS/Linux/Windows,
  and `picotool`'s own build chain varies by OS — genuine engineering
  effort, not just a shell script.
- **RAM, not flash, is the likelier ceiling on a large selection.** Flash
  headroom is generous (2–4 MiB, 27 apps today), but each selected app adds
  a statically-sized Embassy task pool against a 131 KB Core-1 stack — a
  solo-app CI compile check (Phase 2) won't catch an overflow that only
  shows up once a user selects many apps together.
- **No build-time sandboxing.** The automated gate (Phase 2) is the only
  safety net; anything that slips through compiles on the *user's own
  machine*, not a disposable CI runner. Same trust model as any `cargo
  install` of a third-party crate, but worth stating plainly rather than
  implying the review gate is a hard guarantee.
- **No polished discovery UX.** Browsing is a terminal picker or reading
  GitHub directly — not a graphical store front.
- **No centrally-verified binary.** Every custom UF2 is a local one-off
  build; there's no shared checksum a maintainer can point to as "known
  good" the way an official release has.

## Phase 4 — Community manual

- The manual entry for each community app lives in `manual-tab.json` in
  `faderpunk-community-apps`, one appended entry per app — matching the
  shape of the official configurator's `ManualAppData`
  (`configurator/src/components/manual/ManualApp.tsx`: title, description,
  icon, color, params?, storage?, long-form `text`, per-channel jack/fader/
  LED docs), so the desktop app (Phase 5, if built) can render community
  manuals with the same component/look as official ones. Stored as plain
  JSON **data**, not `.tsx` source — deliberately not executable code,
  since a desktop app importing untrusted `.tsx` as a module would be a
  real code-execution risk a JSON parse doesn't have. Replaces an earlier
  `docs/<name>.md`-per-app design entirely (superseded, not kept
  alongside).
- `apps-catalog.json` entries no longer carry a `manual` path — the link
  is the shared `appId` between a catalog entry and its `manual-tab.json`
  entry.
- The CLI (Phase 3) surfaces the same data locally as plain text (there's
  no rich renderer in a terminal) — a short blurb per app in the
  interactive picker, and a `faderpunk-store info <module>` command for
  the full entry — so a user never has to leave the tool to read what an
  app does before selecting it.
- No changes to the *hosted* configurator, and no separate docs site or
  custom renderer — Stage 6 gets a rich preview by building the real
  configurator from a local clone with `manual-tab.json` injected into
  `ManualTab.tsx` before the build, exactly like the CLI already injects
  into `apps/mod.rs`. The upstream repo is never touched.

## Phase 5 — Standalone desktop app (Configurator + App Store)

A consequence of already building local tooling, not a strict requirement
of the app-store work above, but the natural place to put a "real program,
not a browser tab" experience with an App Store feature the hosted
configurator deliberately doesn't have.

**Transport correction found while designing this phase:** `origin/main`
has already moved the configurator off WebUSB entirely — commit `6c8fdd16`
("move configurator protocol from WebUSB to MIDI SysEx", #590) replaced
`usb-protocol.ts` with `midi-protocol.ts`/`sysex.ts`
(`navigator.requestMIDIAccess({ sysex: true })`, wrapping postcard bytes in
a custom `F0 7D 46 50 01 ... F7` SysEx envelope). Older docs describing the
old WebUSB/COBS design are stale. This matters directly for packaging,
since Web MIDI has real support outside Chromium (Firefox since 108,
Safari since 17, including sysex), unlike WebUSB.

**Packaging: Tauri, not Electron.** Given Web MIDI's broader support, Tauri
(Rust-native, ~3–10MB binaries, no bundled Chromium) is viable and a much
better fit for a team already investing in Rust for firmware + the Phase 3
CLI than Electron's 150–200MB bundle would be:
- Windows (WebView2 = real Chromium) and macOS (WKWebView tracks Safari
  17+) are very likely fine for `navigator.requestMIDIAccess`.
- **Linux (WebKitGTK) is the one real unknown** — it lags Apple's WebKit on
  newer web-platform features, and Web MIDI+sysex support there isn't
  something to assume. Spike this first, but test the right thing: API
  *presence* isn't the actual risk, sysex *permission* is — a browser
  prompts the user for `{ sysex: true }`, but an embedded webview may have
  no permission-prompt UI at all and just silently deny where a real
  browser would ask. So the spike's pass/fail bar is "a minimal Tauri
  window completes a real `GetVersion` round-trip against physical
  hardware," not "the API exists," on both Linux and macOS before
  committing further. If it fails, fall back to launching system Chrome in
  `--app=` mode (chromeless window, real Chromium, guaranteed to work) as
  a per-platform exception rather than abandoning Tauri everywhere.

**Structure — two views, only one of which touches `faderpunk` even
read-only-wise:**
- **Configurator view**: pulls `configurator/` source fresh from the local
  `faderpunk` checkout and builds it (`pnpm build`) exactly as it exists
  upstream — zero modifications, so `faderpunk` stays untouched by this
  plan just like everywhere else. Cache the built output locally and only
  rebuild when the pinned commit/tag actually changes, rather than on every
  launch.
- **App Store view**: new UI, owned by its own repo (a third one, separate
  from both `faderpunk` and `faderpunk-store`) — this is the extra feature
  the hosted configurator intentionally doesn't have. Depends on
  `faderpunk-store`'s clone/catalog/build logic as a git dependency rather
  than reimplementing it, so `faderpunk-store` stays the one place that
  logic is maintained.
- **"Pulls what it needs, when it needs it"**: on launch or on-demand
  refresh, re-pull the community catalog (cheap — small JSON) and check
  whether `configurator/`'s pinned commit changed (rebuild only if so).
  The heavy step — an actual firmware compile — stays purely on-demand,
  triggered only when the user picks apps and hits build, exactly like the
  CLI.

### Flash directly from the app (optional — undecided)

Not committed to yet; still weighing it. If pursued, it's the one
deliberate, scoped exception to `faderpunk` staying untouched elsewhere in
this plan — small and additive, doesn't change any existing behavior. If
not pursued, Phase 5 just ends at "produces a UF2," same as the Phase 3
CLI, and the user does the existing manual SHIFT+connect+drag flow —
already a known, working process, so skipping this sub-feature costs
nothing structurally:

1. **New protocol message**: `ConfigMsgIn::RebootToBootloader` (naming
   TBD), handled firmware-side by calling `embassy_rp::rom_data::reboot()`
   with the BOOTSEL flag. Confirmed this exists in the pinned dependency —
   `embassy-rp = "0.6.0"` in `faderpunk/Cargo.toml` already vendors
   `rom_data::rp235x::reboot(flags, delay_ms, p0, p1)` (RP2350 datasheet
   §5.5.10.1) as a safe wrapper; RP2350 replaced RP2040's dedicated
   `reset_to_usb_boot()` with this more general reboot call. The exact flag
   value for "reboot into BOOTSEL/USB mode" needs confirming against the
   datasheet at implementation time, but the capability itself is real and
   already available, not something requiring a new HAL dependency. Per
   AGENTS.md, a new protocol message means running `./gen-bindings.sh`
   after adding it to `libfp`.
2. **App-side flow**: send `RebootToBootloader` over the existing Web
   MIDI/SysEx transport, wait briefly for the device to re-enumerate in
   BOOTSEL/PICOBOOT mode, then run `picotool load <uf2>` — the same
   `picotool` dependency Phase 3 already needs for UF2 conversion, now
   reused for flashing too. No mounted-drive detection or file-copy needed;
   `picotool` talks to a BOOTSEL-mode device directly over USB.
3. **Fallback, exactly as proposed**: if the automatic reboot command times
   out (firmware predating this feature, or an unresponsive device), fall
   back to the existing instructions — hold SHIFT, connect USB — which
   users already know from normal updates. Either way, once the device is
   in BOOTSEL mode, the app finishes with the same automated `picotool
   load` step, so manual drag-and-drop is never actually required, only
   manual bootloader entry in the fallback case.
4. **Bricking risk is not meaningfully higher** than today's manual
   process: RP2350's BOOTSEL bootloader lives in immutable on-chip mask
   ROM, so a failed/interrupted flash just leaves the device back in
   BOOTSEL, retryable — not a state that requires special recovery
   hardware.

## Execution stages

The Phases above describe the architecture by feature area; this section
sequences it into concrete, shippable stages — each with its own exit
criterion, so a stage is either done or it isn't, and later stages don't
block on earlier ones being "perfect."

**Stage 1 — CLI MVP, factory apps only.** ✅ Done, hardware-verified.
Create `faderpunk-store` (`core` library + `cli` binary, per the repo
layout above). Clone `faderpunk`, parse the existing `register_apps!` list,
regenerate it unchanged (all factory apps, always — no picker yet since
there's nothing optional to pick), run the existing build path, produce a
hash-named UF2. Covers Phase 3's mechanics minus the community catalog.
*Exit criterion, met*: the regenerated all-factory-apps build boots on real
hardware and `GetAllApps` matches today's default firmware exactly.

**Stage 2 — Community repo scaffold, no automation yet.** ✅ Done. Created
`faderpunk-community-apps`: the `apps/`+`apps-catalog.json` layout, schema,
`100+` ID reservation, contribution guide. Covers Phase 1.
*Exit criterion, met*: convention documented and schema-checkable.
Deliberately **not** opened to public PRs yet — human-only review of
arbitrary Rust isn't something to commit to publicly before Stage 4's
automated gate exists; treat it as internal/soft-launch until then.

**Stage 3 — Wire the CLI to the community catalog.** ✅ Done,
hardware-verified — including the same-module-name-as-a-factory-app
collision case (proves the renaming actually avoids the collision, not
just avoids triggering it). Extended the CLI to also clone
`faderpunk-community-apps` and offer a picker over *optional community
apps only* (factory apps are never optional, per the product decision
above), copying selected ones in under a renamed, collision-safe filename
and extending the generated `register_apps!` to include them alongside
every factory app. Covers the rest of Phase 3.
*Exit criterion, met*: a firmware with all factory apps plus at least one
community app builds and boots correctly on hardware.

**Stage 4 — Automated safety gate.** ✅ Built and tested locally (not live
— repo unpushed, AI review unwired). Layered CI checks in
`faderpunk-community-apps`: path-scope, API-boundary diff-grep, panic/
unsafe rules, catalog validation, **manual-tab.json validation** (added
retroactively when Stage 5 replaced `docs/<name>.md` — appends-only,
exactly one new entry, required fields present, `appId` cross-checked
against the catalog's new entry), the clone-and-inject build check. AI
first-pass review deliberately left as a documented TODO, not faked.
Covers Phase 2.
*Exit criterion, met*: 10/10 local fixture tests pass (one clean, one per
hard-fail rule including the two manual-tab-specific ones, plus the
unwrap soft-flag case); the build check ran a real cold build against a
POC app and produced a working binary. **This is the gate that turns the
repo public** — don't take outside contributions before it's actually
pushed and live.

**Stage 5 — Manual entries in the CLI.** ✅ Done. Added
`manual-tab.json`/`manual-tab.schema.json` to `faderpunk-community-apps`
(replacing the earlier `docs/<name>.md` design entirely), seeded all 27
POC entries with their *real* manual text pulled straight from
`origin/main`'s `ManualTab.tsx` (not stub blurbs). CLI gained an `info
<module>` command plus a description blurb per app in the interactive
picker. Covers Phase 4 — official apps were never in scope here since
factory apps aren't picked at all and don't have entries in this system.
*Exit criterion, met*: `faderpunk-store info euclid` prints the real
Euclid manual (title, params, storage, long-form text, per-channel jack/
fader/LED docs) matching `origin/main`'s actual data; `build` still
produces a valid UF2 with the new data-loading path wired in.

**Stage 6 — Rich manual preview (standalone CLI feature).** ✅ Done.
Independent of Stage 7 — doesn't need Tauri, Web MIDI, or any desktop-app
work, so it shipped without waiting on that stage's fate. Originally
scoped as a full custom "viewer" — a separate React package faithfully
porting `ManualApp.tsx` plus its dependency chain (`Icon`,
`COLORS_CLASSES`, `Md`, `Shared`, 28 font files, 42 icon SVGs, the
Tailwind v4 theme block). Replaced by a much smaller design: reuse the
real configurator build as-is and inject one small addition into the
local clone before building it — the same pattern the CLI already applies
to `apps/mod.rs`.

`faderpunk-store preview`:
1. Clones `faderpunk` (Stage 1's logic) and reads `manual-tab.json` from
   the community checkout.
2. In the *local clone only* (never upstream), edits
   `configurator/src/components/ManualTab.tsx`: adds `const communityApps:
   ManualAppData[] = <data>` (literally the raw JSON — valid JSON is valid
   TS object-literal syntax), adds one import (`ManualApp`), and inserts a
   JSX block after `<Apps apps={apps} />` — a heading plus a `.map()` over
   `communityApps` rendering `<ManualApp>` per entry. Bypasses `Apps.tsx`'s
   wrapper entirely (its static shared-conventions copy is about *official*
   app behavior, doesn't apply to community apps). **Second injection
   point, found during manual review**: the nav "jump to app" list near
   the top of the page is separate markup from where apps render —
   community apps rendered correctly in their own section but never
   appeared in that nav list until this was added too.
3. Runs `pnpm build`/`pnpm dev` on the modified local clone and serves it.

Gets pixel-identical rendering for free — same component, same `Icon`,
same theme, same fonts, zero new npm dependencies, zero assets to keep in
sync with upstream.
*Exit criterion, met*: real end-to-end run — clone, inject, `gen-bindings.sh`,
`pnpm install`, `pnpm dev` — confirmed clean (zero transform errors) and
the served module verified via curl to contain the injected heading, the
nav entry, and all 27 community `appId`s.

**Stage 7 — Standalone desktop app.** *Optional — needs an explicit
go/no-go from the team first.* Depends on Stage 1's `core` library being
usable as a git dependency. New repo; the very first deliverable is the
Tauri spike itself (Linux + macOS Web MIDI/sysex round-trip against real
hardware) as a hard gate before building anything further. If it passes:
Configurator view + App Store view on top of `faderpunk-store`'s `core`.
Covers Phase 5 minus flash-from-the-app. The App Store view's manual
display can reuse Stage 6's injection approach directly rather than
reimplementing anything.
*Exit criterion*: spike passes on both platforms; Configurator view reads/
writes real device config; App Store view produces a UF2 identical to the
CLI's output for the same selection.

**Stage 8 — Flash directly from the app.** *Optional — needs an explicit
go/no-go; this is the only stage that touches `faderpunk` itself.* Depends
on Stage 7 (or minimally Stage 1, if wanted in the CLI without a desktop
app) plus a team decision to pursue it at all. Adds the
`ConfigMsgIn::RebootToBootloader` protocol message and firmware handler,
then CLI/desktop-app integration via `picotool load`, with fallback to
manual SHIFT+connect.
*Exit criterion*: on real hardware, `RebootToBootloader` lands the device
in BOOTSEL and `picotool load` completes and boots the new firmware; the
fallback path is separately verified against firmware that predates the
new message.

## Note: mechanical checks vs. a compiler-enforced boundary

The gate in Phase 2 is enforced in CI, not by the Rust compiler — once a
community app is pulled into the `faderpunk` crate by the CLI, it
technically *can* reach crate-internal hardware modules. `tb3po.rs` was
once a live example of a direct `crate::tasks::leds::LedMode` import
bypassing the facade — since fixed (`LedMode` re-exported from `app.rs`,
plus a new AGENTS.md rule that new app-facing types must be re-exported in
the same change that introduces them), but that's a convention, not a
compiler guarantee. A stronger-than-CI guarantee would mean splitting apps
into their own crate depending only on a slim, published `app.rs`-
equivalent public API — turning boundary violations into compile errors.
Flagged as a possible future hardening step, not part of this plan.
