# hoist

Provision and verify a repo's GitHub and Cloudflare setup from what the repo already says
(workflows, `wrangler.jsonc`, `package.json`, `renovate.json`, optional `hoist.toml`). A single
static Rust binary. Part of the keelstackhq family (hull, splice, hoard).

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
hoist plan [--area github|cloudflare|npm|crates|release]    # diff desired vs live, no writes
hoist check [--area ...]      # plan plus verification (required checks, Pages URL)
hoist apply [--yes] [--prune] [--area ...]  # write the plan, then verify like check
hoist auth check              # what each token can read vs what hoist needs
hoist secrets set NAME        # seal env NAME or a hidden prompt into an Actions secret
hoist secrets rotate [NAME]   # mint, verify and seal a new CLOUDFLARE_API_TOKEN, revoke the old
hoist import [--write] [--area ...]  # hoist.toml overrides that match the live state; --write writes them
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
`[publish] npm = { access = "restricted" }` exits 3: restricted npm packages are not supported
yet, because anonymous registry reads cannot see them)
and crates with `cargo publish -p <name> --locked` using a short-lived crates.io token exchanged
for the job's OIDC token and revoked afterwards; targets in the package's directory with
`[deploy] on = "release"` run `pnpm --filter <name> run deploy`. The job needs `permissions:
id-token: write` (else exit 3). A package that is not on its registry yet is `pending` with the
human step `first-publish:<name>` (exit 2); the others still publish. Finished steps are skipped
on a re-run, and every step is verified afterwards, registries by up to 5 reads 2 s apart (exit 1
on a failed step or verification). The GitHub token needs Contents: write. `--dry-run` sends GET
requests only and runs anywhere. npm, cargo and pnpm run without `GH_TOKEN`/`GITHUB_TOKEN`, and
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

For each npm package (launchers and every platform package included), publish a placeholder
version `0.0.0` once by hand (`"version": "0.0.0"` in its package.json, `npm publish --access
public`, then revert the edit). Never publish the real first version by hand: npm versions are
immutable, and a platform package published from the checkout has no `bin/` (hoist copies the
binary in only during `release publish`). Then, as a logged-in user with 2FA (npm ≥ 11.15.0),
register the trusted publisher:

```bash
npm trust github <package> --file release.yml --repo <owner>/<repo> --env release --allow-publish
```

For each crate, publish the real first version by hand (`cargo publish`) or a `0.0.0`
placeholder; either works. Then: crates.io, the crate's Settings, Trusted
Publishing, add repository `<owner>/<repo>`, workflow `release.yml`, environment `release`. Then
`hoist apply --area release` creates the `release` environment (`hoist plan` shows it as
`release/environment`).

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
one object on stdout; logs go to stderr. Actions are `create`, `update`, `ok`, `drift`,
`pending`, `unknown` (`unknown` means hoist could not read the resource, for example a 403).
A rejected token (401) or a repo the token cannot see (404) is an error (exit 3), not `unknown`.

Secrets are checked for presence only: a workflow secret counts as present when it is a
repository secret or an organization secret the repo can use. Environment secrets are not
checked in M1. The merge queue is expected on when a workflow has a `merge_group` trigger
(override with `[github] merge_queue`).

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
   to continue; changes already in place are `ok`.
5. After the writes hoist re-reads the live state and runs the `check` logic; that is the final
   report (`command: "apply"`). A Pages project or domain created in this run is not probed
   (`ok`, "created in this run; verified by `hoist check` after the first deploy"). If the re-read
   fails, the report lists the applied changes plus a `post_apply_check` item (exit 1).

Exit codes: 0 in sync, 1 a failed write, a declined prompt, or a change or failed verification
still open, 2 a human step is pending (for example a domain whose zone is not on the Cloudflare
account, a secret hoist cannot produce, or a token due for rotation), 3 preflight, usage, config or
auth error before the first write.

Settings hoist does not model are never touched. Deletes happen only with `--prune`.

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

## Secrets

`hoist secrets set NAME` seals a value into the repo's Actions secret `NAME`. The value comes from
env `NAME`, else a hidden prompt on a terminal, else exit 3; there is no value flag, and
`secrets set NAME VALUE` or `secrets set NAME=VALUE` exits 3 without echoing the value. `NAME` must
match `^[A-Z_][A-Z0-9_]*$` and not start with `GITHUB_` (exit 3 before any request). It needs
admin on the repo (same preflight as `apply`). The hoist credential names `CLOUDFLARE_API_TOKEN`,
`CLOUDFLARE_ACCOUNT_ID` and `GH_TOKEN` never read their env var: they take the value only from the
prompt, and without a terminal exit 3 (for `CLOUDFLARE_API_TOKEN`, `hoist apply` mints a scoped
token instead). A failed PUT exits 1.

`hoist secrets rotate [CLOUDFLARE_API_TOKEN]` replaces the minted deploy token. Only
`CLOUDFLARE_API_TOKEN` can be rotated; another name exits 3. Before any write it runs apply's
preflights and requires hoist-managed Cloudflare with a deploy target, an account id and a
readable token list (else exit 3). Order: mint a new token, verify it (`GET /user/tokens/verify`
with the new token, up to 3 tries about 1 s apart), seal it into the secret, revoke the old
`hoist:<owner>/<repo>:deploy` tokens. After the first write failures exit 1: a failed verify or
seal revokes the new token and leaves the old secret untouched; a failed revoke names the token id
and adds a human step to revoke it in the dashboard. Report: `github/secret/CLOUDFLARE_API_TOKEN`
is `ok` ("rotated"); each revoked token is a `cloudflare/api_token` item keyed by its name with
the id in the reason.

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
  closes the release PR on `hoist/release`. Merging that PR releases.

Settings: `hoist.toml` (`[publish] crates` and `github_release` targets with `bin = "hoist"`),
`npm/hoist/hoist.toml` (`[publish] npm = { launcher = true }`). The optional repository secret
`HOIST_RELEASE_TOKEN` (a token with Contents and Pull requests: write) makes the release PR run
CI; without it the job uses `GITHUB_TOKEN` and the PR gets no checks.

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
publishes each missing crate at its release tag from a temporary `git worktree`, then registers
the npm trusted publishers with `npm trust github <pkg> --file <workflow> --repo <o>/<r> --env
<env> --allow-publish`. npm prompts for 2FA, so run it in a terminal; without one (`--yes` in a
script) the trust commands are listed as human steps instead. crates.io trusted publishers are a
human step with the crate's settings link. Packages that exist are never touched, so a re-run
continues where a failed run stopped. It refuses to run in GitHub Actions and never reads npm or
crates.io credentials. Its last line is the next step: once every package exists and every npm
trusted publisher is registered, re-run the failed `release publish` job at the tag.

## Permissions

| Token      | Reads (`plan`, `check`, `import`)                              | Writes (`apply`, `secrets`)                                           |
| ---------- | -------------------------------------------------------------- | --------------------------------------------------------------------- |
| GitHub     | Administration, Secrets, Environments, Actions, Metadata: read | Administration, Secrets, Environments, Actions: write; Contents: read |
| Cloudflare | Pages, Workers scripts, API tokens: read                       | "API Tokens: Edit" (to mint), plus the Pages and Workers write access |

GitHub writes need admin on the repository. The Cloudflare token is used only on your machine;
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
