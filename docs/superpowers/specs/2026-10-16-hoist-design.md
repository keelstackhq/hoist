# hoist — Design Spec

Date: 2026-10-16 · Status: draft for review

Names: repo `keelstackhq/hoist`, binary `hoist`, Cargo package `keel-hoist` (lib `hoist`; the crate
`hoist` is taken, checked 2026-10-16), npm launcher `@keelstackhq/hoist` with platform packages
`@keelstackhq/hoist-{linux-x64,linux-arm64,darwin-x64,darwin-arm64,win32-x64}` (the splice pattern).

## 1. Goal

One command takes a repo generated from `keelstackhq/hull` from "files on disk" to "set up and
shipping": it provisions what the repo needs in GitHub and Cloudflare, and it releases and
publishes its packages to npm, crates.io, GitHub Releases and Cloudflare.

Why: every hull repo so far ended with the same hand-written Blocked list (branch protection on
`ci-ok`, Actions permissions, secrets, Renovate, Cloudflare project, domain), and splice and hoard
each describe a hand-run release. hull owns files, splice keeps files current, hoard caches CI;
nothing owns the systems around the repo.

Success:
- After `splice new`, `hoist apply` turns the whole Blocked list into done or into explicit human
  steps with links, and `hoist check` reports zero drift.
- A release is "merge the release PR"; publishing uses OIDC trusted publishing, so no long-lived
  publish token exists.
- Re-running any command on a repo that is in sync changes nothing.

### Non-goals (v1)

- A general infrastructure tool. Breadth (databases, DNS zones, other clouds) stays with
  Terraform/OpenTofu. hoist covers GitHub, Cloudflare (Pages, Workers, custom domains, API tokens),
  npm, crates.io and GitHub Releases, nothing else. No provider plugin system.
- Managing files in the repo. Workflows and configs are rendered by hull and kept current by
  splice; hoist only reads them (and the release PR edits versions, changelogs and lockfiles).
- A hosted service or bot server. Everything runs as a CLI, locally or in GitHub Actions.
- A GitHub App of its own (v2 candidate, §8.4).

## 2. Positioning against Terraform/OpenTofu

hoist overlaps with Terraform's GitHub and Cloudflare providers. It does not compete on breadth;
it is better at one job, setting up and shipping a hull repo, because:

1. **No state file.** Desired state comes from the repo; actual state is read live from the APIs
   on every run. Every resource has a natural key (repo, ruleset name, project name, token name),
   so no state backend, locking or state drift exists.
2. **Secrets are never stored.** Plans track presence and update time only. Values come from the
   environment or a hidden prompt and are sealed straight into GitHub.
3. **Inferred from the code.** Most of the desired state is derived from files the repo already
   has (§4); `hoist.toml` holds only what cannot be inferred. Nothing is described twice.
4. **Cross-system wiring is one intent.** "This site deploys to Pages" becomes: project, scoped
   Cloudflare token, GitHub secret, and a check that the workflow references that secret name.
   Trusted publishers are registered against the exact workflow file and environment that
   publishes.
5. **Behaviour checks, not attribute checks.** After `apply`, each resource verifies that it works
   (§7.4): the required check is really reported by a CI job, the deploy URL answers, TLS is live,
   the trusted publisher matches the workflow.
6. **Human steps and releases.** Steps no API can do (installing a GitHub App, buying a domain)
   are first-class pending steps with links and re-verification. Releasing and publishing are in
   scope; Terraform has neither.
7. **Agent- and PR-native.** One JSON object on `--json`, stable exit codes, a skill for agents,
   plan output usable as a PR comment, drift checks as a scheduled workflow.
8. **Adopts existing repos.** `hoist import` writes overrides from the live state.

Coexistence: every area can be marked `managed = "external"` (§5); hoist then verifies but never
writes, so two tools never own the same setting. `hoist export --tofu` (M4) writes equivalent
OpenTofu HCL for teams that must standardise on Terraform.

## 3. CLI

```
hoist explain [--json]                 desired model and the file each item came from
hoist plan    [--json] [--area A]      diff desired vs live, no writes
hoist apply   [--yes] [--prune] [--area A] [--json]
hoist check   [--json]                 plan + verify, read-only; exit 1 on drift
hoist import  [--write]                derive hoist.toml overrides from live state
hoist auth check [--json]              permissions each token has vs needs
hoist secrets set NAME                 value from env NAME or hidden prompt
hoist secrets rotate [NAME]            for tokens hoist minted (§8.3)
hoist release plan    [--json]         next versions and changelogs, no writes
hoist release prepare                  open/update the release PR (CI, push to main)
hoist release publish                  tag, release, publish (CI, release PR merged)
hoist export --tofu <dir>              M4
```

Areas: `github`, `cloudflare`, `npm`, `crates`, `release`. Global flags: `-C <dir>`, `--json`,
`--no-color`, `-v`.

### Exit codes

`0` in sync / success · `1` drift, a failed write or a failed verification ·
`2` human step pending · `3` usage, config, auth or API error (message says how to fix it).

### Output

Human output is a table: resource, action (`create`, `update`, `ok`, `drift`, `pending`,
`unknown`), reason, source (file and line or `hoist.toml` key). `--json` prints exactly one object
on stdout: `{ "version": 1, "command", "ok", "exit_code", "items": [ { "area", "kind", "key",
"action", "reason", "source", "verify" } ], "human_steps": [ { "id", "title", "url", "status" } ] }`.
Logs go to stderr. Secret values never appear in either (§8.5).

## 4. Inference: the repo model

hoist builds a typed model from the repo before reading any API.

| Source | Inferred |
|---|---|
| `git remote get-url origin` (GitHub URL) | owner/repo |
| `git symbolic-ref refs/remotes/origin/HEAD`, else `main` | default branch |
| `.github/workflows/*.yml`: a job named `ci-ok` (or `[github] required_checks`) | required status checks |
| `${{ secrets.NAME }}` in workflows (excluding `GITHUB_TOKEN`) | secrets that must exist |
| `environment: NAME` in workflows | GitHub environments |
| `renovate.json` present | Renovate app installed (check only) |
| `merge_group:` trigger in a workflow (unless `[github] merge_queue` is set) | merge queue on the default-branch ruleset |
| `apps/*/wrangler.jsonc` with `assets` and no `main` | Cloudflare target (Pages unless overridden) |
| `apps/*/wrangler.jsonc` with `main` | Cloudflare Workers script |
| `package.json` with `"private": false` or `publishConfig` | npm package |
| `Cargo.toml` `[package]` without `publish = false` | crate |
| `[package.metadata.hoist] targets` or `hoist.toml` `[publish]` | binary release targets |
| workspace globs from `pnpm-workspace.yaml` and `Cargo.toml` `[workspace]` | package graph |

Rules:
- Inference is deterministic and offline; `hoist explain` prints every item with its source.
- An item that cannot be inferred and is required (for example a Pages project name for a
  Cloudflare target) is a config error (exit 3) that names the `hoist.toml` key to set. Defaults
  exist only where they are safe (project name defaults to `<repo>-<target>`).
- A workflow that references a secret no provisioner can produce becomes a "user secret": hoist
  checks presence and `hoist secrets set` fills it.

## 5. `hoist.toml` (overrides)

Optional. A root file and one per target directory (`apps/<name>/hoist.toml`,
`packages/<name>/hoist.toml`, `crates/<name>/hoist.toml`), found via the workspace globs. Unknown
keys are an error. Every key overrides or completes an inferred value.

Root:

```toml
[github]
visibility = "private"            # private | public | internal
required_checks = ["ci-ok"]       # default: inferred
merge_queue = false               # default: inferred from a merge_group trigger
actions_permissions = "read"      # default GITHUB_TOKEN permission: read | write
allow_auto_merge = true
delete_branch_on_merge = true
managed = "hoist"                 # hoist | external (verify only)

[cloudflare]
account_id = ""                   # default: env CLOUDFLARE_ACCOUNT_ID
managed = "hoist"

[release]
versioning = "per-package"        # per-package | fixed
tag_format = "{name}@{version}"   # single-package repos default to "v{version}"
environment = "release"
```

Per target:

```toml
[deploy]
provider = "cloudflare-pages"     # cloudflare-pages | cloudflare-workers
project = "keelstack-landing"
dir = "dist"
domain = ""                       # optional custom domain
on = "push"                       # push (main) | release

[publish]
npm = { access = "public", launcher = false }
crates = {}
github_release = { targets = ["x86_64-unknown-linux-musl", "aarch64-apple-darwin"] }
```

hull renders a per-target `hoist.toml` only where inference needs help (Pages project name,
release targets). `hoist.toml` files are seed files: splice never overwrites them.

## 6. Resources

A fixed set of typed resources. Each has a natural key, a read, a diff, a write and a verify.

| Area | Resource | Key | Write | Verify |
|---|---|---|---|---|
| github | repo settings | owner/repo | PATCH repo | settings read back |
| github | branch ruleset | name `hoist: default branch` | create/update ruleset | required check is a job name in a workflow on the default branch |
| github | Actions permissions | repo | PUT default workflow permissions | read back |
| github | environment | name | create/update environment with branch policy `main` | read back |
| github | secret | name | PUT sealed secret (presence only) | listed with recent `updated_at` |
| github | app installation | app slug | human step (link) | installation visible to the repo |
| cloudflare | Pages project | project name | create project (production branch = default branch) | deploy URL answers 200 after the next deploy |
| cloudflare | Workers script | script name | none (created by first deploy) | script exists after deploy |
| cloudflare | custom domain | hostname | attach to project | DNS resolves, TLS valid, 200 |
| cloudflare | API token | name `hoist:<owner>/<repo>:<purpose>` | create scoped token | token verify endpoint ok, expiry > 30 days |
| npm | trusted publisher | package + repo + workflow + environment | API if available, else human step | registry shows the publisher |
| crates | trusted publisher | crate + repo + workflow + environment | API if available, else human step | crates.io shows the publisher |

Settings hoist does not model are never touched.

The ruleset diff covers enforcement (anything but `active` is an `update`), the default-branch
target, the required checks and the merge queue. A secret is present when it is a repository
secret or an organization secret the repo can use (`GET .../actions/organization-secrets`); a
403 on the organization list counts as none and the reason says so. Environment secrets are not
checked in M1.

## 7. Provisioning

### 7.1 Plan

1. Build the repo model (§4) and merge `hoist.toml` overrides.
2. Read the live state of every resource in the selected areas. Reads run in parallel per area,
   respect `Retry-After` and rate-limit headers, and never write.
3. Diff per resource: `create`, `update` (with field-level before/after, secrets as
   `present/missing`), `ok`, `drift` (live differs from desired and the area is external),
   `pending` (human step), `unknown` (no permission to read; reason quotes the API message).
   A rejected token (401) on any read, or a 404 on the repo itself (the token cannot see it),
   is an error (exit 3), never `unknown`: a dead token must not read as in sync. The Cloudflare
   account id is required only when the Cloudflare area is read.
4. Deletes appear only with `--prune`, and only for resources hoist would itself create (by key
   prefix `hoist:` or by being in the model).

### 7.2 Apply

1. `hoist auth check` runs first: every planned write lists the permissions it needs; if any token
   lacks one, apply stops before the first write and prints exactly what is missing (exit 3).
2. Confirmation prompt unless `--yes` (CI requires `--yes`).
3. Writes run in dependency order (token → secret → environment → ruleset; project → domain).
   Each write is idempotent. On a failure, hoist stops, prints what was done and what was not, and
   exits 1; re-running continues where it stopped.
4. Verification (§7.4) runs for every touched resource.

### 7.3 Human steps

A step no API can perform (installing a GitHub App, buying or delegating a domain, a registry
action without an API) is a `pending` item with a title, a URL and a check. `apply` exits 2 while
any step is pending; the next `plan`, `apply` or `check` re-runs the check and marks it done.
Steps never block unrelated writes.

### 7.4 Verification

Each resource has a behaviour check (table in §6). Only resources that exist are verified: while
a Pages project does not exist yet, it and its domain are planned (`create`) and not probed.
An existing Pages project or an attached domain whose URL answers 404 is a failed verification
(exit 1) with the reason "no deployment answers yet". Verification failures are drift (exit 1)
with a reason that says what to fix (for example "required check `ci-ok` is not the name of any
job in `.github/workflows/*.yml` on `main`").

### 7.5 Import

`hoist import` reads the live state, compares it with the inferred model and prints the
`hoist.toml` overrides that would make `plan` show no changes; `--write` writes them. Unknown live
settings are listed as comments, not imported.

## 8. Auth and secrets

### 8.1 Local credentials

- GitHub: `GITHUB_TOKEN` or `GH_TOKEN`, else `gh auth token` if `gh` is installed, else exit 3 with
  the needed fine-grained permissions. M1 only reads (Administration, Secrets, Environments,
  Actions, Metadata: read); write permissions (Administration, Secrets, Environments, Actions:
  write; Contents: read) come with M2.
- `hoist auth check` exits 3 only without a GitHub token. A missing Cloudflare token (when the
  repo has a deploy target) or account id is an `unknown` item and the GitHub probes still run;
  a probe that gets a 401 turns that service's `token` item `unknown` ("token rejected (401)").
- Cloudflare: `CLOUDFLARE_API_TOKEN` or a hidden prompt. For token minting it needs the
  "API Tokens: Edit" permission; it is used only locally.
- npm, crates.io: only to register trusted publishers, when an API exists for it (§13).

### 8.2 Minted CI credentials

hoist creates the Cloudflare token CI uses, scoped to the account and the specific Pages projects
or Workers scripts, named `hoist:<owner>/<repo>:deploy`, and stores it as the GitHub secret the
workflows reference (`CLOUDFLARE_API_TOKEN`, plus `CLOUDFLARE_ACCOUNT_ID`). The name is the key;
no id is stored anywhere.

### 8.3 Rotation and expiry

`hoist secrets rotate` mints a new token, replaces the GitHub secret, triggers or waits for a
successful deploy check, then revokes the old token. `check` flags minted tokens that expire
within 30 days.

### 8.4 Drift check in CI

The default `GITHUB_TOKEN` cannot read rulesets or the secret list. `drift.yml` uses a read-only
fine-grained token in the secret `HOIST_READ_TOKEN` (hoist can create the secret; the token itself
is a human step). Without it, `check` still runs and reports those resources as `unknown`. A
keelstack GitHub App would remove this token (v2).

### 8.5 Never leaking

- Secret values come only from environment variables or a hidden prompt, never from argv.
- Values are sealed with the repo public key (libsodium sealed box, as GitHub requires) and
  zeroised after use.
- One HTTP layer redacts `Authorization` headers and never logs bodies of secret endpoints.
- Plans and JSON show only `present`, `missing` or `updated <date>`.
- Tests inject a canary secret and assert it never appears in stdout, stderr, JSON or logs (§11).

## 9. Releasing

### 9.1 Change detection

Conventional commits since the package's last tag (the org already uses `feat:`, `fix:`, `ci:`,
`docs:`, `chore:`). A commit counts for every package whose files it touches; internal dependents
of a bumped package get a patch bump. `fix` → patch, `feat` → minor, `!` or `BREAKING CHANGE:` →
major; below 1.0.0 a breaking change bumps minor. `docs`, `ci`, `chore`, `test` alone release
nothing. `.hoist/release.toml` may pin a version or skip a package for the next release.

### 9.2 Release PR

On every push to the default branch, `hoist release prepare` opens or updates one PR (branch
`hoist/release`, title `release: <list>`) that bumps versions in `package.json` / `Cargo.toml`,
regenerates lockfiles with `pnpm install --lockfile-only` and `cargo update -w`, writes
`CHANGELOG.md` per package and lists the tags to be created. If nothing is releasable, it closes
an open release PR.

### 9.3 Publish

When the release PR is merged, `hoist release publish` runs in the `release` environment, in
dependency order, each step skipped if already done (re-runs are safe):

1. tag (`tag_format`) and push the tag;
2. GitHub Release with the changelog section and binary assets plus `SHA256SUMS`;
3. npm: publish with provenance via OIDC trusted publishing;
4. crates.io: `cargo publish` with a short-lived token from trusted publishing;
5. Cloudflare deploys of targets with `on = "release"`.

Binaries come from a matrix job in hull's `release.yml` that builds `[publish] github_release`
targets and uploads them as workflow artifacts; `publish` attaches them. `npm.launcher = true`
publishes the launcher and per-platform packages (splice's pattern).

### 9.4 Safety

Publishing runs only from the default branch in the `release` environment. hoist provisions that
environment (branch policy) and registers trusted publishers for exactly that repo, workflow file
and environment, so a fork or another workflow cannot publish. No publish token exists.

### 9.5 Verification

After publishing: the version is visible on each registry, the release has all assets and
checksums, the tag points at the merge commit. A failure is exit 1 with what is missing.

## 10. Integration with hull, splice and decisions

- hull renders: per-target `hoist.toml` where inference needs help (seed), `release.yml` and
  `drift.yml` (owned), and the site/worker `deploy` scripts from `[deploy]` (Pages or Workers). The
  `preview.yml` link extraction covers `*.pages.dev` as well as `*.workers.dev`.
- splice: no change. `hoist.toml` files are ordinary template files; the template contract is
  unaffected.
- decisions: new D-004 "repo setup and releases go through hoist" in `keelstackhq/decisions`.
- Generated `CLAUDE.md`: one line pointing to `hoist plan` / `hoist check`.
- Rollout: example-app (import, apply, zero drift), keelstack (Pages deploy via hoist), then hoist
  releases itself, then splice and hoard replace their hand-written release steps.

## 11. Testing

- Unit: inference on fixture repos rendered from hull's combos (`base`, `web`, `site`, `docs`);
  diffing as pure functions (desired, actual) → plan; insta snapshots of table and JSON output.
- HTTP: an in-process fake server replays recorded GitHub, Cloudflare, npm and crates.io responses
  for create, update, no-op, 403, 404, 429 with `Retry-After`, and partial failure followed by a
  re-run. Runs on every PR with no network.
- Secrets: every test run injects a canary secret; a test asserts it never appears in any output.
- Live contract tests: nightly, against a sandbox repo in the org and a test Cloudflare account:
  apply → check (zero drift) → teardown. Never on PRs.
- Dogfood: example-app and keelstack for provisioning; hoist's own releases for publishing.

## 12. Distribution

Like splice: static musl/macOS/Windows binaries on GitHub Releases, the npm launcher
`@keelstackhq/hoist` with platform packages, `cargo install keel-hoist`. hoist releases itself
with `hoist release` from M3 on. `rustls` only, no OpenSSL; no `unsafe`.

## 13. Facts to verify before the milestone that needs them

Checked or to check with real API calls and dated in `docs/PROGRESS.md`:

- GitHub: rulesets API fields for required status checks and merge queue; default workflow
  permissions endpoint; environment branch policies; reading app installations for a repo with a
  user token (may need the installation list of the org).
- Cloudflare: Pages project create/read API, custom domain attach, API token create with resource
  scoping to specific Pages projects (may only scope to the account; then document it).
- npm: whether trusted publishers can be registered by API or CLI, or only in the web UI (then a
  human step); whether `pnpm publish` performs the OIDC exchange or hoist must `npm publish` the
  packed tarball.
- crates.io: trusted publishing token exchange in Actions and whether publishers can be registered
  by API.

Verified 2026-10-16 (M1 dogfood on `keelstackhq/keelstack`, session `GITHUB_TOKEN`, through the
sandbox HTTPS proxy; Cloudflare not run, no token):

- The rustls platform verifier works through the proxy: `GET /repos/{owner}/{repo}` returned 200.
- Private repo, this token: `GET /repos/{o}/{r}` 200 (`allow_auto_merge` and `delete_branch_on_merge`
  readable, both false). `GET .../rulesets` and `.../branches/main/protection` 403 with the message
  "Upgrade to GitHub Pro or make this repository public to enable this feature." (a plan limit,
  not a missing permission). `GET .../actions/secrets`, `.../actions/permissions/workflow` and
  `.../environments` 403 "Access to this GitHub Actions path is not permitted through this proxy."
  (the proxy blocks them). `GET /user/installations` 403 (so app installations are unreadable
  with this token).
- Not yet verified: rulesets, secrets, environments and Actions permissions field shapes against a
  real repo; merge queue fields; Cloudflare Pages API, custom domains and token scoping.

M2 Task 6 decision (not yet verified against the real API): the minted deploy token is scoped to
the account (`com.cloudflare.api.account.<id>`: `*`) with the permission groups `Pages Write`
and/or `Workers Scripts Write`, matched by exact name from `GET /user/tokens/permission_groups`.
Cloudflare cannot scope a Pages permission to one project. The token expires after 365 days; plan
reports `update` within 30 days of expiry (`hoist secrets rotate`). `HOIST_NOW` (RFC 3339)
overrides "now" for tests only.

## 14. Milestones

1. **Read-only** — repo model, `explain`, `plan`, `check`, `auth check` for GitHub and Cloudflare;
   fake-server tests. Useful on its own as a drift report.
2. **Provisioning** — `apply` with verification, human steps, `import`, Cloudflare token minting,
   sealed secrets, `secrets set/rotate`.
3. **Releasing** — `release plan/prepare/publish`, npm and crates.io trusted publishing, GitHub
   Releases with binaries, the `release` environment; hoist releases itself.
4. **Integration** — hull renders `hoist.toml`, `release.yml`, `drift.yml` and deploy scripts;
   D-004; rollout to example-app, keelstack, hull, splice, hoard; `export --tofu`.

Each milestone gets its own plan in `docs/superpowers/plans/`.
