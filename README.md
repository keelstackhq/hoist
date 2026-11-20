# hoist

Provision and verify a repo's GitHub and Cloudflare setup from what the repo already says
(workflows, `wrangler.jsonc`, `package.json`, `renovate.json`, optional `hoist.toml`). A single
static Rust binary. Part of the keelstackhq family (hull, splice, hoard, hoist, berth, bosun).

Status: M3 (releasing) done. `apply`, `secrets set|rotate`, `release prepare` and `release
publish` write (after a preflight); every other command, including `import`, `release plan` and
`release publish --dry-run`, only sends GET requests.
Design: `docs/superpowers/specs/2026-10-16-hoist-design.md`.

## Install

From source (Rust 1.97, see `rust-toolchain.toml`):

```bash
cargo install --path .
```

Once released (see Releasing): `npm install -g @keelstackhq/hoist`, `cargo install keel-hoist`,
or a binary from the GitHub Release.

## Commands

```bash
hoist explain                 # desired model and the file each item came from
hoist plan [--adopt] [--area github|cloudflare|npm|crates|release]  # diff desired vs live, no writes
hoist check [--adopt] [--area ...]  # plan plus verification (required checks, Pages URL)
hoist apply [--yes] [--prune | --adopt] [--area ...]  # write the plan, then verify like check
hoist auth check              # what each token can read vs what hoist needs
hoist secrets set NAME        # seal env NAME or a hidden prompt into an Actions secret
hoist secrets rotate [NAME]   # mint, verify and seal a new CLOUDFLARE_API_TOKEN, revoke the old
hoist secrets check [--remote] # secrets repo or secrets.toml; never decrypts (spec §15.9)
hoist secrets exec --env E --keys A,B -- cmd  # CI only: run cmd with those secrets (spec §15.7)
hoist secrets edit FILE         # secrets repo: sops edit with your own age key, then the git commands
hoist secrets grant OWNER/REPO --env E [--name N] [--replace | --public AGE1]  # secrets repo: a consumer env's key, sealed; prints the git commands
hoist secrets repo init OWNER/NAME [--require-check C] [--dry-run]  # a private secrets repo with its ruleset
hoist secrets revoke NAME [--delete-secret]  # secrets repo: remove a recipient, new data keys, list the values to rotate
hoist secrets rotate-key NAME [--public AGE1]  # secrets repo: routine rotation of a consumer key, new data keys
hoist secrets sync [--rotate]  # secrets repo: .sops.yaml from access.toml, re-key changed files; --rotate: new data keys, then exit 2 until you rotate the values
hoist import [--write] [--area ...]  # hoist.toml overrides that match the live state; --write writes them
hoist init [--sha SHA] [--force] [--dry-run]  # write release.yml (thin call of the release action) and, if the repo has no `ci-ok` job, ci.yml; never commits
hoist release plan            # next versions from conventional commits, read-only (exit 0)
hoist release prepare         # open/update the release PR on branch hoist/release, or close it
hoist release publish [--assets DIR] [--dry-run]  # tags, GitHub Releases, npm, crates.io, deploys (CI only)
hoist release bootstrap [--dry-run] [--yes]  # first publish + trusted publishers, locally with your logins
```

`hoist release prepare` needs a clean tree on the default branch and full history. It rebuilds
the branch `hoist/release` from the default branch (versions, dependents, `CHANGELOG.md`,
lockfiles), force-pushes it to `origin` with the checkout's git credentials and opens or updates
the PR; when nothing releases it closes the PR and deletes the branch. A package whose manifest
version is above its last tag but not tagged yet (the PR was merged, publish has not run) is
"awaiting publish": `release plan` shows it `pending` and `prepare` proposes no second bump for it.
When the remote `hoist/release` already has the same tree, the push is skipped ("unchanged").
Without `origin/HEAD` (actions/checkout) the default branch comes from the GitHub API. Exit 3 before any write, 1
when a write fails (the report lists what was done). PRs opened or pushed with the default
`GITHUB_TOKEN` do not trigger CI: pass a token that can (`GH_TOKEN`, and the same token to
`actions/checkout`) for the release PR to run checks. The GitHub token needs Pull requests: write.

`hoist release publish` runs only in GitHub Actions on the default branch, at `GITHUB_SHA`, in a
job that declares `environment: <[release] environment>` (else exit 3 before any request). For
every package whose version has no tag on an earlier commit it creates the tag on `GITHUB_SHA`
through the API and a draft GitHub Release with the changelog section, uploads the binaries
`<DIR>/<bin>-<target>` plus `SHA256SUMS`, then publishes the draft. Then npm packages are
published with `npm publish --access public` (npm ≥ 11.5.1, OIDC trusted publishing, no token;
a stable publish then removes a leftover `next` dist-tag unless it is newer than the published version (skip with `HOIST_KEEP_NEXT_TAG=1`; an npm OIDC publish token may not be allowed `dist-tag rm`, in which case the item notes it: verify on the next release);
`[publish] npm = { access = "restricted" }` exits 3: restricted npm packages are not supported
yet, because anonymous registry reads cannot see them)
and crates with `cargo publish -p <name> --locked` using a short-lived crates.io token exchanged
for the job's OIDC token and revoked afterwards; targets in the package's directory with
`[deploy] on = "release"` run `pnpm --filter <name> run deploy`. The job needs `permissions:
id-token: write` (else exit 3). A package that is not on its registry yet is `pending` with the
human step `first-publish:<name>` (exit 2); the others still publish. Finished steps are skipped
on a re-run, and every step is verified afterwards, registries by up to 10 reads over about 5
minutes, since npm can take minutes to serve a new version (exit 1 on a failed step or
verification; the table lists each failed check under `failed checks:`). The GitHub token needs
Contents: write. `--dry-run` sends GET requests only and runs anywhere. npm, cargo and pnpm run without `GH_TOKEN`/`GITHUB_TOKEN`, and
`cargo publish` output is shown with the crates.io token replaced by `***`. A launcher waits
(`pending`) while one of its platform packages is not published yet.

Requirements of the publish job: npm ≥ 11.5.1 and Node ≥ 22.14.0 (npm's trusted publishing
needs both; hoist checks only the npm version, and fails the step when it is older), `permissions:
id-token: write`, and `actions/checkout` with `fetch-depth: 0`.

If a publish job fails, fix the cause outside the repo (a registry setting, a trusted publisher,
a token) and **re-run the failed job**; do not push a fix commit. After a new commit the release
tag is no longer on HEAD, so that version counts as released and its remaining steps are skipped.
Finished steps are skipped on the re-run.

GitHub Releases are created as drafts, and the release is published only after every asset and
`SHA256SUMS` are uploaded (`PATCH {"draft": false}`), so the flow also works with immutable
releases (a published release cannot change). A re-run repairs a draft; a published release with a
missing or different asset is not touched (exit 1: release a new version). Enabling immutable
releases is optional (repository settings).

First publish and trusted publishers are human steps, because both registries need the package to
exist and only a person can register a publisher. `hoist plan --area release --area npm --area
crates` lists them as human steps (`first-publish:<name>`, `npm-trust:<name>`,
`crates-trust:<name>`); the registered publisher itself is `unknown`, since hoist cannot read it
with a CI token.

Do both with `hoist release bootstrap`, locally, after the release workflow has tagged the
first version (`npm login`, `cargo login`, npm ≥ 11.15.0, a terminal for npm's 2FA; see
"Releasing" below). It publishes each missing npm package (launchers and every platform package
included) once as a placeholder `0.0.0` from a temp dir, publishes each missing crate at its
release tag, and registers the npm trusted publishers (`npm trust github <package> --file
release.yml --repo <owner>/<repo> --env release --allow-publish`). Never publish a real npm
version by hand: npm versions are immutable, and a platform package published from the checkout
has no `bin/` (hoist copies the binary in only during `release publish`). crates.io trusted
publishers stay a human step: the crate's Settings, Trusted Publishing, add repository
`<owner>/<repo>`, workflow `release.yml`, environment `release`. Then re-run the failed
`release publish` job within 2 days (npm expires an unused trusted publisher). `hoist apply
--area release` creates the `release` environment with you as its required reviewer (`hoist plan`
shows it as `release/environment`, and as `update` when the environment exists without a required reviewer); run it before the first workflow run. The approvers are the logins in `[release] reviewers` (resolved before any write; an unknown login is exit 3), else the token's user, which needs a user token (an App installation token has no user: exit 3 before any write); a bot token user is never made the sole approver: the environment is created without one and the human step `release-reviewer` (exit 2) asks for a required reviewer; where the plan allows no required reviewers (private repo) the environment is created without one and reported as `drift` (only when GitHub's 422 names reviewers or protection rules; any other 422 is a failed write).

`hoist import` sends GET requests only. It prints the `hoist.toml` lines (kind `override`, key
`<file>:<table>.<key>`) that make `plan` match the live settings: `[github] visibility` (when
pinned), `allow_auto_merge`, `delete_branch_on_merge`, `actions_permissions`, `required_checks`
and `merge_queue` (from the live `hoist: default branch` ruleset), and `[deploy] project` for a
Pages target whose inferred project is missing when exactly one live project starts with the
target name. Other live rulesets show as comments. A field it cannot read is `unknown` and is
not imported. `--write` edits the files in place (a missing file is created; comments, order and
other keys are kept; equal values leave the file untouched). `--area github|cloudflare` limits
the reads like `plan`: `import --area github` needs no Cloudflare credentials. Exit codes: 0
nothing to import or written, 1 overrides found without `--write`, 3 config, auth or API error.

Global flags: `-C <dir>` (repo to inspect), `--json`, `--no-color`, `-v`. `--json` prints exactly
one compact (single-line) object on stdout; logs go to stderr. Actions are `create`, `update`, `ok`, `drift`,
`pending`, `unknown` (`unknown` means hoist could not read the resource, for example a 403).
A rejected token (401) or a repo the token cannot see (404) is an error (exit 3), not `unknown`.
With `--json`, an exit-3 error (usage errors too) still prints one object on stdout:
`{"version":1,"command":…,"ok":false,"exit_code":3,"items":[],"human_steps":[],"error":{"message":…,"fix":…}}`;
the text stays on stderr. A usage error also prints the hoist version and a `which -a hoist` hint
(an older hoist earlier on `PATH` may be the one that rejected the flag); the exit code stays 3.

`check` accepts a required check that is a commit status rather than a job: the built-in bosun
set (`main-open`, `review-verdict`, `risk-check`) plus `[github] statuses = ["name"]`. Mark
secrets a workflow names but that may stay unset with `[github] optional_secrets = ["NAME"]`:
they show as `optional_secret` items (action `ok`), not as pending human steps.

Secrets are checked for presence only: a workflow secret counts as present when it is a
repository secret or an organization secret the repo can use. Environment secrets are not
checked in M1. The merge queue is expected on when a workflow has a `merge_group` trigger
(override with `[github] merge_queue`).

### Repository variables

A workflow's `${{ vars.NAME }}` (or `vars['NAME']`, any case; the name is upper-cased) is a
repository variable that must exist. hoist reads `GET .../actions/variables` and the organization
variables the repo can use (a 403 there counts as none, with a note; a 404 is none) and reports a
`variable` item per name. A missing one with no value in `hoist.toml` is `pending` with a human
step (exit 2 on `check`) that gives the command:
`gh variable set NAME --repo <owner>/<repo> --body '<value>'`. A variable read only by jobs with an
`environment:` also counts as present when that environment (if it exists) holds it
(`GET .../environments/<env>/variables`, reason "present (environment production)"), and its
command is then `gh variable set NAME --env <env> --repo <owner>/<repo> --body '<value>'`; a job
without an environment needs the repository variable. Give the value in `hoist.toml`
instead and `hoist apply` sets it (a missing variable is `create`, a different value `update`);
values are not secret and show in the report. Names must be letters, digits and `_` (no leading digit,
no `GITHUB_` prefix, no two keys that differ only in case), else exit 3:

```toml
[github.variables]
API_URL = "https://api.example.com"

[github]
optional_variables = ["REGION"]    # may stay unset: an `optional_variable` item, never a step
```

### GitHub environments

Every environment a workflow names is planned, created and checked: a job's `environment:` (a
name or `{name: ..., url: ...}`), or `with: {environment: <name>}` on a job that calls a reusable
workflow. An environment name that is an expression (`${{ ... }}`) cannot be known before the run:
it is an `unknown` `environment` item naming the `file:line` (once per expression); list the real names in `hoist.toml`.
(`environment: ${{ inputs.X }}` in a local reusable workflow is skipped when every caller passes a literal `with: X:`.)
Each environment is created with a deployment branch policy for the default branch.

```toml
[github.environments.production]
branches = ["main", "release/*"]   # default: the default branch; [] = no branch restriction

[github.environments.splice-bot]
manage = false                     # hoist leaves this one alone (an `ok` "not managed" item)

[github.environments.canary]       # listed only here: created too
```

Unknown keys under `[github.environments.<name>]` are an error (exit 3).

An environment that exists is read (GET only: the environment list, and its
`deployment-branch-policies` when it uses custom policies). One that lacks a wanted branch is
`update` ("the deployment branch policy lacks `hotfix/*`; also allowed (kept): `legacy`"); one with
no policy at all is `update` too (any branch can deploy). `apply` adds the missing policies (a
`POST` each, no `PUT`: reviewers and the other settings stay) and never deletes extra ones; an
environment with no custom policy is updated with a `PUT`, after a `GET` of it: its wait timer,
self-review rule and reviewers are read and sent back, so they are kept (an environment hoist cannot
read first is not changed). Under `--adopt` the gap is `drift` and
nothing is written. The `release` environment is not read for branch policies.

## Apply

`hoist apply [--yes] [--prune] [--area ...]` plans, writes, then verifies like `check`.

1. Preflight, before the first write (any failure exits 3): GitHub writes need
   `permissions.admin: true` on `GET /repos/{owner}/{repo}` ("the token needs admin on
   <owner>/<repo>"); Cloudflare writes need an account id and a token that `GET /user/tokens/verify`
   reports `active`; when a deploy token is to be minted, its permission groups are resolved here.
2. Confirmation: on a terminal the change list goes to stderr and `y` or `yes` continues. Any other
   answer writes nothing and reports every change as `not applied: declined` (exit 1). Without a
   terminal and without `--yes`, exit 3 ("use --yes in non-interactive runs"). With no changes,
   nothing is written and the `check` result is reported.
3. Writes run in dependency order: Cloudflare project, domain, minted token (sealed into
   `CLOUDFLARE_API_TOKEN`), `CLOUDFLARE_ACCOUNT_ID` secret, environment, ruleset, repo settings,
   Actions permissions, then token revocations (`--prune` only: stale
   `hoist:<owner>/<repo>:*` tokens, never the live deploy token). Every write is idempotent.
4. Partial failure: the first failed write stops apply and exits 1. The report marks each change
   `applied`, `failed: <error>` or `not attempted`, plus the plan's other items. Run `apply` again
   to continue; changes already in place are `ok`. One exception: GitHub Free refuses rulesets
   on a private repo (a 403 "Upgrade to GitHub Pro or make this repository public"), and classic
   branch protection and the merge queue too. Then the ruleset item is `drift` ("NOT enforced:
   required checks `ci-ok`; GitHub Free: …; upgrade the plan … or make the repo public"),
   the other writes still run, and apply exits 1. `plan` and `check` report the same drift when
   the ruleset read gets that 403.
5. After the writes hoist re-reads the live state and runs the `check` logic; that is the final
   report (`command: "apply"`). A Pages project or domain created in this run is not probed
   (`ok`, "created in this run; verified by `hoist check` after the first deploy"). If the re-read
   fails, the report lists the applied changes plus a `post_apply_check` item (exit 1).

Exit codes: 0 in sync, 1 a failed write, a declined prompt, or a change or failed verification
still open, 2 a human step is pending (for example a domain whose zone is not on the Cloudflare
account, a secret hoist cannot produce, or a token due for rotation), 3 preflight, usage, config or
auth error before the first write.

Settings hoist does not model are never touched, with one exception: a ruleset update replaces
the whole `hoist: default branch` ruleset, so rules hoist does not write are dropped from it.
Deletes happen only with `--prune`.

`--adopt` sets up an existing repo without loosening anything: live required checks, a merge
queue, read-only workflow permissions and a more private visibility are kept ("kept stricter live
setting", exit 0), and a ruleset update that would drop rules hoist does not write is drift
(exit 1) and not written. A missing deploy token is minted only when no older hoist deploy token
exists (else a `hoist secrets rotate` step, exit 2), and an existing Pages project with another
production branch is drift. It refuses `--prune`. `plan
--adopt` and `check --adopt` preview and check the same view (spec §7.6).

### DNS records

`[cloudflare.dns]` in `hoist.toml` keeps a host's address records in a Cloudflare zone:

```toml
[cloudflare.dns]
zone = "example.com"
host = "app.example.com"               # the zone itself or a name under it
targets = ["203.0.113.7", "2001:db8::7"]  # A for IPv4, AAAA for IPv6; or "fly"
# fly_app = "my-app"                   # with targets = "fly": fly ips list -a my-app --json
# no_mail = true                       # SPF `v=spf1 -all` and DMARC `v=DMARC1; p=reject`
```

Items are `cloudflare/dns_record`, keyed `<name> <TYPE> <content>`: `create` for a missing
record, `ok`, `drift` for a proxied (orange cloud) record or an A/AAAA at the host that is not a
target, `update` for an SPF or DMARC record with other content. Records are always DNS only:
`proxied = true` is an error, because a proxied record breaks Let's Encrypt HTTP challenges and
replaces the client IP. `apply` creates the missing records, turns the proxy off, deletes stray
A/AAAA records (`--adopt` keeps them) and rewrites SPF/DMARC content. A CNAME at the host is drift
with the human step `dns-cname:<host>`: delete it by hand. `targets = "fly"` reads
`fly ips list --json` (program `HOIST_FLY`), skipping private addresses. DNS needs a token with
Zone Read and DNS Write on the zone, no account id.

Without a Cloudflare token, or when the zone cannot be read, each wanted record is `unknown`
with the human step `dns:<name> <TYPE>` ("in zone <zone> add <TYPE> <name> <content>, DNS only
(grey cloud), TTL auto"); `check` and `apply` then exit 2 and the other areas still run.

### Minted token

When the Cloudflare area is hoist-managed and the repo has a deploy target, `apply` mints the
Cloudflare token CI deploys with, named `hoist:<owner>/<repo>:deploy`, and seals it into the
GitHub secret `CLOUDFLARE_API_TOKEN` (hoist also sets `CLOUDFLARE_ACCOUNT_ID`). The scope is the
whole Cloudflare account, not one project: Cloudflare cannot scope a Pages permission to a single
project. Permission groups are `Pages Write` and/or `Workers Scripts Write`. The token expires
after 365 days; `plan` and `check` report it as pending within 30 days of expiry or when it is
not active. If `CLOUDFLARE_API_TOKEN` was set by hand, hoist leaves it alone (`ok`) and tells you
to run `hoist secrets rotate`. hoist does not mint when it cannot list tokens (`unknown`).

The live deploy token is the newest one issued at or before the secret's last update (GitHub's
`updated_at`). `--prune` keeps it and any deploy token issued after the secret was written (left
by a failed mint; `hoist secrets rotate` revokes those). When hoist cannot tell which token is
live (including tokens issued in the same second, or within 60 s of the secret's update while
another token exists), `--prune` revokes nothing and the token item is pending with a rotate step naming the ids.

## Immutable releases

`[github] immutable_releases = true` (opt-in; unset leaves the setting alone) makes `check` and
`plan` report drift until GitHub's immutable releases are on, and `apply` turns them on
(`false` turns them off). Spec §5.

## Secrets

`secret_scope = "environment"`, `hoist secrets set|rotate|grant|revoke|sync` and secrets repos:
see [docs/SECRETS.md](docs/SECRETS.md) and the spec (§8, §15).

## Releasing

hoist releases itself with `hoist release` (`.github/workflows/release.yml`, on every push to
`main` and by hand); no step that holds `GH_TOKEN` compiles anything:

- `detect` reads the version in `Cargo.toml`; it is pending when no tag `v<version>` exists or
  that tag points at HEAD (a re-run).
- `build` (pending only) builds `hoist-<target>` (`.exe` on Windows) for
  `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`,
  `aarch64-apple-darwin` and `x86_64-pc-windows-msvc` and uploads them as workflow artifacts.
- `publish` (pending only, environment `release`, `contents: write`, `id-token: write`) runs
  the shipped `dist/hoist-x86_64-unknown-linux-musl release publish --assets dist`: tag `v<version>`, the GitHub Release with the binaries
  and `SHA256SUMS`, the crate `keel-hoist` on crates.io and the npm launcher
  `@keelstackhq/hoist` with its five platform packages `@keelstackhq/hoist-<os>-<cpu>` (in
  `npm/`, listed by `pnpm-workspace.yaml`), all through trusted publishing (npm 12.2.0, Node 24).
- `prepare` (after `publish` when that ran) builds hoist in a step without `GH_TOKEN`, then runs
  `hoist release prepare`: it opens, updates or
  closes the release PR on `hoist/release`. Merging that PR releases. The step passes
  `HOIST_RELEASE_TOKEN`, `HOIST_RELEASE_APP_ID` (a repository variable) and `HOIST_RELEASE_APP_KEY`
  as env; exit 2 (PR open without a release token, so it runs no checks) becomes a warning and the
  step succeeds. Other exit codes fail the step.

Settings: `hoist.toml` (`[publish] crates` and `github_release` targets with `bin = "hoist"`),
`npm/hoist/hoist.toml` (`[publish] npm = { launcher = true }`). The optional repository secret
`HOIST_RELEASE_TOKEN` (a token with Contents and Pull requests: write) makes the release PR run
CI (or the env pair `HOIST_RELEASE_APP_ID` and `HOIST_RELEASE_APP_KEY` of a GitHub App with Contents and
Pull requests: write, from which `release prepare` mints an installation token); pass them to the
job as env. `release prepare` pushes the `hoist/release` branch with the same token (through git's environment, never argv), so the push also triggers checks. Without them, in Actions, the PR is opened with `GITHUB_TOKEN`, gets no checks, and
`release prepare` exits 2 with the human step `release-token`, which names what to create: a
fine-grained personal access token (this repo; Contents and Pull requests: Read and write) saved as
the secret `HOIST_RELEASE_TOKEN` under Settings > Secrets and variables > Actions, or the App pair.
On a repo with its own `release.yml` (no `# hoist:managed` first line), `hoist init` leaves it and
exits 2 (`release-workflow`): keep yours and make sure its publish job uses the release environment (`[release] environment`),
or run `hoist init --force` to replace it (keep a copy) and delete any other workflow that tags or
publishes.

The first release needs a person once, because trusted publishing can only be set up for a
package that already exists on its registry. Run `hoist apply --area release` first, so the
`release` environment has its branch policy before the first workflow run; optionally add
`HOIST_RELEASE_TOKEN`. Let the release workflow tag the version (its publish step fails for the
packages that are not set up yet), then, on your machine:

```bash
npm login && cargo login                    # your own accounts; npm >= 11.15.0 (`npm trust`)
hoist release bootstrap --dry-run           # read-only: what is missing, what would be written
hoist release bootstrap                     # lists the writes, asks [y/N], then runs them
```

`hoist release bootstrap` publishes each missing npm package once as a placeholder `0.0.0`
(only `package.json` and `README.md`, from a temp dir; nothing from `npm/` in the checkout),
publishes each missing crate at its release tag from a temporary `git worktree` (worktrees left
by an interrupted run are removed first; `git worktree prune` only drops registrations whose
directories are gone), then registers
the npm trusted publishers with `npm trust github <pkg> --file <workflow> --repo <o>/<r> --env
<env> --allow-publish`. npm prompts for 2FA, so run it in a terminal; without one (`--yes` in a
script) the trust commands are listed as human steps instead. crates.io trusted publishers are a
human step with the crate's settings link. Packages that exist are never touched, so a re-run
continues where a failed run stopped. It refuses to run in GitHub Actions and never reads npm or
crates.io credentials. Its last line is the next step: once every package exists and every npm
trusted publisher is registered, re-run the failed `release publish` job at the tag.
Do that within 2 days: npm expires a trusted publisher whose first publish has not happened by
then (delete and recreate it). The npm publisher fields are organization `keelstackhq`,
repository `hoist`, workflow `release.yml` (file name only) and environment `release`. While the
repository is private, npm publishes without provenance; it adds provenance by itself once the
repository is public.

### The release action

`hoist init` writes the three-job `release.yml` that uses it (detect, publish, prepare), pinned to the
release commit of this binary; a file you edited is kept unless `--force`. Not for npm launchers.
One step per job instead of building hoist yourself: `uses: keelstackhq/hoist/release@<full sha> # vX.Y.Z`
with `command: detect | prepare | publish` (`assets:` for publish; `token:` is hoist's `GH_TOKEN`;
`detect` sets output `pending`). It installs the hoist release of that commit from its GitHub
Release asset, checked against `SHA256SUMS`; no npm, Node or Rust. `SHA256SUMS` comes from the same
release as the binary, so the `sha256:` input is the only independent anchor: pin it.
`prepare` exiting 2 (PR open, no release token) is a warning in the action; the CLI still exits 2.
Linux x64 and arm64 runners. For `prepare`, pass a token that triggers workflows (App token or
`HOIST_RELEASE_TOKEN`), else the release PR runs no checks.

### Container images

hoist does not push OCI images yet. A `[deploy]` script cannot either: it runs in the `publish`
job, which has no `packages: write`. Add a separate `image` job to `release.yml` that runs after
`publish`, only in a run that released, from the release tag, with `packages: write`,
`attestations: write` and `id-token: write`. It pushes `ghcr.io/<owner>/<image>:<version>` for
`linux/amd64,linux/arm64` only when the registry answers 404 for that tag (a version tag never
moves), and `actions/attest-build-provenance` attests only the digest it built. A tag that is
already there must carry that attestation, or the job fails. Deploy by digest. The full pattern is
in the design spec, §9.7.

## Permissions

| Token      | Reads (`plan`, `check`, `import`)                              | Writes (`apply`, `secrets`)                                           |
| ---------- | -------------------------------------------------------------- | --------------------------------------------------------------------- |
| GitHub     | Administration, Secrets, Variables, Environments, Actions, Metadata: read | Administration, Secrets, Variables, Environments, Actions: write; Contents: read |
| Cloudflare | Pages, Workers scripts, API tokens: read; Zone Read for `[cloudflare.dns]` | "API Tokens: Edit" (to mint), plus the Pages and Workers write access; DNS Write for `[cloudflare.dns]` |

GitHub writes need admin on the repository. A GitHub App installation token (`GH_TOKEN` from
`actions/create-github-app-token`, no PAT) works too: the App must be installed on the repository
with those write permissions. Actions' built-in `GITHUB_TOKEN` also passes the preflight but
cannot apply (its writes fail, exit 1). With an App token, `[github] apps` installs read as
`unknown` (hoist lists installs through `/user/installations`, a user-token endpoint). The Cloudflare token is used only on your machine;
CI gets the minted, narrower token.

## Exit codes

| Code | Meaning                                                          |
| ---- | ---------------------------------------------------------------- |
| 0    | in sync                                                          |
| 1    | drift, a failed write or a failed verification                   |
| 2    | human step pending                                               |
| 3    | usage, config, auth or API error (the message says how to fix it) |

## Environment

| Variable                | Use                                                                                 |
| ----------------------- | ----------------------------------------------------------------------------------- |
| `GITHUB_TOKEN`          | GitHub token (see Permissions)                                                      |
| `GH_TOKEN`              | used when `GITHUB_TOKEN` is unset; then `gh auth token`                             |
| `CLOUDFLARE_API_TOKEN`  | Cloudflare token (see Permissions), needed when the repo has a deploy target        |
| `CLOUDFLARE_ACCOUNT_ID` | Cloudflare account id (or set it in `hoist.toml`); not needed for `--area github`   |
| `HOIST_GITHUB_API`      | GitHub API base, default `https://api.github.com`                                   |
| `HOIST_CLOUDFLARE_API`  | Cloudflare API base, default `https://api.cloudflare.com/client/v4`                 |

Token values are never printed or logged.
