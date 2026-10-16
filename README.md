# hoist

Provision and verify a repo's GitHub and Cloudflare setup from what the repo already says
(workflows, `wrangler.jsonc`, `package.json`, `renovate.json`, optional `hoist.toml`). A single
static Rust binary. Part of the keelstackhq family (hull, splice, hoard).

Status: M2 in progress. `apply` and `secrets set|rotate` write (after a preflight); every other
command only sends GET requests. Release comes in a later milestone.
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
hoist import [--write]        # hoist.toml overrides that match the live state; --write writes them
```

`hoist import` sends GET requests only. It prints the `hoist.toml` lines (kind `override`, key
`<file>:<table>.<key>`) that make `plan` match the live settings: `[github] visibility` (when
pinned), `allow_auto_merge`, `delete_branch_on_merge`, `actions_permissions`, `required_checks`
and `merge_queue` (from the live `hoist: default branch` ruleset), and `[deploy] project` for a
Pages target whose inferred project is missing when exactly one live project starts with the
target name. Other live rulesets show as comments. A field it cannot read is `unknown` and is
not imported. `--write` edits the files in place (a missing file is created; comments, order and
other keys are kept; equal values leave the file untouched). Exit codes: 0 nothing to import or
written, 1 overrides found without `--write`, 3 config, auth or API error.

Global flags: `-C <dir>` (repo to inspect), `--json`, `--no-color`, `-v`. `--json` prints exactly
one object on stdout; logs go to stderr. Actions are `create`, `update`, `ok`, `drift`,
`pending`, `unknown` (`unknown` means hoist could not read the resource, for example a 403).
A rejected token (401) or a repo the token cannot see (404) is an error (exit 3), not `unknown`.

Secrets are checked for presence only: a workflow secret counts as present when it is a
repository secret or an organization secret the repo can use. Environment secrets are not
checked in M1. The merge queue is expected on when a workflow has a `merge_group` trigger
(override with `[github] merge_queue`).

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
| `GITHUB_TOKEN`          | GitHub token (read: Administration, Secrets, Environments, Actions, Metadata)       |
| `GH_TOKEN`              | used when `GITHUB_TOKEN` is unset; then `gh auth token`                             |
| `CLOUDFLARE_API_TOKEN`  | Cloudflare token (Pages read), needed when the repo has a deploy target             |
| `CLOUDFLARE_ACCOUNT_ID` | Cloudflare account id (or set it in `hoist.toml`); not needed for `--area github`   |
| `HOIST_GITHUB_API`      | GitHub API base, default `https://api.github.com`                                   |
| `HOIST_CLOUDFLARE_API`  | Cloudflare API base, default `https://api.cloudflare.com/client/v4`                 |

Token values are never printed or logged.
