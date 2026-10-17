# hoist M3 (releasing) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Merging the release PR ships the repo's packages. `hoist release plan` shows the next versions. `hoist release prepare` opens and keeps the release PR up to date. `hoist release publish` tags, creates the GitHub Release with binaries and checksums, publishes to npm and crates.io through OIDC trusted publishing, deploys `on = "release"` targets, and verifies the result. hoist then releases itself this way.

**Architecture:** The layers stay as they are: inference → live read → pure diff → typed changes → executors. M3 adds:
- a release model: packages with versions, an internal dependency graph and `[publish]` settings;
- a pure version planner: conventional commits since each package's last tag become a bump;
- file editors for manifests, lockfiles and changelogs;
- two executors. `prepare` drives git and the GitHub pulls API. `publish` drives the GitHub refs and releases APIs and the `npm`/`cargo` CLIs.

`publish` is stateless: a package is pending when its manifest version has no tag. Every step checks before it writes, so a re-run is always safe. External tools run through env-overridable paths, so tests replace them with recording stubs.

**Tech Stack:** The M2 crates stay unchanged. New: `sha2 =0.11.1` (checked on crates.io on 2026-10-17; released 2026-10-12), for `SHA256SUMS`. Hex encoding and semver parsing are written by hand: only `MAJOR.MINOR.PATCH`, and pre-release versions are refused with exit 3. Ask before adding anything else. External tools at runtime are `git`, `npm` ≥ 11.5.1 with Node ≥ 22.14.0 (needed for the OIDC exchange), `cargo` and `pnpm`.

**Spec:** `docs/superpowers/specs/2026-10-16-hoist-design.md` (§3 CLI, §4 inference, §5 `[release]`/`[publish]`, §6 npm and crates rows, §9.1–9.5, §11, §12, §13).

## Global Constraints

- All M1 and M2 constraints stay in force:
  - exit codes `0/1/2/3`;
  - the one-object `--json` shape;
  - actions are `create|update|ok|drift|pending|unknown`;
  - `rustls` only and `#![forbid(unsafe_code)]`;
  - secret values only in `Secret`, never in argv, output or logs;
  - tests never reach a real API or registry.
- Read-only commands: `explain`, `plan`, `check`, `auth check`, `import`, `release plan` and `release publish --dry-run` send only GETs, run no tool that writes, and change no file or git ref. Each one has an `assert_only_get` test, plus a check that `git status --porcelain` and `git tag` are unchanged.
- Write commands:
  - `release prepare` writes only the branch `hoist/release` and its PR;
  - `release publish` writes only tags, GitHub Releases and assets, registry publishes and `on = "release"` deploys.
- Exit 3 only happens before the first write (R18/R23 from M2 carry over).
- API and registry bases come from env:
  - `HOIST_GITHUB_API`;
  - `HOIST_GITHUB_UPLOADS` (default `https://uploads.github.com`);
  - `HOIST_NPM_REGISTRY` (default `https://registry.npmjs.org`);
  - `HOIST_CRATES_API` (default `https://crates.io`).
  Every crates.io request sends `User-Agent: hoist/<version> (+https://github.com/keelstackhq/hoist)`; crates.io answers 403 without one. Scoped npm names go into URLs as `@scope%2fname` via `seg()`.
- Tool paths come from env: `HOIST_GIT` (default `git`), `HOIST_NPM`, `HOIST_CARGO`, `HOIST_PNPM`. Stdout and stderr of tools go to hoist's stderr. `--json` stdout stays one object.
- Names:
  - release branch `hoist/release`;
  - release PR title `release: <name>@<version>, …`;
  - tag format from `[release] tag_format`, default `{name}@{version}`, or `v{version}` for a single-package repo;
  - changelog `CHANGELOG.md` in each package directory;
  - pins and skips in `.hoist/release.toml`.
- Versions are `MAJOR.MINOR.PATCH`:
  - `fix` → patch, `feat` → minor, `!` or `BREAKING CHANGE:` → major (minor below 1.0.0);
  - `docs`, `ci`, `chore`, `test`, `style`, `refactor`, `build` and `perf` alone release nothing;
  - an internal dependent of a bumped package gets a patch bump;
  - a package with no tag yet releases its manifest version unchanged.
- `publish` only runs when all of these hold:
  - `GITHUB_ACTIONS=true`;
  - `GITHUB_REF` is `refs/heads/<default branch>`;
  - `HEAD` equals `GITHUB_SHA`;
  - the running job (`GITHUB_WORKFLOW_REF` + `GITHUB_JOB`) declares `environment: <[release] environment>`.
  Otherwise it exits 3 naming the failed condition. `--dry-run` skips these checks.
- GitHub Releases are created as drafts. All assets and `SHA256SUMS` are uploaded, then the release is published with `PATCH {"draft": false}`, which works with immutable releases. Tags are lightweight refs on `GITHUB_SHA`.
- Fast checks before every commit: `cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test`.

## Facts this plan relies on (research 2026-10-17, see Task 11 for §13)

- **npm trusted publishers.**
  - They can be created only by a logged-in user with 2FA: `npm trust github <pkg> --file <workflow> --repo <o>/<r> --env <env> --allow-publish` (npm ≥ 11.15.0), or the web UI. A CI token cannot create or read them.
  - The package must already exist, so its first publish is manual.
  - `npm publish` (≥ 11.5.1) performs the OIDC exchange by itself when the job has `id-token: write`. Provenance is automatic for public repos with public packages.
  - Whether `pnpm publish` supports OIDC is unverified, so hoist calls `npm publish`.
- **crates.io trusted publishing.**
  - It is stable, and the crate must already exist, so its first publish is manual.
  - Token exchange: an Actions OIDC JWT with audience `crates.io` goes to `POST {crates}/api/v1/trusted_publishing/tokens` with body `{"jwt": …}` (field name from the official action's source, unverified). The response holds a short-lived token, which is revoked after use.
  - The configs API exists in source only, so hoist makes publisher registration a human step.
- **Registry reads.**
  - `GET {npm}/<pkg>/<version>` answers 200 when the version exists and 404 otherwise.
  - `GET {crates}/api/v1/crates/<name>/<version>` answers 200 when the version exists and 404 otherwise, and needs a User-Agent.
  - `GET {npm}/<pkg>` and `GET {crates}/api/v1/crates/<name>` answer 404 when the package does not exist.
- **Actions OIDC.** Request `GET $ACTIONS_ID_TOKEN_REQUEST_URL&audience=<aud>` with `Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN`; the response is `{"value": "<jwt>"}`.

## Review Focus

1. **A publish job is re-run after a partial failure** (the tag and release exist, npm is done, crates failed). The second run skips every finished step and only publishes the crate. Test in Task 8.
2. **A package that was never published** (the registry answers 404 for the package). `publish` does not try OIDC. It reports `pending` with a human step for a manual first publish and exits 2, and the other packages still publish. Test in Task 8.
3. **A shallow clone or missing tags** (`actions/checkout` without `fetch-depth: 0`). `release plan`/`prepare` exit 3 with "fetch full history and tags (fetch-depth: 0)" and never propose bumping every package. Test in Task 3.
4. **`publish` run outside the release environment or off the default branch** (a fork PR, a manual run on a branch, a local shell). It exits 3 before any request that writes. Test in Task 7.
5. **A `package.json` with unusual formatting** (tabs, CRLF, `"version"` inside a nested object). Only the top-level `version` value changes; every other byte stays the same. Test in Task 4.

---

## File Structure

```
src/release/mod.rs        Release model: Package {name, kind, dir, version, deps, publish}, read_release_model()
src/release/semver.rs     Version {major, minor, patch}, parse, bump
src/release/commits.rs    Commit {sha, subject, body}, conventional parse -> Bump
src/release/planner.rs    plan_versions(model, history, pins) -> Vec<NextVersion> (pure)
src/release/gitlog.rs     tags, commits since tag per path, shallow check (git CLI)
src/release/edit.rs       set_package_json_version, set_cargo_version, prepend_changelog, update dependents
src/release/tools.rs      Tool {Git, Npm, Cargo, Pnpm}: run(args, env) with HOIST_* overrides
src/release/registry.rs   npm and crates.io reads (version_exists, package_exists)
src/release/oidc.rs       actions_id_token(aud) -> Secret; crates token exchange and revoke
src/release/github_rel.rs tags, draft release, asset upload, publish draft
src/ops/release/{plan,prepare,publish}.rs   the three commands
src/config.rs             + PublishCfg schema (replaces toml::Table), release.toml schema
src/repo/targets.rs       + versions, internal deps, root single package as a target
src/cli.rs                + `release {plan,prepare,publish}`, Area npm|crates|release
tests/release_*.rs, tests/cli_release_*.rs, tests/fixtures/repos/release-*/, tests/common/stub_tool.rs
npm/hoist/, npm/hoist-<platform>/, .github/workflows/release.yml, hoist.toml   (hoist's own release, Task 11)
```

---

### Task 1: Release model and `[publish]` schema

**Files:**
- Create: `src/release/mod.rs`
- Modify: `src/config.rs` (`PublishCfg`), `src/repo/targets.rs`, `src/desired.rs`, `src/ops/explain.rs`, spec §4 and §5
- Test: `tests/release_model.rs`, fixture `tests/fixtures/repos/release-ws/` (pnpm workspace with `packages/a` (public npm, depends on `b`), `packages/b` (public npm), `crates/tool` (crate with `[package.metadata.hoist] targets`), and a root `hoist.toml` with `[release]`), fixture `tests/fixtures/repos/release-single/` (one root `Cargo.toml` `[package]`, no `[workspace]`)

**Interfaces:**
- Produces:
  - `pub enum PkgKind { Npm, Crate }`.
  - `pub struct Package { pub name: String, pub kind: PkgKind, pub dir: PathBuf /* repo-relative */, pub version: Sourced<String>, pub deps: Vec<String> /* internal package names */, pub publish: PublishCfg }`.
  - `pub struct ReleaseModel { pub packages: Vec<Package>, pub versioning: Versioning /* PerPackage|Fixed */, pub tag_format: String, pub environment: String }`.
  - `pub fn read_release_model(dir: &Path) -> Result<ReleaseModel, HoistError>`.
  - `impl ReleaseModel { pub fn tag_for(&self, p: &Package, version: &str) -> String }`.
- `PublishCfg` (strict, `deny_unknown_fields`):
  - `npm: Option<NpmPublish { access: "public"|"restricted" (default public), launcher: bool }>`;
  - `crates: Option<CratesPublish {}>`;
  - `github_release: Option<GithubRelease { targets: Vec<String>, bin: Option<String> }>`.
  - Defaults: an npm package gets `npm = {}`, a crate gets `crates = {}`, and `[package.metadata.hoist] targets` fills `github_release.targets`.
- Root `[package]` without `[workspace]`, and a root `package.json` with neither `workspaces` nor `pnpm-workspace.yaml`, count as packages. `[publish]` is then allowed in the root `hoist.toml`.
- Internal deps are:
  - npm: names in `dependencies`, `optionalDependencies` and `peerDependencies` that are workspace packages;
  - Cargo: `[dependencies]` entries that have a `path`.

- [ ] **Step 1: Write failing tests:**
  - `model_reads_versions_and_internal_deps`: on `release-ws`, `a.deps == ["b"]`, versions come from the manifests, and sources are `packages/a/package.json:<line of "version">`.
  - `single_root_crate_is_a_package_with_v_tag`: on `release-single`, `tag_for(.., "1.2.3") == "v1.2.3"`.
  - `tag_format_default_and_override`: the default is `{name}@{version}`; a `[release] tag_format` is used as given.
  - `unknown_publish_key_exits_3`: `[publish] npm = { acess = "public" }` exits 3 and names the key and file.
  - `prerelease_version_exits_3`: `"version": "1.0.0-beta.1"` exits 3 with "pre-release versions are not supported".
  - `explain_lists_release_items`: `hoist explain --json` contains `{"area":"release","kind":"package","key":"a"}` with the version in its reason.
- [ ] **Step 2: Run** `cargo test --test release_model` → FAIL.
- [ ] **Step 3: Implement.** `src/release/semver.rs` (`Version::parse(&str) -> Result<Version, String>`, `Display`) is created here, because the model validates versions. Spec §4 gets these table rows: root single package, package versions, internal deps. Spec §5 gets the `[publish]` schema exactly as above.
- [ ] **Step 4: Run** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: release model and [publish] schema`

### Task 2: Conventional commits and the version planner (pure)

**Files:**
- Create: `src/release/commits.rs`, `src/release/planner.rs`
- Modify: `src/release/semver.rs` (`bump`), `src/config.rs` (`ReleasePins` for `.hoist/release.toml`)
- Test: `tests/release_planner.rs`

**Interfaces:**
- Consumes: `ReleaseModel`, `Package`, `Version` (Task 1).
- Produces:
  - `pub enum Bump { None, Patch, Minor, Major }` (ordered).
  - `pub struct Commit { pub sha: String, pub subject: String, pub body: String, pub paths: Vec<String> }`.
  - `pub fn classify(c: &Commit) -> (Bump, Option<ChangeEntry>)`, where `ChangeEntry { section: Section /* Breaking|Features|Fixes */, text: String /* subject without the type prefix */, short_sha: String /* 7 chars */ }`.
  - `pub struct History { pub last_tag: HashMap<String, Option<String>> /* package -> tag */, pub commits: HashMap<String, Vec<Commit>> /* package -> commits since its tag */ }`.
  - `pub struct NextVersion { pub package: String, pub from: Version, pub to: Version, pub reason: String, pub entries: Vec<ChangeEntry>, pub first_release: bool }`.
  - `pub fn plan_versions(m: &ReleaseModel, h: &History, pins: &ReleasePins) -> Vec<NextVersion>`, returned in dependency order (dependencies first; ties by name).
- `.hoist/release.toml`: `[packages.<name>] version = "x.y.z"` and/or `skip = true`. Unknown keys exit 3. A pin applies only when it is above the bump result. A pin at or below the current version exits 3.

- [ ] **Step 1: Write failing tests** (table-driven where natural):
  - `classify_types`: `feat: x` → Minor, `fix(api): y` → Patch, `feat!: z` → Major, `chore: w` → None, a body containing `BREAKING CHANGE: …` → Major, `Merge pull request #1` → None, a non-conventional subject → None.
  - `breaking_below_1_bumps_minor`: 0.4.2 with `feat!` gives 0.5.0.
  - `dependent_gets_patch`: `b` gets a fix, so `b` 1.0.0→1.0.1 and `a` 2.0.0→2.0.1 with reason "dependency b bumped".
  - `fixed_versioning_uses_max_bump_for_all`.
  - `first_release_keeps_manifest_version` (no tag: `to == from`, `first_release == true`).
  - `docs_only_releases_nothing`.
  - `pin_and_skip_apply`.
  - `pin_not_above_current_exits_3`.
  - `order_is_dependencies_first`.
- [ ] **Step 2: Run** `cargo test --test release_planner` → FAIL.
- [ ] **Step 3: Implement.** Pure functions; no I/O.
- [ ] **Step 4: Run** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: conventional commits and version planner`

### Task 3: Git history reader and `hoist release plan`

**Files:**
- Create: `src/release/tools.rs`, `src/release/gitlog.rs`, `src/ops/release/plan.rs`, `tests/common/stub_tool.rs`
- Modify: `src/cli.rs` (`release plan`), `src/lib.rs`, spec §3 (exit codes of `release plan`)
- Test: `tests/cli_release_plan.rs` (real temporary git repos built with `tests/common::git`)

**Interfaces:**
- Consumes: `read_release_model`, `plan_versions` (Tasks 1–2).
- Produces:
  - `pub enum Tool { Git, Npm, Cargo, Pnpm }`.
  - `pub fn run(tool: Tool, dir: &Path, args: &[&str], env: &[(&str, &Secret)]) -> Result<String /* stdout */, ToolError>`. `ToolError` holds the tool, the args and the exit code, never env values.
  - `pub fn read_history(dir: &Path, m: &ReleaseModel) -> Result<History, HoistError>`. It runs `git rev-parse --is-shallow-repository` and exits 3 when the answer is true. It finds the last tag per package by matching `tag_for(p, <version pattern>)` against `git tag --list` and picks the highest version. Commits come from `git log <tag>..HEAD --format=%H%x1f%s%x1f%b%x1e --name-only -- <dir>`.
  - `pub fn release_plan(dir: &Path) -> Result<Report, HoistError>`. One item per package: area `release`, kind `version`, key `<name>`, action `create` when it releases (reason `1.2.3 -> 1.3.0 (feat: …)` or `first release 1.2.3`) and `ok` otherwise. Exit 1 when anything releases, 0 otherwise (the same rule as `plan`).
- `tests/common/stub_tool.rs`: `pub fn stub(dir: &Path, name: &str, script: &str) -> PathBuf` writes an executable script. It appends `$@` and the env names it was given (never values) to `<dir>/<name>.log` and prints a fixed stdout. Tasks 4, 5, 8 and 9 reuse it.

- [ ] **Step 1: Write failing tests:**
  - `plan_after_feat_shows_minor`: tag `b@1.0.0`, then a `feat:` commit in `packages/b`. Expect exit 1, the item `b` reads `1.0.0 -> 1.1.0`, and `a` gets a patch.
  - `nothing_releasable_exits_0`.
  - `shallow_clone_exits_3` (Review Focus 3): a `git clone --depth 1` of a fixture repo exits 3 and the message contains `fetch-depth: 0`.
  - `plan_is_read_only`: the fake API logs no request, and `git status --porcelain` and `git tag` are unchanged.
  - `json_is_one_object`.
- [ ] **Step 2: Run** `cargo test --test cli_release_plan` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: hoist release plan`

### Task 4: Manifest, dependent and changelog editors

**Files:**
- Create: `src/release/edit.rs`
- Test: `tests/release_edit.rs`

**Interfaces:**
- Consumes: `NextVersion`, `ChangeEntry` (Task 2).
- Produces:
  - `pub fn set_package_json_version(text: &str, v: &str) -> Result<String, String>`. A byte-preserving edit of the top-level `"version"` string only, found by a small JSON tokenizer that tracks depth. An error if there is none.
  - `pub fn set_package_json_dep(text: &str, dep: &str, req: &str) -> Result<String, String>`. Same approach, inside top-level `dependencies` / `optionalDependencies` / `peerDependencies`. It leaves `workspace:` specifiers alone.
  - `pub fn set_cargo_version(text: &str, v: &str) -> Result<String, String>` and `pub fn set_cargo_dep_version(text: &str, dep: &str, v: &str) -> Result<String, String>`. Both use toml_edit and touch only `[package] version` or the `version` of a path dependency.
  - `pub fn prepend_changelog(existing: Option<&str>, nv: &NextVersion, date: &str) -> String`. Format: `# Changelog` header if missing, then `## <to> (<YYYY-MM-DD>)`, then the sections `### Breaking changes`, `### Features`, `### Fixes` (empty ones omitted). Entries read `- <text> (<short_sha>)`. A first release without entries gets `- First release.`. The date comes from `timeutil::now_secs()` (honours `HOIST_NOW`).

- [ ] **Step 1: Write failing tests:**
  - `json_version_edit_preserves_bytes` (Review Focus 5): inputs with tabs, CRLF, a nested `"version"` inside `"engines"` before the top-level one, and unicode. The output equals the input with only the top-level value replaced.
  - `json_without_version_errors`.
  - `json_dep_edit_skips_workspace_protocol`.
  - `cargo_edit_keeps_comments`.
  - `cargo_path_dep_version_updated`.
  - `changelog_sections_and_order`.
  - `changelog_first_release_line`.
  - `changelog_prepends_above_previous`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4:** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: manifest and changelog editors`

### Task 5: `hoist release prepare`

**Files:**
- Create: `src/ops/release/prepare.rs`
- Modify: `src/cli.rs`, `src/lib.rs`, spec §9.2 (the exact flow below)
- Test: `tests/cli_release_prepare.rs` (temp git repo with a bare `origin` remote, the fake GitHub API, and stub `pnpm`/`cargo`)

**Interfaces:**
- Consumes: `read_history`, `plan_versions`, editors (Task 4), `Tool::run`, `Http::send_json`.
- Produces: `pub fn release_prepare(dir: &Path, verbose: bool) -> Result<Report, HoistError>`. The flow:
  1. Preflight (exit 3): a clean working tree, HEAD on the default branch, a GitHub token present, not shallow.
  2. `plan_versions`. When nothing is releasable: if an open PR from `hoist/release` exists (`GET /repos/{o}/{r}/pulls?head=<o>:hoist/release&state=open`), close it (`PATCH … {"state":"closed"}`) and delete the remote branch (`git push origin --delete hoist/release`). Item `release/pr` reads `ok`. Exit 0.
  3. Otherwise:
     - `git checkout -B hoist/release`;
     - apply the edits: versions, internal dependents' requirements, changelogs, and the launcher's platform packages (Task 9);
     - run `pnpm install --lockfile-only` when any npm package changed and `cargo update -w` when any crate changed;
     - `git commit -m "release: <list>"`;
     - `git push --force origin hoist/release` (the branch belongs to hoist and is rebuilt from the default branch every time);
     - restore the original branch.
  4. Create the PR (`POST /pulls`, base = default branch) or update its title and body (`PATCH /pulls/{n}`). The body lists each package `from → to`, the changelog entries, and the tags `publish` will create. Items: one `release/version` item per package (action `update`, reason "in release PR #n"), plus `release/pr` (`create` or `update`). Exit 0 on success.
  5. A failure after the first write is exit 1 with what was done (the M2 convention).
- The README notes that PRs opened with the default `GITHUB_TOKEN` do not trigger CI; pass a token that can (`GH_TOKEN`) for the release PR to run checks.

- [ ] **Step 1: Write failing tests:**
  - `prepare_opens_pr_with_bumps`: after the run, the bare remote's `hoist/release` has the commit, `package.json` shows the new version, `CHANGELOG.md` has the section, the stub `pnpm.log` shows `install --lockfile-only`, the fake API got `POST /repos/acme/x/pulls` with title `release: b@1.1.0, a@2.0.1`, and the original branch and working tree are restored.
  - `prepare_updates_existing_pr`: a `PATCH`, not a `POST`.
  - `prepare_closes_pr_when_nothing_to_release`.
  - `dirty_tree_exits_3_before_writes`.
  - `push_failure_exits_1_and_reports`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4:** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: hoist release prepare`

### Task 6: Registries, trusted publishers and the release environment in `plan`/`check`/`apply`

**Files:**
- Create: `src/release/registry.rs`
- Modify: `src/http.rs` (`Service::{Npm, Crates, GithubUploads}`, a crates User-Agent), `src/cli.rs` (`Area::{Npm, Crates, Release}`), `src/desired.rs`, `src/diff.rs` (or a new `src/diff_release.rs` if diff.rs would pass ~600 lines), spec §6 (npm/crates rows), §7.3
- Test: `tests/release_registry.rs`, `tests/cli_plan.rs` additions

**Interfaces:**
- Produces:
  - `pub fn package_exists(http: &Http, p: &Package) -> Result<Fetch<bool>, HoistError>`.
  - `pub fn version_exists(http: &Http, p: &Package, v: &str) -> Result<Fetch<bool>, HoistError>`.
  - The desired GitHub environment `<[release] environment>` with branch policy = default branch, whenever any package publishes to npm or crates.io or has `github_release`. It is merged with the workflow-inferred environments, without duplicates.
  - Per package, in areas `npm`/`crates`:
    - item kind `trusted_publisher`, key `<name>`, action `pending`, verify `None`. The reason is "registered by a person; hoist cannot read it with a CI token".
    - Human step `npm-trust:<name>`, titled with the exact command `npm trust github <name> --file release.yml --repo <o>/<r> --env <env> --allow-publish`, URL `https://www.npmjs.com/package/<name>/access`. For crates: `crates-trust:<name>`, URL `https://crates.io/crates/<name>/settings`.
    - When `package_exists` is false: item kind `first_publish`, action `pending`, and a human step `first-publish:<name>` ("publish the first version by hand; trusted publishing needs an existing package").
  - The release workflow file name comes from the workflow whose job declares `environment: <env>`. If there is none, the item `release/workflow` reads `pending` with the reason "no workflow job uses environment `<env>`".
- `plan`, `check` and `explain` stay GET-only. `apply` provisions the environment (the M2 Environment change) and adds no new write kinds.

- [ ] **Step 1: Write failing tests:**
  - `registry_404_means_absent` and `registry_200_means_present`, for npm (scoped name encoded `@scope%2fname`) and crates (asserting a `User-Agent` header).
  - `plan_lists_trusted_publisher_steps` (exit 2, human steps carry the exact command).
  - `plan_adds_release_environment`.
  - `new_package_gets_first_publish_step`.
  - `area_npm_reads_only_npm` (no GitHub or Cloudflare request).
  - `plan_still_get_only`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4:** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: registries, trusted publishers and release environment in plan`

### Task 7: `hoist release publish` — preflight, tags and GitHub Releases

**Files:**
- Create: `src/release/github_rel.rs`, `src/ops/release/publish.rs`
- Modify: `src/cli.rs` (`release publish [--assets DIR] [--dry-run]`), `src/lib.rs`, spec §3 (`release publish [--assets DIR] [--dry-run]`) and §9.3–9.5 (draft flow, preflight)
- Test: `tests/cli_release_publish.rs`

**Interfaces:**
- Consumes: `ReleaseModel`, `Http::send_json`, `Tool::run`, `sha2`.
- Produces:
  - `pub fn release_publish(dir: &Path, assets: Option<&Path>, dry_run: bool, verbose: bool) -> Result<Report, HoistError>`.
  - `fn pending(m: &ReleaseModel, tags: &[String]) -> Vec<(&Package, String /* tag */)>`: packages whose manifest version has no tag, in dependency order.
  - In `github_rel.rs`, `pub enum Done { Already, Wrote }`, plus:
    - `ensure_tag(gh, repo, tag, sha) -> Result<Done, HoistError>`: if `GET /git/ref/tags/<tag>` finds it at `sha`, the step is done; if it points elsewhere, exit 1 "tag exists at another commit"; otherwise `POST /git/refs`.
    - `ensure_release(gh, repo, tag, name, body, assets: &[PathBuf]) -> Result<Done, HoistError>`:
      - `GET /releases/tags/<tag>`: if it is published and its asset names match, done.
      - Otherwise find a draft for the tag in `GET /releases?per_page=100`, or create one (`POST /releases`, `draft: true`, `tag_name`, `name = tag`, `body` = the changelog section for that version).
      - Upload the missing assets and `SHA256SUMS` (lines `<hex>  <file>`) to `{uploads}/repos/{o}/{r}/releases/{id}/assets?name=<file>`, with `Content-Type: application/octet-stream`.
      - Then `PATCH /releases/{id} {"draft": false}`.
  - Assets per package with `github_release`: files `<assets>/<bin>-<target>` (`.exe` for `*-windows-*`). A missing file is exit 3 before any write. `bin` defaults to the package name.
- The preflight (Global Constraints) runs before anything else. `--dry-run` sends only GETs and prints the steps it would take, with action `create` or `ok`.
- After the writes, verification: the tag sha equals `GITHUB_SHA`, and the release is published with every expected asset plus `SHA256SUMS`. A failure is exit 1 naming what is missing.

- [ ] **Step 1: Write failing tests:**
  - `preflight_off_default_branch_exits_3`, `preflight_wrong_environment_exits_3` and `preflight_not_in_actions_exits_3` (Review Focus 4): only GETs, or no requests at all.
  - `publish_creates_tag_draft_assets_then_publishes`: the logged non-GET sequence is exact, and the `SHA256SUMS` body equals the sha256 of the fixture files.
  - `rerun_skips_existing_tag_and_release`.
  - `tag_at_other_commit_exits_1`.
  - `missing_asset_exits_3_before_writes`.
  - `dry_run_is_get_only`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4:** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: release publish tags and GitHub Releases`

### Task 8: Publish to npm and crates.io, and deploys `on = "release"`

**Files:**
- Create: `src/release/oidc.rs`
- Modify: `src/ops/release/publish.rs` (if it would pass ~300 lines, split into `publish/{github,registries}.rs`), spec §9.3 and §13
- Test: `tests/cli_release_publish.rs` additions, `tests/release_oidc.rs`

**Interfaces:**
- Consumes: `version_exists`, `package_exists` (Task 6), `Tool::run`, `Secret`.
- Produces:
  - `pub fn actions_id_token(aud: &str) -> Result<Secret, HoistError>`: `GET $ACTIONS_ID_TOKEN_REQUEST_URL&audience=<aud>` with `Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN`, reading `.value`. When the env vars are missing, exit 3 "the job needs `permissions: id-token: write`".
  - `pub fn crates_token(http: &Http) -> Result<(Secret, CratesTokenGuard), HoistError>`: `POST {crates}/api/v1/trusted_publishing/tokens` with body `{"jwt": <jwt>}`, reading `.token`. The JWT comes from `actions_id_token("crates.io")`. `CratesTokenGuard::revoke(self)` sends `DELETE {crates}/api/v1/trusted_publishing/tokens` with that token, and also runs on drop when not revoked explicitly.
  - Steps per pending package, after its GitHub Release (dependency order; each step skipped when `version_exists` already says 200):
    - **npm.** `package_exists == false` gives `pending` plus the `first-publish:<name>` human step and no publish. Otherwise check `npm --version` ≥ 11.5.1 (else exit 1 for that package with the reason) and run `npm publish --access <access>` in the package dir. Env: none. npm does the OIDC exchange itself.
    - **crates.** `package_exists == false` gives `pending` and a human step, as for npm. Otherwise get `crates_token`, run `cargo publish -p <name> --locked` with env `CARGO_REGISTRY_TOKEN` (a Secret passed only through env), then revoke.
    - **deploy.** For targets with `[deploy] on = "release"` whose package is in this release, run `pnpm --filter <package name> run deploy`.
  - After the writes, verification (§9.5): poll `version_exists` up to 5 times, 2 s apart (through Http's capped sleep, `HOIST_MAX_SLEEP_MS`). Not visible after that is exit 1.
- The first failure stops the remaining packages and gives exit 1 with done / failed / not attempted (the M2 `stopped` shape). Human steps alone give exit 2.

- [ ] **Step 1: Write failing tests:**
  - `npm_publish_runs_in_package_dir` (stub `npm` logs `publish --access public`; the stub `npm --version` prints `12.2.0`).
  - `old_npm_fails_that_package`.
  - `crates_exchange_publish_and_revoke`: the order is POST token, cargo stub, DELETE. The cargo stub log has env name `CARGO_REGISTRY_TOKEN` and no value. A canary JWT and token are absent from stdout and stderr with `-v`.
  - `crates_revoke_runs_when_cargo_fails`.
  - `rerun_after_partial_failure_publishes_only_missing` (Review Focus 1).
  - `never_published_package_is_pending_and_others_continue` (Review Focus 2, exit 2).
  - `missing_oidc_env_exits_3_before_writes`.
  - `release_deploy_runs_pnpm_filter`.
  - `registry_lag_retries_then_ok`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4:** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: release publish to npm and crates.io`

### Task 9: npm launcher with platform packages

**Files:**
- Modify: `src/release/mod.rs` (launcher discovery), `src/ops/release/prepare.rs`, `src/ops/release/publish.rs`, spec §9.3
- Test: `tests/cli_release_launcher.rs`, fixture `tests/fixtures/repos/release-launcher/` (`npm/tool/package.json` with `"bin"` and `optionalDependencies` on the platform packages, `npm/tool-linux-x64/package.json` with `os`/`cpu`, and the same for `darwin-arm64`; a crate `tool` with `github_release.targets = ["x86_64-unknown-linux-musl", "aarch64-apple-darwin"]`)

**Interfaces:**
- Rules: an npm package with `[publish] npm = { launcher = true }` is a launcher.
  - Its platform packages are `npm/<launcher dir name>-<platform>/`, with platforms mapped from targets:
    - `x86_64-unknown-linux-musl` → `linux-x64`
    - `aarch64-unknown-linux-musl` → `linux-arm64`
    - `x86_64-apple-darwin` → `darwin-x64`
    - `aarch64-apple-darwin` → `darwin-arm64`
    - `x86_64-pc-windows-msvc` → `win32-x64`
  - The targets come from the crate whose `github_release.bin` (or name) equals the launcher's `bin` name.
  - A launcher shares its version with that crate.
  - Platform packages are never released on their own.
- `prepare`: sets the platform packages' versions and the launcher's `optionalDependencies` to the new version.
- `publish`: copies `<assets>/<bin>-<target>[.exe]` to `npm/<…>-<platform>/bin/<bin>[.exe]` (mode 0755), publishes each platform package, then the launcher. Each step is skipped when that version already exists.
- A target without a mapping, or a missing platform dir, exits 3 at model read.

- [ ] **Step 1: Write failing tests:**
  - `prepare_bumps_platform_packages_and_optional_deps`.
  - `publish_copies_binaries_and_publishes_platforms_first` (the stub `npm.log` order: linux-x64, darwin-arm64, launcher).
  - `unknown_target_exits_3`.
  - `launcher_rerun_skips_published_platforms`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4:** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: npm launcher with platform packages`

### Task 10: hoist releases itself

**Files:**
- Create:
  - `hoist.toml` (root: `[release]` defaults; `[publish] crates = {}`, `github_release = { targets = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl", "x86_64-apple-darwin", "aarch64-apple-darwin", "x86_64-pc-windows-msvc"], bin = "hoist" }`);
  - `npm/hoist/` (`package.json` name `@keelstackhq/hoist`, `bin/hoist.js` launcher, `[publish] npm = { launcher = true }` via `npm/hoist/hoist.toml`);
  - `npm/hoist-<platform>/package.json` for the 5 platforms;
  - `npm/hoist/test/launcher.test.js` (node:test);
  - `.github/workflows/release.yml`.
- Modify: `Cargo.toml` (`[package] name = "keel-hoist"` stays; `version = "0.1.0"`), `.github/workflows/ci.yml` (add `node --test npm/hoist/test`), `pnpm-workspace.yaml` only if inference needs it to see `npm/*` (otherwise list `npm/*` via `[release]`; decide by reading `targets.rs` and record the choice in the report), README (Releasing), spec §12

**Interfaces:**
- `bin/hoist.js` maps `process.platform`/`process.arch` to `@keelstackhq/hoist-<platform>`, resolves `bin/hoist[.exe]` from it, and spawns it with inherited stdio and the same exit code. An unknown platform gets exit 1 with a message listing the supported ones.
- `release.yml`:
  - Triggers: `on: push: branches: [main]` and `workflow_dispatch`.
  - Job `prepare`: `contents: write`, `pull-requests: write`; `fetch-depth: 0`; runs `hoist release prepare` with `GH_TOKEN` (falling back to `GITHUB_TOKEN`).
  - Job `build`: matrix over the 5 targets, runs only when the head commit message starts with `release:`; uploads `hoist-<target>[.exe]` as artifacts.
  - Job `publish`: `needs: build`, `environment: release`, `permissions: contents: write, id-token: write`; `actions/download-artifact` into `dist/`; runs `hoist release publish --assets dist`.
  - The workflow uses the hoist binary built in the same run (`cargo run --release --locked --`), so hoist does not depend on an earlier release of itself.

- [ ] **Step 1: Write failing tests:**
  - `launcher.test.js`: platform mapping, and an unknown platform exits 1.
  - `tests/self_release.rs`: `hoist release plan -C .` on the repo itself parses `hoist.toml` and finds packages `keel-hoist` and `@keelstackhq/hoist` (5 platform packages, launcher shares the crate version), with exit 0 or 1 and no exit 3.
  - `hoist plan --area release -C .` lists the `release` environment and the trusted-publisher human steps for `keel-hoist` and `@keelstackhq/hoist`.
- [ ] **Step 2: Run** → FAIL. **Step 3: Implement.** **Step 4:** fast checks plus `node --test npm/hoist/test` → PASS.
- [ ] **Step 5: Commit** `ci: hoist releases itself`

### Task 11: Docs, spec facts and read-only dogfood

**Files:**
- Modify: `README.md`, `docs/PROGRESS.md`, `CLAUDE.md` (Layout: `src/release/`, `src/ops/release/`; Rules: the read-only list gains `release plan` and `release publish --dry-run`), spec §13

- [ ] **Step 1: README.**
  - `release plan|prepare|publish`;
  - the preflight conditions;
  - the first-publish and trusted-publisher human steps, with the exact commands;
  - the `GITHUB_TOKEN`-does-not-trigger-CI note;
  - the npm ≥ 11.5.1 and Node ≥ 22.14.0 requirement;
  - immutable releases (the draft flow).
- [ ] **Step 2: Spec §13.** Move the npm and crates facts from "to verify" to "researched 2026-10-17 (docs and source, not yet exercised)", with the source URLs from this plan's research. List the unverified ones: `pnpm publish` OIDC support; the crates exchange body field name `jwt` and token lifetime; the npm trust read response shape.
- [ ] **Step 3: Read-only dogfood** on hoist itself:
  - `hoist release plan --json`, `hoist release publish --dry-run --json` and `hoist plan --area release --json`;
  - capture the outputs in the scratchpad, never in the repo;
  - run the M2 token-prefix count check, which must give 0.
  Real releases need the owner. Record under Blocked, verbatim:
  - "first manual publish of `keel-hoist` to crates.io and `@keelstackhq/hoist` (+5 platform packages) to npm";
  - "register trusted publishers: `npm trust github …` for each npm package (2FA), crates.io settings for `keel-hoist`";
  - "`hoist apply` on keelstackhq/hoist to create the `release` environment";
  - "optional `GH_TOKEN` secret so the release PR triggers CI";
  - "enable immutable releases if wanted".
- [ ] **Step 4: Fast checks** → PASS.
- [ ] **Step 5: Commit** `docs: M3 docs and dogfood results`
