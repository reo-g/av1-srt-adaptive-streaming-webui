# CLAUDE.md

Web UI for a LAN video transmitter/receiver pair (送信機 TX / 受信機 RX). Each device runs one
daemon that serves this UI and the API; operators open it from a PC or tablet on the same LAN.
The devices must work fully offline after installation.

## Repository layout

- `web/` — React + TypeScript + Vite UI, plus `web/mock/` (a Node mock of the device API)
- `api/openapi.yaml` — the REST contract. `web/src/api/types.ts` mirrors it; keep both in sync
- `docs/architecture.md` — how the device side is meant to be built (daemon, media process, packaging)
- `docs/screens.md` — behaviour of every screen, decisions made, open questions
- `docs/terms.md` — UI vocabulary and how each word maps to the implementation
- `docs/api.md` — API overview, live channel, state machine, error and impact codes
- `design/screens/*.png` — the approved designs; `design/canvas/*.dc.html` — their sources (read-only)

## Commands (run in `web/`)

```
npm install
npm run mock        # mock device API on :8787 (MOCK_ROLE=tx|rx, MOCK_SCENARIO=normal|warn|error)
npm run dev         # UI on :5173, /api proxied to the mock (API_TARGET to point elsewhere)
npm run typecheck
npm run build       # type check + build + check:terms
npm run check:terms # fails if implementation terms appear in web/src or web/dist
```

In dev builds a small switcher (bottom right) changes the mock's role and scenario.

## Rules that must not be broken

1. **Do not reveal the implementation.** The transport protocol, the media framework, the
   bandwidth tool and their option names, error names and element names must not appear in the
   UI, the API (paths, field names, error codes), log lines shown to users, process names,
   thread names, file names on the device, package names or HTTP headers. Use the words in
   `docs/terms.md`. `npm run check:terms` must pass. Implementation names are fine only inside
   the daemon's private code and developer-only logs.
   The repository name is not a product name: never derive binary, package, service, host or
   file names on the device from it.
   During the prototype phase the device-side part of this rule (process, thread, file and
   package names on the device) is deferred; see "Prototype phase" below. The UI, the API,
   user-visible log lines and HTTP headers must stay neutral now.
2. **Offline.** No CDN, web font service, analytics or update check. Everything the UI needs is
   bundled into `web/dist` (the Geist font comes from `@fontsource-variable/geist`).
3. **The API contract is the boundary.** UI and daemon talk only through `/api/v1` as described
   in `api/openapi.yaml`. The API sends codes and numbers; Japanese text is produced in
   `web/src/i18n/ja.ts`. Change the contract in the YAML, `types.ts` and the mock together.
4. **State is never shown by colour alone.** ● ok ▲ warn ■ error ○ off — not measured, always
   with a text label (`web/src/components/Glyph.tsx`).
5. **Times are the device clock in JST** and labelled "JST".
6. **Destructive actions** (stop, discard changes) go through an alert dialog. Settings changes
   are drafts until "適用"; show what changes and its impact before applying.

## Building the device side (next step)

**Prototype phase (user decision 2026-11-04).** For now the daemon drives the existing media
programs (sender, encoder bridge, receiver, player; some are Python) as child processes and
passes their configuration through argv and environment variables. The rules "no argv/env",
"no Python on the device", "pipelines built in-process", "bandwidth test linked as a library"
and the hiding measures in `docs/architecture.md` §2 are deferred to the commercial phase.
Still required now: neutral names in the UI, API, user-visible logs and HTTP headers
(`npm run check:terms`), offline operation, no GPL encoders.

The target design for the commercial phase: follow `docs/architecture.md`. In short: one control daemon (HTTP + WebSocket, serves `web/dist`)
and one media process, both started by systemd under a dedicated user. The media process builds
pipelines through the framework's API (never a command-line launcher, never a pipeline string),
gives every element a neutral name, receives its configuration over a Unix socket (never argv or
environment), and maps statistics to the API's neutral fields. The bandwidth test is linked as a
library, not run as a command. Do not use GPL encoders in the daemon (see licences in
`docs/architecture.md`).

## Conventions

- UI text is Japanese and follows the existing tone; code, comments and commit messages in English.
- Keep the visual language of `design/screens`: tokens in `web/src/styles/app.css`, 14px base,
  4px radius, black accent on white.
- Screens not designed yet are placeholders (`web/src/pages/Placeholders.tsx`); do not invent
  final designs for them without asking.
