# Progress

## Status

hoist 0.4.1 is released (2026-11-04): GitHub Release `v0.4.1` with 5 binaries and `SHA256SUMS`,
crate `keel-hoist`, and `@keelstackhq/hoist` plus five platform packages on npm. Milestones M1
(read-only), M2 (provisioning), M3 (releasing), M4 (npm launcher) and M5 (secrets repos) are done;
container publish and required-check sources shipped after. Plans: `docs/superpowers/plans/`.

Open work (hardening round, this branch): `--json` error object on exit 3, SHA-pinned CI actions
with a pin check, the `release` composite action (`release/`), the agent skill
(`skills/hoist/SKILL.md`), `apply` defaults tested (`ci-ok` required check, `release`
environment reviewers), npm dist-tag cleanup after publish, immutable releases from `apply`,
release PR opened with a GitHub App token. Release-workflow generation by `apply` is in spec
§12.4 (see there for its state).

Task log: `docs/archive/PROGRESS-done.md`. Quickstart: `docs/QUICKSTART.md`.

## Blocked

Human-only items, with the exact action:

- Cloudflare dogfood: needs `CLOUDFLARE_API_TOKEN` (Pages read) and `CLOUDFLARE_ACCOUNT_ID`. Run
  `hoist plan`, `hoist check` and `hoist auth check` without `--area` on a repo with a Pages
  target and record Pages and custom-domain results in spec §13.
- GitHub dogfood with a token that reads rulesets, secrets, environments and Actions permissions
  (Administration, Secrets, Environments, Actions read), on a repo whose plan supports rulesets,
  outside the sandbox proxy: verifies those API field shapes.
- Dogfood `hoist apply` on a real repo: needs the owner's go-ahead, a GitHub token with
  Administration/Secrets/Environments/Actions write and a Cloudflare token with API Tokens: Edit.
- Release PR checks: create the GitHub App (Contents and Pull requests: write) and store its id and
  key as the secrets named in README "Releasing"; without them `release prepare` falls back to
  `HOIST_RELEASE_TOKEN` or `GITHUB_TOKEN` and the PR runs no checks.
