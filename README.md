# hoist

Provision and verify a repo's GitHub and Cloudflare setup from what the repo already says
(workflows, `wrangler.jsonc`, `package.json`, `renovate.json`, optional `hoist.toml`). A single
static Rust binary. Part of the keelstackhq family (hull, splice, hoard).

Status: M2 (provisioning) done. `apply` and `secrets set|rotate` write (after a preflight); every
other command, including `import`, only sends GET requests. Release comes in a later milestone.
Design: `docs/superpowers/specs/2026-10-16-hoist-design.md`.

## Install

From source (Rust 1.97, see `rust-toolchain.toml`):

```bash
cargo install --path .
```

## Commands

```bash
hoist explain                 # desired model and the file each item came from
hoist plan [--area github|cloudflare]    # diff desired vs live, no writes
hoist check [--area ...]      # plan plus verification (required checks, Pages URL)
hoist apply [--yes] [--prune] [--area ...]  # write the plan, then verify like check
hoist auth check              # what each token can read vs what hoist needs
hoist secrets set NAME        # seal env NAME or a hidden prompt into an Actions secret
hoist secrets rotate [NAME]   # mint, verify and seal a new CLOUDFLARE_API_TOKEN, revoke the old
hoist import [--write] [--area ...]  # hoist.toml overrides that match the live state; --write writes them
hoist release plan            # next versions from conventional commits, read-only
hoist release prepare         # open/update the release PR on branch hoist/release, or close it
hoist release publish [--assets DIR] [--dry-run]  # tag and publish GitHub Releases (CI only)
```

`hoist release prepare` needs a clean tree on the default branch and full history. It rebuilds
the branch `hoist/release` from the default branch (versions, dependents, `CHANGELOG.md`,
lockfiles), force-pushes it to `origin` with the checkout's git credentials and opens or updates
the PR; when nothing releases it closes the PR and deletes the branch. Exit 3 before any write, 1
when a write fails (the report lists what was done). PRs opened or pushed with the default
`GITHUB_TOKEN` do not trigger CI: pass a token that can (`GH_TOKEN`, and the same token to
`actions/checkout`) for the release PR to run checks. The GitHub token needs Pull requests: write.

`hoist release publish` runs only in GitHub Actions on the default branch, at `GITHUB_SHA`, in a
job that declares `environment: <[release] environment>` (else exit 3 before any request). For
every package whose version has no tag on an earlier commit it creates the tag on `GITHUB_SHA`
through the API and a draft GitHub Release with the changelog section, uploads the binaries
`<DIR>/<bin>-<target>` plus `SHA256SUMS`, then publishes the draft; finished steps are skipped on
a re-run, and every step is verified afterwards (exit 1 on a failed write or verification). The
GitHub token needs Contents: write. `--dry-run` sends GET requests only and runs anywhere.

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
