# M4 npm launcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `npm install -g @keelstackhq/hoist` works on the five release platforms the first time the
release workflow publishes, and the owner knows exactly what to set up on npm first.

**Starting point (checked 2026-10-22 on `main` at 8593a37):** the launcher was not deferred on
`main`. M3 Tasks 9 and 10 shipped it: `npm/hoist` (`bin/hoist.js`, `[publish] npm = { launcher =
true }`), the five `npm/hoist-<platform>` packages, `hoist release publish` (copies
`<assets>/hoist-<target>[.exe]` to `bin/hoist[.exe]` 0755, `npm publish --access public`,
platforms before the launcher), `hoist release bootstrap` (placeholders `0.0.0` and `npm trust
github`), and the `publish` job in `.github/workflows/release.yml` (environment `release`,
`id-token: write`, Node 24, npm 12.2.0). The "deferred to M4" note is from the superseded branch
`claude/friendly-hamilton-mn95h1`. What is missing is proof that the packed packages install and
run, an npm page for the launcher, a guard on the publish job's trusted-publishing setup, and the
owner's npm steps written down with the facts checked today.

**Architecture:** No Rust behaviour change. A node:test suite packs the real package dirs with
fixture release assets (a shell-script `hoist`, or the cargo-built binary when `HOIST_E2E_BIN` is
set), installs the tarballs offline into a temp project and runs `node_modules/.bin/hoist`. A Rust
test reads `release.yml` with `yaml_serde` and pins the publish job's trusted-publishing needs.

**Tech Stack:** No new crates or npm dependencies. Node ≥ 22 (`node --test`), npm (any version
for `pack`/`install --offline`).

**Spec:** §12 Distribution, §13 Facts.

## Global Constraints

- Never publish, tag or run the release workflow. Tests never reach a registry: `npm install
  --offline` with tarball paths only, `npm_config_registry` pointed at `http://127.0.0.1:9`.
- The real `npm/` dirs are never written by tests: they pack temp copies.
- Linux CI only (the one `ci` job); tests that need a POSIX shell script skip on Windows.

## Decisions

- D1 Scope: gap-closing on the shipped launcher, not a rewrite. Spec §14 milestone 4
  ("Integration": hull, D-004, rollout, `export --tofu`) is unchanged; this plan is named M4
  because the orchestrator scheduled the launcher as M4.
- D2 One `publish` job, as spec §12 says. A separate npm job would need the binaries and the
  GitHub Release state again; `hoist release publish` already orders GitHub Release → npm
  platforms → launcher → crates and skips what exists (R19 re-runs).
- D3 Provenance stays automatic: no `--provenance` and no `NPM_CONFIG_PROVENANCE`.
  `keelstackhq/hoist` is private on 2026-10-22, and npm does not generate provenance for private
  repositories; forcing it would fail the publish. When the repo becomes public, npm adds
  provenance itself.
- D4 The workflow keeps `actions/setup-node@v4` without `registry-url` and without `cache`: no
  `.npmrc` token placeholder, no package cache in a release build (npm's docs: "never use caching
  in release builds").
- D5 Fixture assets: the default fixture binary is a shell script that echoes its args to stdout,
  writes to stderr and exits 7; `HOIST_E2E_BIN` swaps in a real binary (CI passes
  `target/debug/hoist`) and the test then checks `hoist --version` prints the package version.
- D7 Scope narrowed by the orchestrator mid-run (2026-10-22): it confirmed the launcher shipped in
  M3 and that spec §14 M4 Integration waits for v0.1.0 and other repos, and asked for two more
  in-repo items in the same PR (Task 4). Tasks 1-3 were already done and stay.
- D6 Implementation done in the main session (two small tasks), review by a subagent.

## File Structure

- Create `npm/hoist/test/install.test.js` — pack, install offline, run.
- Create `npm/hoist/README.md`, `npm/hoist-<platform>/README.md` (5) — the npm pages.
- Modify `tests/self_release.rs` — publish-job guard.
- Modify `.github/workflows/ci.yml` — `HOIST_E2E_BIN` for the node tests.
- Modify spec §12/§13, `README.md`, `docs/PROGRESS.md`.

### Task 1: Install test against fixture release assets, package READMEs

- [x] Step 1: `install.test.js`:
  - `packs each platform package with its binary`: for every `npm/hoist-<platform>`, copy the dir
    to a temp dir, put the fixture asset at `bin/hoist[.exe]` (0755), `npm pack --json`; the
    tarball lists exactly `package.json`, `README.md`, `bin/hoist[.exe]`, and on POSIX the binary
    is executable in the tarball; `os`/`cpu` match `launcher.PLATFORMS`.
  - `launcher tarball holds only the launcher`: `package.json`, `README.md`, `bin/hoist.js` (no
    `test/`), `bin.hoist` is `bin/hoist.js`, versions of `optionalDependencies` equal its version.
  - `installed launcher runs the host platform's binary` (skip on Windows and on hosts outside the
    five): install the host platform tarball and the launcher tarball offline into a temp
    project, run `node_modules/.bin/hoist plan --json`: stdout `args:plan --json`, stderr has the
    fixture line, exit 7. With `HOIST_E2E_BIN`: `hoist --version` exits 0 and prints the version.
  - `installed launcher without its platform package exits 1 naming it`.
- [x] Step 2: run, watch the README assertions fail.
- [x] Step 3: add the READMEs (install, supported platforms, links; platform READMEs say "installed
  by @keelstackhq/hoist, do not depend on it directly").
- [x] Step 4: run, pass; CI sets `HOIST_E2E_BIN: target/debug/hoist` on the node step.
- [x] Step 5: commit `test: npm launcher installs and runs from packed release assets`.

### Task 2: Publish-job guard

- [x] Step 1: `tests/self_release.rs::publish_job_can_use_npm_trusted_publishing`: parse
  `release.yml`; job `publish` has `environment: release`, `permissions.id-token: write`, a
  `setup-node` step with `node-version` ≥ 22.14 and no `registry-url`/`cache`, an `npm install -g
  npm@X` with X ≥ 11.5.1, runs `release publish --assets dist`; no step or job env names
  `NODE_AUTH_TOKEN` or `NPM_TOKEN`; the file never says `--provenance`.
- [x] Step 2: run; it passes on the current workflow (a guard, not a fix). Break it locally once
  (remove `id-token`) to watch it fail, restore.
- [x] Step 3: commit `test: release workflow keeps npm trusted publishing requirements`.

### Task 3: Docs

- [x] spec §12 (READMEs, install test), §13 (2026-10-22 facts: private repo → no provenance;
  trusted-publisher configs expire unless used for a publish within 2 days; workflow field is the
  file name only; self-hosted runners unsupported), README Releasing (2-day window), PROGRESS
  (Status, Done, Blocked: npm owner steps).
- [x] Commit `docs: M4 npm launcher facts, owner steps and progress`.

### Task 4: README first publish via bootstrap, `ci-ok`

- [x] README "First publish and trusted publishers" points at `hoist release bootstrap` instead of
  the manual `0.0.0` edit-publish-revert steps.
- [x] `.github/workflows/ci.yml` job `ci-ok` (`needs: [ci]`, `if: always()`, fails unless `ci`
  succeeded): the required check name the other keelstackhq repos use, and the one hoist infers
  (spec §4). CLAUDE.md Layout.
- [x] Commit `ci: ci-ok aggregate check; docs: first publish via bootstrap`.

## Owner setup on npm (for the report and PROGRESS Blocked)

1. The npm org/scope `keelstackhq` exists and the owner's npm account can publish to it, with 2FA.
2. First publish is manual, as a placeholder: `hoist release bootstrap` (npm ≥ 11.15.0, `npm login`,
   a terminal) publishes `0.0.0` of `@keelstackhq/hoist` and the five
   `@keelstackhq/hoist-<platform>` packages. Never publish a real version by hand.
3. Trusted publisher on each of the 6 packages (bootstrap does it with `npm trust github`, or the
   web UI): provider GitHub Actions, organization `keelstackhq`, repository `hoist`, workflow
   filename `release.yml` (file name only), environment `release`.
4. Within 2 days of step 3, re-run the failed `publish` job (or run the release workflow), or the
   configurations expire and must be deleted and recreated.
5. Optional after the first CI publish: package settings → "Require two-factor authentication and
   disallow tokens".
