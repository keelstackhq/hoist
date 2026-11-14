---
name: hoist
description: Provision and verify a repo's GitHub, Cloudflare and release setup with the hoist CLI. Use when asked to set up, check, diff or release a repo's settings, rulesets, environments, secrets, Pages or npm/crates publishing.
---

# hoist

hoist reads what the repo says (workflows, `wrangler.jsonc`, `package.json`, `hoist.toml`) and
makes GitHub and Cloudflare match. Always pass `--json` and parse stdout (one object).

## Which command

- Understand the setup: `hoist explain`.
- Preview changes: `hoist plan` (read-only, exit 0). Add `--area github|cloudflare|npm|crates|release`.
- Is it in sync: `hoist check` (read-only, plan + verify).
- Token problems: `hoist auth check`.
- Existing repo, no `hoist.toml`: `hoist import` prints one.
- Change things: `hoist apply --yes` (writes; run `plan` first and read it).
- Secrets: `hoist secrets set|rotate`; shared secrets repos: `hoist secrets grant|revoke|sync`.
- No `release.yml` (or no `ci-ok` job): `hoist init` (writes the files; you commit them; exit 2 and human step `ci-ok` when a hand-written workflow needs the job or the npm `test` script is the placeholder; `ci-ok-duplicate`: delete the hoist-written ci.yml). To preview `apply`, run `hoist plan` (`apply --dry-run` does not exist). Then `hoist release plan` (preview), `release prepare` (release PR), `release publish`
  (CI only, `release` environment), `release bootstrap` (first publish, local, human).

## Exit codes

0 in sync. 1 drift, failed write or failed verification. 2 a human step is pending. 3 usage, config,
auth or API error: read `error.message` and `error.fix` in the JSON, fix, run again. Do not retry 3.

## JSON

`{version, command, ok, exit_code, items[], human_steps[], error?}`. An item has `area`, `kind`,
`key`, `action` (`create|update|ok|drift|pending|unknown`), `reason`, `source`. `unknown` means
hoist could not read it (often a plan or permission limit), not that it is fine.

## Needs a human

Anything in `human_steps` (create a token, install an app, set a repo secret, approve an npm
publisher). Report each step's `title` and `url`; do not try to do them. Never print, log or pass
token values as arguments; tokens come from env or `gh auth token`.

## Rules

- Run `plan` before `apply`; never `apply` without the user's go-ahead.
- `hoist.toml` unknown keys are an error. Fix the key; do not delete the file.
- Releases, tags, npm and crates publishing are human-approved; never run `publish` locally.
