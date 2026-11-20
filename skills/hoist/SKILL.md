---
name: hoist
description: Provision and verify a repo's GitHub, Cloudflare and release setup with the hoist CLI. Use when asked to set up, check, diff or release a repo's settings, rulesets, environments, secrets, Pages or npm/crates publishing.
---

# hoist

hoist reads what the repo says (workflows, `wrangler.jsonc`, `package.json`, `hoist.toml`) and
makes GitHub and Cloudflare match. Always pass `--json` and parse stdout (one object).

## Happy path (run in this order, `--json`)

1. `hoist plan`: read it; fix exit 3 first (`hoist auth check` for token problems).
2. `hoist apply --yes`: only after the user's go-ahead. Writes GitHub/Cloudflare, then verifies.
3. `hoist init`: writes `.github/workflows/release.yml` (and `ci.yml` with the `ci-ok` gate when
   the repo has none). You commit and push the files. Exit 2 lists what the owner must do.
4. `hoist check`: exit 0 means in sync. Exit 1: run `plan`, then `apply` again.

## Other commands

- Understand the setup: `hoist explain`. Existing repo without `hoist.toml`: `hoist import`.
- Limit any of plan/apply/check with `--area github|cloudflare|npm|crates|release`.
- Secrets: `hoist secrets set|rotate`; shared secrets repos: `grant|revoke|sync`. `hoist secrets check`
  in a repo with no `access.toml` or `secrets.toml` exits 0 (one `secrets/config` item, `ok`).
- Releases: `hoist release plan` (preview), `prepare` (release PR), `publish` (CI only),
  `bootstrap` (first publish, local, human).
- `init` human steps: `ci-ok` (hand-written workflow needs the job), `ci-ok-npm-test` (npm `test`
  script is the placeholder; the gate runs cargo only), `ci-ok-duplicate` (delete hoist's ci.yml),
  `release-workflow` (repo has its own release.yml, left alone: keep it and add `environment: release`
  to its publish job, or run `hoist init --force` to replace it).
- `apply --dry-run` does not exist: use `hoist plan`.
- Item kinds you will see besides settings and secrets: `environment` (every workflow `environment:`,
  also `with: {environment: X}` on a reusable-workflow call, plus `[github.environments.<name>]`
  in `hoist.toml`: `branches = [..]` deployment branch policies, `manage = false` to leave one alone;
  an `unknown` one whose key is `${{ ... }}` is an expression hoist cannot resolve: name the real
  environments in `hoist.toml`; an `update` says which branch policy is missing, `apply` adds it and
  never deletes extras) and `variable` / `optional_variable` (a workflow's `vars.X`).

## Exit codes

0 in sync. 1 drift, failed write or failed verification. 2 a human step is pending. 3 usage, config,
auth or API error: read `error.message` and `error.fix` in the JSON, fix, run again. Do not retry 3.

## JSON

`{version, command, ok, exit_code, items[], human_steps[], error?}`. An item has `area`, `kind`,
`key`, `action` (`create|update|ok|drift|pending|unknown`), `reason`, `source`. `unknown` means
hoist could not read it (often a plan or permission limit), not that it is fine.

## Needs a human

Anything in `human_steps` (create a token, install an app, set a repo secret, set a repo variable
(`variable:<NAME>`: the title is the exact `gh variable set NAME --repo o/r --body '<value>'`; ask the
user for the value, or put it in `hoist.toml` `[github.variables]` and `apply`), approve an npm
publisher). Report each step's `title` and `url`; do not try to do them. Never print, log or pass
token values as arguments; tokens come from env or `gh auth token`.

## Rules

- Run `plan` before `apply`; never `apply` without the user's go-ahead.
- `hoist.toml` unknown keys are an error. Fix the key; do not delete the file.
- Releases, tags, npm and crates publishing are human-approved; never run `publish` locally.
