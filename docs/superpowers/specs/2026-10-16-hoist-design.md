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
hoist import  [--write] [--area A]     derive hoist.toml overrides from live state
hoist auth check [--json]              permissions each token has vs needs
hoist secrets set NAME                 value from env NAME or hidden prompt; never argv
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

`release plan` writes nothing: it exits `1` when any package would release (one `release/version`
item per package, `create` or `ok`), `0` when none would, and `3` for a config error, a failing
`git`, or a shallow clone or missing tags (no local tag matches a package while `origin` has
one; "fetch full history and tags (fetch-depth: 0)"; it never proposes bumping every package).
Tool paths come from `HOIST_GIT`, `HOIST_NPM`, `HOIST_CARGO`, `HOIST_PNPM`. `git` runs with
`GIT_TERMINAL_PROMPT=0` and `GCM_INTERACTIVE=never`, so it never waits for credentials.

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
| root `Cargo.toml` with `[package]` and no `[workspace]`, or root `package.json` with neither `workspaces` nor `pnpm-workspace.yaml` | the root is the single package (tag format `v{version}`; `[publish]` goes in the root `hoist.toml`) |
| `version` in each package manifest (`MAJOR.MINOR.PATCH`; pre-release versions exit 3) | package versions, with `path:line` of the `version` key |
| npm `dependencies`, `optionalDependencies`, `peerDependencies` naming workspace packages; Cargo `[dependencies]` entries with a `path` (or `workspace = true` where `[workspace.dependencies]` has one) | internal deps |
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

`versioning = "fixed"` means one shared version for the repo (the highest current version, bumped by the highest bump); every package releases at it, a version pin above it moves every package, the first release (no tag yet) keeps the highest manifest version, and `skip` in `.hoist/release.toml` is refused (exit 3).

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
github_release = { targets = ["x86_64-unknown-linux-musl", "aarch64-apple-darwin"], bin = "tool" }
```

`[publish]` schema (strict):
- `npm`: `access = "public" | "restricted"` (default `public`), `launcher` (bool, default `false`).
- `crates`: an empty table; its presence means "publish to crates.io".
- `github_release`: `targets` (list of target triples) and `bin` (binary name, default: the package name).

`[publish]` is strict (unknown keys exit 3). Defaults: an npm package gets `npm = {}`
(`access` defaults to `public`, `launcher` to `false`), a crate gets `crates = {}`, and
`[package.metadata.hoist] targets` in `Cargo.toml` fills `github_release.targets`. A single-package
repo (the root is the package) puts `[publish]` in the root `hoist.toml`; in a workspace a root
`[publish]` is an error (exit 3), it belongs in the package's own `hoist.toml`. A `[release]
tag_format` must contain `{version}`. A crate without `version` is unpublished and not a package.

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
| cloudflare | API token | name `hoist:<owner>/<repo>:<purpose>` | create account-level token (Pages/Workers Write; Cloudflare cannot scope Pages to one project) | token verify endpoint ok, expiry > 30 days |
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
   prefix `hoist:` or by being in the model). The live deploy token is the newest
   `hoist:<owner>/<repo>:deploy` token issued at or before the `updated_at` of the GitHub secret
   `CLOUDFLARE_API_TOKEN`; `plan` and `check` report that one, and `--prune` never revokes it nor
   a deploy token issued after the secret was written (an orphan of a failed mint; `hoist secrets
   rotate` revokes those). A single deploy token without dates counts as live. When the live
   token cannot be told apart (no `updated_at`, a missing `issued_on`, no deploy token issued
   before the secret was written, two candidates with the same `issued_on`, or the newest
   candidate within 60 s of `updated_at` while another candidate exists, which allows for clock
   skew between Cloudflare and GitHub), `--prune` revokes nothing and the `api_token` item is
   `pending` with the human step `rotate:CLOUDFLARE_API_TOKEN` naming the token ids.

### 7.2 Apply

1. Preflight before the first write (fine-grained GitHub tokens cannot list their own write
   permissions): GitHub writes need `permissions.admin: true` in `GET /repos/{o}/{r}` (else exit 3,
   "the token needs admin on <o>/<r> (Administration: write)"); Cloudflare writes need an account
   id and a token that `GET /user/tokens/verify` reports `active` (else exit 3). When a deploy
   token is to be minted, the Cloudflare permission groups are resolved here too (a GET), so a
   failure there exits 3 with nothing written. With no changes, apply writes nothing and reports
   the `check` result.
2. Confirmation prompt unless `--yes`: on a terminal the change list goes to stderr and `y`/`yes`
   continues; any other answer writes nothing and reports every change as `not applied:
   declined` (exit 1). Without a terminal and without `--yes`, exit 3 ("use --yes in
   non-interactive runs"). CI passes `--yes`.
3. Writes run in dependency order (token → secret → environment → ruleset; project → domain).
   Each write is idempotent. On a failure, hoist stops and exits 1; the report lists each change
   as `applied`, `failed: <error>` or `not attempted`, plus the plan's other items (pending,
   unknown, ok). Re-running continues where it stopped.
4. After the writes, apply re-reads the live state and runs the `check` logic (§7.4); that is the
   final report (`command: "apply"`). If that re-read or check fails after writes, the report
   lists the applied changes plus a `post_apply_check` item with the error (exit 1); exit 3
   is only for errors before the first write. A `create`/`update` still present is exit 1. Human steps
   (apps, user secrets, rotation, a domain whose zone is not on the account) make it exit 2
   when nothing failed.

### 7.3 Human steps

A step no API can perform (installing a GitHub App, buying or delegating a domain, a registry
action without an API) is a `pending` item with a title, a URL and a check. `apply` exits 2 while
any step is pending; the next `plan`, `apply` or `check` re-runs the check and marks it done.
Steps never block unrelated writes.

### 7.4 Verification

Each resource has a behaviour check (table in §6). Only resources that exist are verified: while
a Pages project does not exist yet, it and its domain are planned (`create`) and not probed.
An existing Pages project or an attached domain whose URL answers 404 is a failed verification
(exit 1) with the reason "no deployment answers yet". A Pages project or domain that `apply`
created in the same run is not probed: it is `ok` with the reason "created in this run; verified
by `hoist check` after the first deploy". Verification failures are drift (exit 1)
with a reason that says what to fix (for example "required check `ci-ok` is not the name of any
job in `.github/workflows/*.yml` on `main`").

### 7.5 Import

`hoist import` reads the live state, compares it with the inferred model and prints the
`hoist.toml` overrides that would make `plan` show no changes; `--write` writes them. Unknown live
settings are listed as comments, not imported.

Imported keys: `[github] visibility` (only when already set), `allow_auto_merge`,
`delete_branch_on_merge`, `actions_permissions`, `required_checks` and `merge_queue` (from the
live `hoist: default branch` ruleset; `merge_queue` only when that ruleset has required checks),
and per Pages target `[deploy] project` when the inferred project is missing and exactly one live
Pages project name starts with the target name. Other live rulesets become comments
(`# live ruleset "<name>" is not managed by hoist`) in the printed text; `--write` does not write
them. A field whose live read is unknown or forbidden gives an `unknown` item with the read's
reason and no override. Items have kind `override`, key `<file>:<table>.<key>` and the TOML line
as reason; action `drift` without `--write`, `create` (key added) or `update` (key replaced) with
it. In table mode without `--write` the TOML text follows the table; `--json` prints only the
object. `--write` edits with `toml_edit`: a missing file is created, a key with a different value
is replaced in place keeping its comments, equal values leave the file byte-identical, and every
write goes through a temp file plus rename in the same directory; every file is rendered before
any is replaced (a failure writes nothing), permissions are kept and a symlinked `hoist.toml` is
refused. A `hoist: default branch` ruleset that is not `active` or does not target the default
branch gives a comment item (`# live ruleset "hoist: default branch" enforcement "<x>" is not
importable; apply sets "active"`, likewise `target "<include>" ... apply sets
"~DEFAULT_BRANCH"`), since neither has a `hoist.toml` key; it does not change the exit code. Exit codes: `0` nothing to import or a
successful `--write`, `1` overrides exist and `--write` was not given, `3` config, auth or API
error. GET requests only. `--area github|cloudflare` (repeatable) limits the reads like `plan`:
without the `cloudflare` area import makes no Cloudflare reads and needs no Cloudflare token or
account id; without `github` it needs no GitHub token.

## 8. Auth and secrets

### 8.1 Local credentials

- GitHub: `GITHUB_TOKEN` or `GH_TOKEN`, else `gh auth token` if `gh` is installed, else exit 3 with
  the needed fine-grained permissions. Reads (`plan`, `check`, `import`, `auth check`) use
  Administration, Secrets, Environments, Actions, Metadata: read. `apply` and `secrets` writes use
  Administration, Secrets, Environments, Actions: write and Contents: read; M2 checks only
  `permissions.admin` on the repo before writing (§7.2), since fine-grained tokens cannot list
  their own permissions.
- `hoist auth check` exits 3 only without a GitHub token. A missing Cloudflare token (when the
  repo has a deploy target) or account id is an `unknown` item and the GitHub probes still run;
  a probe that gets a 401 turns that service's `token` item `unknown` ("token rejected (401)").
- Cloudflare: `CLOUDFLARE_API_TOKEN` or a hidden prompt. For token minting it needs the
  "API Tokens: Edit" permission; it is used only locally.
- npm, crates.io: only to register trusted publishers, when an API exists for it (§13).

### 8.2 Minted CI credentials

hoist creates the Cloudflare token CI uses, scoped to the account (Cloudflare cannot scope a Pages
permission to one project; §13), named `hoist:<owner>/<repo>:deploy`, and stores it as the GitHub secret the
workflows reference (`CLOUDFLARE_API_TOKEN`, plus `CLOUDFLARE_ACCOUNT_ID`). The name is the key;
no id is stored anywhere.

`hoist secrets set NAME` takes the value from env `NAME`, else a hidden prompt on a terminal, else
exits 3; there is no value flag. Nothing typed after `NAME` is accepted or echoed: extra
arguments exit 3 with a fixed message, and an invalid name shows only the text before a `=`
(`NAME=VALUE`). `NAME` must match `^[A-Z_][A-Z0-9_]*$` and not start with
`GITHUB_` (exit 3 before any request). It needs admin on the repo (same preflight as `apply`),
seals the value and PUTs the secret (the value is read last, just before the PUT; item source is
`env NAME` or `prompt`). `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID` and `GH_TOKEN` are also
hoist's own credentials, so their env var is never sealed: they take the value only from the
terminal prompt, and without a terminal exit 3 before any request (for `CLOUDFLARE_API_TOKEN` the
message adds that `hoist apply` can mint a scoped deploy token); a failed PUT exits 1 with item reason `failed: <error>`.

### 8.3 Rotation and expiry

`hoist secrets rotate` mints a new token, replaces the GitHub secret, verifies the new token with
Cloudflare's token verify endpoint, then revokes the old token. (Deploy checks need a deploy run,
which M4's workflows provide.) `check` flags minted tokens that expire within 30 days.

Only `CLOUDFLARE_API_TOKEN` is rotatable (no name means all rotatable; another name exits 3). Before
any write, rotate runs `apply`'s GitHub-admin and Cloudflare preflights, requires hoist-managed
Cloudflare with a deploy target and an account id, and requires a readable token list; otherwise it
exits 3. The old ids are every `hoist:<owner>/<repo>:deploy` token in that pre-mint list. The
Cloudflare permission groups are resolved (a GET) before the mint, so a 403 there exits 3 with
nothing written. Order: mint, verify the new value (`GET /user/tokens/verify` with the new token),
seal it into the secret, revoke the old ids. Verification tries up to 3 times about 1 s apart
(capped by `HOIST_MAX_SLEEP_MS`) while the new token is not active or answers 401. After the first
write failures exit 1: a failed mint writes nothing else; a failed verify or seal revokes the new
token and leaves the old secret untouched; a failed revocation (old token, or the new one during
cleanup) is an item naming the token id, never a value, plus the human step "revoke Cloudflare token
<id> in the dashboard". Items: `github/secret/CLOUDFLARE_API_TOKEN` (`ok`, "rotated"; `update`, "failed: ..." when
not rotated) and one `cloudflare/api_token/hoist:<owner>/<repo>:deploy` item per revoked or
failed token, keyed by name like `plan`'s item, with the id in the reason (`ok`, "revoked token
<id>", or `update`, "failed: revoking token <id>: ..."). A transport error on the mint says the
token may exist and that the secret still holds the previous token; run rotate again.

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

Verified 2026-10-16 (M2 Task 11 read-only dogfood on `keelstackhq/example-app`, session
`GITHUB_TOKEN`, no Cloudflare token, `plan`/`check`/`auth check`/`import` without `--write`, no
write sent):

- Same proxy and plan limits as M1: rulesets 403 (plan limit), Actions secrets, Actions
  permissions and environments 403 (proxy), `/user/installations` 403. `repo_setting` reads work
  (`allow_auto_merge` and `delete_branch_on_merge` both false).
- `import` needs the Cloudflare token whenever the repo has a deploy target (it has no `--area`),
  so on example-app it exits 3; on a copy without `wrangler.jsonc` it exits 1 with two overrides
  (`allow_auto_merge = false`, `delete_branch_on_merge = false`) and three `unknown` items.
- Not verified (no write approval, no Cloudflare token, no plan or proxy access): every write
  endpoint (repo PATCH, ruleset POST/PUT body and merge queue rule, workflow permissions PUT,
  environment PUT and branch policy 303/422, secret PUT with the sealed box and key re-read, Pages
  project and domain POST, the Cloudflare zone error wording, `POST /user/tokens` with the chosen
  permission group names, `DELETE /user/tokens/{id}`, `/user/tokens/verify` activation delay after
  minting) and the Cloudflare permissions the local token needs besides "API Tokens: Edit".

M2 Task 6 decision (not yet verified against the real API): the minted deploy token is scoped to
the account (`com.cloudflare.api.account.<id>`: `*`) with the permission groups `Pages Write`
and/or `Workers Scripts Write`, matched by exact name from `GET /user/tokens/permission_groups`.
Cloudflare cannot scope a Pages permission to one project. The token expires after 365 days; plan
reports it `pending` with the human step `rotate:CLOUDFLARE_API_TOKEN` (`hoist secrets rotate`)
within 30 days of expiry or when it is not active (R16), never `update`. `HOIST_NOW` (RFC 3339)
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
