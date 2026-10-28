# hoist — required checks pinned to their source (and optional code-owner review)

Design: hoist spec §6.1. Requested for bosun's App check runs (bosun `docs/spec/15-app-checks.md`,
"hoist" and "Transition" step 5; bosun plan `2026-10-28-app-checks-team-owners.md`, B2W/T3 waits
for a hoist release with it). Not scheduled: the code waits until bosun's App design is settled.
Then the owner schedules it; each task below is one PR of 400 lines or fewer, in this order.

Tech stack: no new crates. Rules from hoist's CLAUDE.md apply (exit codes, one JSON object,
GET-only reads, never leak, tests on the fake API only). PRs do not edit `docs/PROGRESS.md` or
`CHANGELOG.md`.

## Facts to verify first (real API, read-only, dated in `docs/PROGRESS.md` by the owner)

- `GET /apps/github-actions` answers `id: 15368` (bosun O6 read 15368 from ruleset exports).
- `GET /apps/{slug}` for a private App: which tokens see it (an org member's fine-grained token
  with no extra permission, or only the owner's), and the status when not (404 or 403).
- The ruleset API accepts and returns `integration_id` per entry of
  `required_status_checks` and keeps entries without it unpinned.
- A ruleset `pull_request` rule with `required_approving_review_count: 0` and
  `require_code_owner_review: true` blocks a PR touching a CODEOWNERS path until a code owner
  approves, and leaves other PRs alone.
- Whether a ruleset write with the `integration_id` of an App not installed on the repo is
  refused (422); if so, RC-2 tests that apply reports it as a failed write.
- Whether a live ruleset can list one context twice with different ids (import's `unknown`).
- Whether a ruleset PUT without `bypass_actors` keeps the live ones or clears them.

## Tasks

- [ ] RC-1 `feat(config): required_checks entries with a source`: `src/config.rs` (an untagged
  enum: string or `{context, app | integration_id}`, strict), `src/desired.rs`
  (`RequiredCheck { context, source: Any | App(slug) | Id(u64) }`), `src/ops/explain.rs` (shows
  the slug or id), `tests/config.rs`, `tests/cli_plan.rs` and `tests/cli_explain.rs`.
  Tests first: a table with both keys, neither, an unknown key, a slug outside `[a-z0-9-]`,
  `integration_id = 0`, a context twice: exit 3 naming the file and entry; strings parse as today
  (every existing test stays green unchanged).
- [ ] RC-2 `feat(github): read and write pinned required checks`: `src/github.rs` (the live
  `Ruleset` keeps `integration_id` per check), a resolver in `src/github.rs` (`GET /apps/{slug}`,
  once per slug, plus `github-actions` when any entry has an `integration_id`; every failure exit
  3 with the §6.1 message), `src/diff.rs` (compare pairs; reasons as in §6.1; string vs live pin
  is an `update` reported as a downgrade, "removes source pin"), `src/change.rs` and
  `src/github_write.rs` (`ruleset_body` writes `integration_id`), `tests/github_diff.rs`,
  `tests/github_write.rs`, `tests/cli_plan.rs`, `tests/cli_apply.rs`. Tests: plan with a pinned
  check missing, different, extra (string with a live pin), in sync; apply's exact PUT body; a
  private-App 404 is exit 3 with nothing written, and so is a failed `github-actions` resolve; a
  slug resolved once for two checks; the downgrade reason; plan and check stay GET-only.
- [ ] RC-3 `feat(check): verify pinned checks; import keeps pins`: `src/verify.rs` (the job-name
  rule only for unpinned and GitHub Actions checks; an App-pinned check is `ok` with the §6.1
  reason, since a failed resolve already exited 3), `src/ops/import.rs` and
  `src/ops/import/toml.rs` (table form for pinned live checks), `tests/cli_check.rs`,
  `tests/cli_import.rs`. Tests: an App-pinned `risk-check` with no workflow job of that name is
  `ok`; an Actions-pinned `ci-ok` without the job is drift; import of a pinned ruleset then plan
  is clean; a live context listed twice is `unknown` with no override.
- [ ] RC-4 `feat(github): optional code-owner review`: `[github] code_owner_review` (bool, default
  false) in `src/config.rs` and `src/desired.rs`; `src/github.rs` (the live `Ruleset` reads the
  `pull_request` rule), `src/diff.rs` (the ruleset is managed when checks or `code_owner_review`
  ask for it; `ruleset_summary` and the missing-ruleset reason name the review when there are no
  checks), `src/ops/explain.rs`, `src/change.rs` and `src/github_write.rs` (the rule with all five
  parameters, stale reviews dismissed on push; no `required_status_checks` rule when there are no
  checks), `src/verify.rs` (the local CODEOWNERS verify), `src/ops/import.rs`,
  `tests/github_diff.rs`, `tests/github_write.rs`, `tests/cli_plan.rs`, `tests/cli_check.rs`,
  `tests/cli_import.rs`, `tests/cli_explain.rs`. Tests: off writes no `pull_request` rule
  (existing bodies unchanged); off with a live rule added by hand, on with none, and on with
  another count or flag are each an `update`; on with no required checks creates the ruleset; the
  CODEOWNERS lookup order, the last matching rule wins, a rule with no owner is drift; import
  writes it; a live rule hoist cannot express is a comment with no override.
- [ ] RC-5 `docs: required-check sources`: move §6.1 from "planned" into §5, §6, §7.4, §7.5 and
  §8.1 as built; README's `hoist.toml` section; the facts above with their dates go to §13.

## What the consumer does after the release (not hoist's work)

bosun: hoist.toml lists `ci-ok` pinned to `github-actions` and its three gates pinned to its App;
hoist.toml is R-gov and on the critical floor first (bosun B2/T6), so no agent PR can unpin a
check; then the owner runs `hoist plan --area github` and `hoist apply --area github --yes` (bosun
H5). Rollback: the same apply from hoist.toml with plain strings.

## Open questions for the owner

- Pin an inferred `ci-ok` to GitHub Actions by default? Safer, but every existing repo would show
  an `update` on the next plan; proposed: no, opt in with the table form.
- Accept an `app` given as a number (`app = "123456"`) as an id, or keep ids in `integration_id`
  only? Proposed: only `integration_id`, so a typo never looks like a slug.
- Should `hoist check` look for a recent check run of a pinned App (on the default branch or a
  recent PR) to prove it runs here? Proposed: not in v1; the reason says hoist does not see its
  runs.
