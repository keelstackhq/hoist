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
hoist release publish [--assets DIR] [--dry-run]  tag, release, publish (CI, release PR merged)
hoist export --tofu <dir>              M4
```

Areas: `github`, `cloudflare`, `npm`, `crates`, `release`. Global flags: `-C <dir>`, `--json`,
`--no-color`, `-v`.

### Exit codes

`0` in sync / success · `1` drift, a failed write or a failed verification ·
`2` human step pending · `3` usage, config, auth or API error (message says how to fix it).

`release plan` writes nothing: like `plan` it is a preview and exits `0` whether or not a package
would release (one `release/version` item per package, `create`, `pending` "awaiting publish
of <version>" (§9.2, R26) or `ok`; R17), and `3` for a config error, a failing
`git`, or a shallow clone or missing tags (no local tag matches a package while `origin` has
one; "fetch full history and tags (fetch-depth: 0)"; it never proposes bumping every package).
Tool paths come from `HOIST_GIT`, `HOIST_NPM`, `HOIST_CARGO`, `HOIST_PNPM`. `git` runs with
`GIT_TERMINAL_PROMPT=0` and `GCM_INTERACTIVE=never`, so it never waits for credentials. `npm`,
`cargo` and `pnpm` run without `GH_TOKEN` and `GITHUB_TOKEN` in their environment (R27); git
keeps them.

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
- `npm`: `access = "public"` (the default; `"restricted"` exits 3 at model read, R25:
  "restricted npm packages are not supported yet: anonymous registry reads cannot see them"),
  `launcher` (bool, default `false`).
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
| npm | trusted publisher | package name | human step `npm-trust:<name>` (`npm trust github <name> --file <workflow> --repo <o>/<r> --env <env> --allow-publish`, `https://www.npmjs.com/package/<name>/access`) | none: a CI token cannot read it (`unknown`) |
| npm | first publish | package name | human step `first-publish:<name>` while `GET {npm}/<name>` is 404 | the registry has the package |
| crates | trusted publisher | crate name | human step `crates-trust:<name>` (`https://crates.io/crates/<name>/settings`) | none: a CI token cannot read it (`unknown`) |
| crates | first publish | crate name | human step `first-publish:<name>` while `GET {crates}/api/v1/crates/<name>` is 404 | the registry has the crate |
| release | environment | `[release] environment` | the github environment write (branch policy = default branch) | read back |
| release | workflow | `[release] environment` | none (`pending` "no workflow job uses environment `<env>`") | a workflow job declares `environment: <env>` |

Settings hoist does not model are never touched.

The `npm`, `crates` and `release` areas exist only when a package publishes to npm or crates.io
or has `github_release` (§4); a repo without such packages plans exactly as before. Then the
release environment joins the workflow-inferred environments once and is reported in the
`release` area only (not again under `github`); `apply` creates it with the same environment
write. The workflow file in the trust commands is the file of the first job that declares
`environment: <env>` (`release.yml` when none does). Registry reads are anonymous GETs (crates.io
with `User-Agent: hoist/<version> (+https://github.com/keelstackhq/hoist)`; a scoped npm name is
`@scope%2Fname`); a registry 5xx or transport error makes that package's `first_publish` item
`unknown`, never exit 3. `--area npm` and `--area crates` read only their registry; `--area
release` reads GitHub. Only `plan`, `check` and `apply` read the release model (R18): an error in
it (a pre-release version, a bad `tag_format`) exits 3 only with an explicit `--area
npm|crates|release`; with no `--area` it is one item `release/model` (`unknown`, the error as
reason) and the GitHub and Cloudflare areas proceed; `--area github|cloudflare` ignore it.
`auth check`, `import`, `secrets` and `explain`'s environment list never read it.

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
action without an API) is a `pending` item with a title, a URL and a check. A trusted publisher
is the exception: no check can read it with a CI token, so its item is `unknown` ("registered by
a person; hoist cannot read it with a CI token") and its human step stays listed without holding
`check` at exit 2 (R6); a missing publisher shows at publish time. A first publish (the
registry does not have the package yet) is `pending` with the step `first-publish:<name>`. `apply` exits 2 while
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
`hoist/release`, title `release: <name>@<version>, …` in dependency order). The flow:

1. Preflight, exit 3: a clean working tree (no untracked files), HEAD on the default branch, a
   GitHub token, not a shallow clone (and no missing tags, §9.1). The plan, every file edit and
   `GET /repos/{o}/{r}/pulls?head=<o>:hoist/release&state=open` also happen before any write, so
   an editor error or an unreadable PR list is exit 3 with nothing written.
   R24: the default branch is `origin/HEAD`; when the checkout has none (actions/checkout),
   it is `default_branch` from `GET /repos/{o}/{r}`, read before any write (`release plan`
   keeps the local fallback `main`).
2. Nothing releasable: an open release PR is closed (`PATCH … {"state":"closed"}`) and the remote
   branch deleted (`git push origin --delete refs/heads/hoist/release`). A branch that is already
   gone counts as deleted: `git ls-remote --exit-code origin refs/heads/hoist/release` exit 2
   skips the delete, and a delete rejected with "remote ref does not exist" / "non-existent ref"
   is success. Item `release/pr` is `ok`, exit 0.
3. Otherwise: `git checkout -B hoist/release`; write the edits (versions; internal dependents'
   requirements: npm `^<new>` unless the spec is `workspace:`, Cargo the exact new version in the
   dependent's `Cargo.toml` and, for `workspace = true` deps, in the root
   `[workspace.dependencies]`; `CHANGELOG.md` per package); `pnpm install --lockfile-only` when an
   npm package changed and `cargo update -w` when a crate changed (their stdout goes to stderr);
   `git commit -m "release: <list>"` (identity `github-actions[bot]` when the checkout has none);
   `git push --force origin HEAD:refs/heads/hoist/release` (the branch belongs to hoist and is rebuilt from the
   default branch each time; git uses the checkout's credentials, never a token in argv); check
   out the default branch again. R27: when the remote `hoist/release` exists
   (`git ls-remote --exit-code`) and, after `git fetch origin refs/heads/hoist/release`, its tree
   equals the new commit's tree, the push is skipped and `release/step/push` is `ok`
   "unchanged" (the success report carries the `push` step item).
   An inherited Cargo version (`version.workspace = true`) is bumped in the root
   `[workspace.package] version` only when every workspace member inheriting it, published or
   not, gets the same new version; otherwise exit 3 before any write, naming the crates (R15,
   R16).
4. `POST /repos/{o}/{r}/pulls` (base = default branch) or `PATCH /pulls/{n}` with the new title and
   body. The body lists each package `from → to` with its tag, the changelog entries and the tags
   `publish` will create. Items: `release/version/<name>` per released package (`update`, "in
   release PR #n") and `release/pr/hoist/release` (`create` or `update`). Exit 0.
5. A failure after the first write is exit 1. The report has one `release/step` item per step
   (`branch`, `files`, `lockfiles`, `commit`, `push`: `ok` with what was done, `update` "failed:
   <error>", or "not attempted"), `release/step/restore` (the default branch is checked out again
   with `-f` and files hoist created are removed; `unknown` if that fails) and `release/pr`
   ("not attempted" or "failed: <error>").

Awaiting publish (R26): a package whose manifest version is above its last tag and has no tag
of its own (the release PR was merged, `release publish` has not tagged it yet) gets no bump:
`release plan` shows it `pending` "awaiting publish of <version>", `prepare` proposes nothing for
it (so a run before publish does not bump it twice) and closes the PR when nothing else
releases. Its dependents are not bumped for it. A launcher awaits with its crate. With
`versioning = "fixed"` nothing releases while any package awaits publish.

PRs opened or pushed with the default `GITHUB_TOKEN` do not trigger workflows; pass a token that
can (`GH_TOKEN`, and the same token to `actions/checkout`) for the release PR to run checks.

### 9.3 Publish

When the release PR is merged, `hoist release publish` runs in the `release` environment, in
dependency order, each step skipped if already done (re-runs are safe). A package is pending when
no local tag for its manifest version exists on an earlier commit (`git tag --list` minus
`git tag --points-at HEAD`): a tag on HEAD is this release's own, so a re-run re-checks every
step of that package instead of skipping it. With nothing pending the report is one
`release/publish/pending` item, `ok`, exit 0. Per pending package:

1. tag: `GET /repos/{o}/{r}/git/ref/tags/<tag>`; at `GITHUB_SHA` it is done, at another commit
   the run stops ("tag exists at another commit", exit 1), else `POST /git/refs` creates a
   lightweight tag on `GITHUB_SHA` (no `git push`). An annotated tag (`object.type == "tag"`) is
   peeled with `GET /git/tags/<sha>` and its `object.sha` is the commit compared, in the step and in
   verification;
2. GitHub Release: `GET /releases/tags/<tag>`; published with every asset matching is done.
   Otherwise reuse the draft for the tag from `GET /releases?per_page=100&page=N` (paging stops
   at the first matching draft or a short page) or create one (`POST /releases`, `draft: true`, `name` = the tag,
   body = the package's `CHANGELOG.md` section `## <version> (` up to the next `## `, else
   "Release <tag>."), upload the missing assets and then `SHA256SUMS` (lines `<hex>  <file>`) to
   `{HOIST_GITHUB_UPLOADS}/repos/{o}/{r}/releases/{id}/assets?name=<file>` as
   `application/octet-stream`, then `PATCH /releases/{id} {"draft": false}`. R20: an existing
   asset counts only when `state == "uploaded"` and its `digest` (`sha256:<hex>`, else its `size`)
   matches the local file; a draft's stale asset is deleted
   (`DELETE /releases/assets/{id}`) and uploaded again, a 422 (`already_exists`) on upload re-lists
   `GET /releases/{id}/assets` and applies the same check, and a published release with a missing
   or mismatched asset gets no write: exit 1 "published release <tag> lacks/mismatches <asset>;
   releases are immutable — release a new version". Assets are
   `<assets>/<bin>-<target>` (`.exe` for `*-windows-*`) per `github_release` target, `bin`
   defaulting to the package name; a package without targets gets a release without assets and
   without `SHA256SUMS`. A missing file (or no `--assets` when one is needed) is exit 3 before any
   request;
3. npm (packages with `[publish] npm`): `GET {npm}/<pkg>/<version>` 200 is done ("already on
   npm"). Otherwise `GET {npm}/<pkg>` 404 makes the step `pending` with the human step
   `first-publish:<name>` and no publish (trusted publishing needs an existing package); the
   run goes on with the other steps and packages. Else `npm --version` must be ≥ 11.5.1 (the
   first `MAJOR.MINOR.PATCH` in its output; unreadable counts as older; else the step fails),
   then `npm publish --access <access>` runs in the package directory with no extra env: npm
   does the OIDC exchange itself and adds provenance;
4. crates.io (crates with `[publish] crates`): the same done / `pending` checks on
   `{crates}/api/v1/crates/<name>[/<version>]`. Else an Actions OIDC token for audience
   `crates.io` (`GET $ACTIONS_ID_TOKEN_REQUEST_URL&audience=crates.io`, `Authorization: bearer
   $ACTIONS_ID_TOKEN_REQUEST_TOKEN`, `.value`) is exchanged with `POST
   {crates}/api/v1/trusted_publishing/tokens {"jwt": …}` for a short-lived token (`.token`),
   `cargo publish -p <name> --locked` runs at the repo root with it in `CARGO_REGISTRY_TOKEN`
   (env only, never argv; R21: its stdout and stderr are captured and every secret value passed
   in its env is replaced by `***` before they reach hoist's stderr), then `DELETE {crates}/api/v1/trusted_publishing/tokens` revokes it,
   also when cargo fails. A failed revoke is a `release/crates_token/<tag>` item, `unknown`, and
   does not fail the publish;
5. deploy: for a target in the package's directory with `[deploy] on = "release"`,
   `pnpm --filter <package name> run deploy` at the repo root. Deploys have no done-check, so a re-run of
   publish at the same commit runs them again.

Tool stdout is echoed to stderr, also when the tool fails. Before any request, when a pending package publishes to npm or
crates.io, `ACTIONS_ID_TOKEN_REQUEST_URL` and `ACTIONS_ID_TOKEN_REQUEST_TOKEN` must be set, else
exit 3 "the job needs `permissions: id-token: write`". A registry read that fails (not 200/404),
of the version or of the package, fails the step before any OIDC request or publish. Tokens and the OIDC JWT never reach stdout,
stderr (also with `-v`), JSON or error messages.

Binaries come from a matrix job in hull's `release.yml` that builds `[publish] github_release`
targets and uploads them as workflow artifacts; `publish` attaches them.

npm launcher (splice's pattern): an npm package with `[publish] npm = { launcher = true }` is a
launcher. Its command is the single key of `bin` in its package.json (or its unscoped name when
`bin` is a string); the crate whose `github_release.bin` (or name) equals it supplies the
targets. Each target maps to a platform package `<launcher dir>-<platform>/` next to the launcher
(`npm/<dir>-<platform>/` for a launcher in `npm/<dir>`): `x86_64-unknown-linux-musl` →
`linux-x64`, `aarch64-unknown-linux-musl` → `linux-arm64`, `x86_64-apple-darwin` → `darwin-x64`,
`aarch64-apple-darwin` → `darwin-arm64`, `x86_64-pc-windows-msvc` → `win32-x64`. No such crate,
a crate without targets, a target without a mapping, a `bin` with several commands or a missing
platform `package.json` (it needs `name` and `version`) exits 3 at model read. Platform packages
are never released on their own: they leave the model even when the workspace globs match them,
and leave every dependency list. The launcher shares its crate's version: it releases exactly
when the crate does, at the crate's new version (its own commits release nothing; a pin on it is
an error naming the crate), and has no tag, changelog or GitHub Release of its own, so
`v{version}` tag formats stay collision-free. `release plan` shows it as
`<from> -> <to> (follows <crate>)`; `release prepare` sets the launcher's and its platform
packages' `version` and the launcher's `optionalDependencies` on them (exact version) and lists
it in the PR table with no tag. R23: `optionalDependencies` must name exactly the platform
packages at plain versions; prepare exits 3 before any write on a `workspace:` spec (npm publish
does not rewrite it), an entry that is no platform package, or a missing entry. `release publish` runs the launcher as a
job right after its crate's job, whenever the crate is pending (R19): first each platform package
in target order, then the launcher, each an npm step with the same done / `pending` checks as
step 3, so a re-run skips what is already on npm. While any platform package is `pending`, the
launcher's step is skipped: `pending` "platform packages not published yet" (R27). A platform step copies
`<assets>/<bin>-<target>[.exe]` to `<platform dir>/bin/<bin>[.exe]` (directory created, mode 0755
on unix) only when it publishes, right before `npm publish`, and removes it again (and a `bin/`
it created) afterwards, on success or failure, so the checkout stays clean; a failed copy or
removal fails the step. Before any write, the launcher and every platform package must carry the
crate's version, and the launcher's `optionalDependencies` must name exactly the platform
packages, each at that version, else exit 3 "run `hoist release prepare`". Items:
`release/npm/<platform pkg>@<version>` per platform, then `release/npm/<launcher>@<version>`.
A platform package must ship `bin/`: when the repo's `.gitignore` ignores `bin/` (npm then
leaves it out too), give the platform package.json a `files` field that includes `bin/`. R22:
`plan`/`check` list a `trusted_publisher` item (`unknown`, step `npm-trust:<pkg>`) and a
`first_publish` item (step `first-publish:<pkg>` when absent) for every platform package too,
right after its launcher, since each needs its own manual first publish and trust registration.

Items: `release/tag/<tag>`, `release/github_release/<tag>` and, where configured,
`release/npm/<tag>`, `release/crates/<tag>` and `release/deploy/<tag>` per pending package, `ok`
with "already …" or "created …"/"published …"/"ran …", or `pending` (first publish). The first
failed step stops the run: that step is `update` "failed: <error>", later steps (of every
package) `update` "not attempted", exit 1. Human steps alone (first publishes) exit 2. `--dry-run` skips the
preflight, sends only GETs and reports each step as `create`, `update` (a draft that needs
assets), `ok` or `drift` (a tag at another commit, or an incomplete published release); registry
steps are `ok` (version there), `pending` (no package yet, with the human step), `create` or
`unknown` (registry unreadable), deploys `create`; it always exits 0 (R17) apart from exit 3
errors.

### 9.4 Safety

Publishing runs only from the default branch in the `release` environment. Before anything else
(no write is sent; the only request is R24's `GET /repos/{o}/{r}` when the checkout has no
`origin/HEAD`) `publish` checks, in order, and exits 3 naming the failed condition:
`GITHUB_ACTIONS=true`; `GITHUB_REF` is `refs/heads/<default branch>`; `HEAD` equals `GITHUB_SHA`;
the job named by `GITHUB_WORKFLOW_REF` (`<o>/<r>/.github/workflows/<file>@<ref>`, the file name)
and `GITHUB_JOB` declares `environment: <[release] environment>` in that workflow file. A shallow
clone is exit 3 as in §9.1. hoist provisions that
environment (branch policy) and registers trusted publishers for exactly that repo, workflow file
and environment, so a fork or another workflow cannot publish. No publish token exists.

### 9.5 Verification

After publishing: the version is visible on each registry, the release has all assets and
checksums, the tag points at the merge commit. Each step item gets `verify`: the tag's sha
equals `GITHUB_SHA`, the release is published with every expected asset plus `SHA256SUMS`, and
for npm and crates.io `version_exists` answers 200 within 5 reads 2 s apart (the sleep is
capped by `HOIST_MAX_SLEEP_MS`). Deploy items carry no `verify`. A failure is exit 1 with what
is missing in `verify.detail`.

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

hoist's own setup: the root `hoist.toml` sets `[publish] crates = {}` and `github_release`
(targets `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`,
`aarch64-apple-darwin`, `x86_64-pc-windows-msvc`, `bin = "hoist"`) for the root crate
`keel-hoist` (tags `v{version}`); `npm/hoist` is the launcher (`npm/hoist/hoist.toml`:
`[publish] npm = { launcher = true }`; `bin/hoist.js` maps `process.platform`-`process.arch` to
`@keelstackhq/hoist-<platform>`, spawns its `bin/hoist[.exe]` with inherited stdio and exit code,
and exits 1 listing the supported platforms otherwise) with platform packages
`npm/hoist-<platform>`. `pnpm-workspace.yaml` lists `npm/*` so inference sees them
(`linkWorkspacePackages: true`, so `pnpm install --lockfile-only` in `prepare` resolves the
unpublished platform versions locally). `.github/workflows/release.yml` runs on pushes to `main`
and `workflow_dispatch`; no step that holds `GH_TOKEN` compiles anything:
`detect` (pending when `Cargo.toml`'s version has no tag `v<version>` or that tag is on HEAD, the
R19 rule; not the head commit message, which reads "Merge pull request …" after a merge), `build`
(pending only; a 5-target matrix on native runners uploading `hoist-<target>[.exe]`, checkout
with `persist-credentials: false`, `CC_<musl target>=musl-gcc` on the musl targets), `publish`
(pending only; `environment: release`, `contents: write`, `id-token: write`, checkout with
`persist-credentials: false`, Node 24 and npm 12.2.0, artifacts into the ignored `dist/`, then
the shipped `dist/hoist-x86_64-unknown-linux-musl release publish --assets dist`) and `prepare`
(`cargo build --release --locked` in a step without `GH_TOKEN`, then `target/release/hoist
release prepare`; `contents: write`, `pull-requests: write`, `fetch-depth: 0`, `GH_TOKEN` =
`HOIST_RELEASE_TOKEN` or `GITHUB_TOKEN`; it runs after `publish` when that ran, so the new tag is
seen and no bogus release PR is opened for the version just merged).

## 13. Facts to verify before the milestone that needs them

Checked or to check with real API calls and dated in `docs/PROGRESS.md`:

- GitHub: rulesets API fields for required status checks and merge queue; default workflow
  permissions endpoint; environment branch policies; reading app installations for a repo with a
  user token (may need the installation list of the org).
- Cloudflare: Pages project create/read API, custom domain attach, API token create with resource
  scoping to specific Pages projects (may only scope to the account; then document it).
- npm and crates.io releasing: see "Researched 2026-10-17" below.

Researched 2026-10-17 (docs and source, not yet exercised against the real services). The plan
carried no URL list; the sources below are the pages that document each fact. Checked on
2026-10-17: the docs.npmjs.com, github.blog and docs.github.com pages answer 200; the crates.io
and github.com pages answered 403 through the sandbox proxy and were not read in this task.

- npm trusted publishers can be created only by a logged-in user with 2FA, with `npm trust github
  <pkg> --file <workflow> --repo <o>/<r> --env <env> --allow-publish` (npm ≥ 11.15.0) or the web
  UI; a CI token can neither create nor read them (hence `trusted_publisher` is `unknown`, R6,
  and registration is the human step `npm-trust:`). The package must exist first, so the first
  publish is manual. `npm publish` (npm ≥ 11.5.1, Node ≥ 22.14.0) performs the OIDC exchange
  itself when the job has `id-token: write`; provenance is automatic for public repos with public
  packages. Sources: https://docs.npmjs.com/trusted-publishers,
  https://docs.npmjs.com/cli/v12/commands/npm-trust/,
  https://github.blog/changelog/2025-07-31-npm-trusted-publishing-with-oidc-is-generally-available/.
- crates.io trusted publishing is stable; the crate must exist first (manual first publish).
  Token exchange: an Actions OIDC JWT with audience `crates.io` is posted to
  `POST /api/v1/trusted_publishing/tokens`; the response holds a short-lived token that is revoked
  after use. The publisher configs API exists in source only, so registration is the human step
  `crates-trust:`. Sources: https://crates.io/docs/trusted-publishing,
  https://blog.rust-lang.org/2025/07/11/crates-io-development-update-2025-07,
  https://github.com/rust-lang/crates.io (`src/controllers/trustpub`),
  https://github.com/rust-lang/crates-io-auth-action.
- Registry reads: `GET {npm}/<pkg>/<version>` answers 200 when the version exists and 404
  otherwise; `GET {crates}/api/v1/crates/<name>/<version>` likewise, and needs a `User-Agent`
  (403 without); `GET {npm}/<pkg>` and `GET {crates}/api/v1/crates/<name>` answer 404 when the
  package does not exist. Sources: https://github.com/npm/registry/blob/main/docs/REGISTRY-API.md,
  https://crates.io/data-access.
- Actions OIDC: `GET $ACTIONS_ID_TOKEN_REQUEST_URL&audience=<aud>` with `Authorization: bearer
  $ACTIONS_ID_TOKEN_REQUEST_TOKEN` answers `{"value": "<jwt>"}`. Source:
  https://docs.github.com/en/actions/reference/security/oidc.
- GitHub Releases: a draft is created, assets are uploaded to `{uploads}/repos/{o}/{r}/releases/
  {id}/assets?name=`, then `PATCH {"draft": false}` publishes; this works with immutable
  releases, which lock assets and the tag once published. Tags are lightweight refs created with
  `POST /repos/{o}/{r}/git/refs`. Sources: https://docs.github.com/en/rest/releases/releases,
  https://docs.github.com/en/rest/releases/assets, https://docs.github.com/en/rest/git/refs,
  https://docs.github.com/en/code-security/supply-chain-security/understanding-your-software-supply-chain/immutable-releases.

Unverified (hoist's choices rest on these; the first real release exercises them): whether
`pnpm publish` supports OIDC (hoist calls `npm publish`); the crates.io exchange body field name
`jwt`, the response field `token`, the revoke call (`DELETE /api/v1/trusted_publishing/tokens`
with `Authorization: Bearer <token>`) and the token lifetime; the shape of the npm trust read
response (hoist does not read it). Also unverified: the `GET` of the Actions OIDC token from a
real job, and every release write endpoint against real GitHub.

Read-only dogfood 2026-10-17 on hoist itself (`hoist release plan`, `release publish --dry-run`,
`plan --area release --area npm --area crates`; session `GITHUB_TOKEN`, sandbox proxy; registries
read anonymously): both registries answered 404 for `keel-hoist` and every `@keelstackhq/hoist*`
package (nothing is published yet); the `release` environment read was 403 through the proxy
(`unknown`). `publish --dry-run` outside Actions needs `--assets DIR` with all binaries (exit 3
naming the first missing one); with placeholder files it reports the planned tag, draft release
with 6 assets and `pending` first publishes, exit 0.

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
