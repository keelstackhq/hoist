# Changesets Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** hoist versions crates and npm packages from `.changeset/*.md` files (the @changesets/cli
2.x format), checks that PRs carry them, and keeps its release PR and publish flow.

**Architecture:** new module `src/changeset/` (format, config, plan source); the planner's
per-package `(Bump, entries)` input (`src/release/planner.rs` `own`) gets a second source; new ops
`src/ops/changeset/{add,init,status}.rs`; `prepare` deletes consumed files and writes
changesets-style changelog entries. Mode = `.changeset/config.json` exists.

**Tech Stack:** Rust 1.97, existing crates only (serde_json for config.json; hand parser for the
frontmatter subset; glob matching hand-written for `*`/`?`). No new crate without owner approval.

**Spec:** `docs/superpowers/specs/2026-11-10-changesets-design.md` (and hoist spec §9).

## Global Constraints

- hoist CLAUDE.md rules: exit codes 0/1/2/3, one JSON object, GET-only reads, never leak, tests
  via `tests/common::hoist()` and the fake API only, rustls, `forbid(unsafe_code)`.
- Format reference: `@changesets/cli@2.31.1`; writer bytes `---\n"<name>": <type>\n---\n\n<summary>\n`.
- `config.json` unknown keys exit 3; `changelog` only `"@changesets/cli/changelog"`,
  `"@changesets/changelog-git"` or `false`; `commit` only `false`.
- `changeset status`, `add`, `init` send no HTTP request (tests assert zero requests).
- D-016: release paths are gated: each PR ≤ 400 changed lines (tests, lockfiles excluded), one task.
- D-007: no new workflow job, trigger or runner; the check is one step in job `ci`.
- D-015/D-017: agents never merge, tag or publish in these tasks; spec §9 and README change in the
  same PR as the behaviour.

## Review Focus

- A changeset written by real `changeset add` (prettier-formatted, quoted names, CRLF from a
  Windows editor) parses → CS-1 test `parses_changesets_cli_output_and_crlf`.
- A PR touching only the npm launcher dir is reported against the crate, not the launcher →
  CS-5 test `launcher_dir_counts_for_crate`.
- A changeset whose package awaits publish (R26) is kept, not deleted, by `prepare` → CS-4 test
  `awaiting_publish_keeps_its_changeset`.
- Cargo caret on 0.x (`"0.4"` vs new `0.5.0`) leaves range and bumps the dependent → CS-3 test
  `cargo_zero_caret_out_of_range`.
- The release PR itself (branch `hoist/release`, changesets deleted, versions bumped) passes the
  check → CS-5 test `release_branch_is_exempt`.

---

## Order and owners

| task | repo | model | gated | depends |
|-|-|-|-|-|
| CS-1 format + config | hoist | opus | yes | — |
| CS-2 `changeset add`, `init` | hoist | sonnet | yes | CS-1 |
| CS-3 plan from changesets | hoist | opus | yes | CS-1 |
| CS-4 prepare consumes + changelog | hoist | opus | yes | CS-3 |
| CS-5 `changeset status` | hoist | opus | yes | CS-1 |
| CS-6 `init --from-commits` | hoist | sonnet | yes | CS-2, CS-3 |
| CS-7 hoist adopts (release hoist after) | hoist | sonnet | yes | CS-1..6 |
| CS-8 bosun code knows `.changeset/` | bosun | sonnet | yes | — |
| CS-9 bosun adopts | bosun | sonnet | yes | CS-7 released, CS-8 |
| CS-10 hull + bosun channel seeds | hull, bosun | sonnet | yes | CS-9 |
| CS-11 berth, splice, hoard adopt | each repo | sonnet | yes | CS-7 released |
| CS-12 pre-release mode (O5, not scheduled) | hoist | opus | yes | CS-4 |

Every PR gets an independent reviewer session on opus. CS-2, CS-3, CS-5 can run in parallel after
CS-1.

### CS-1 `feat(changeset): read and write the changesets format` — PR #64

**Files:** Create `src/changeset/mod.rs`, `src/changeset/file.rs`, `src/changeset/config.rs`;
modify `src/lib.rs` (mod); test `tests/changeset_format.rs` (+ unit tests in the files).

**Interfaces (produces):**
- `pub enum CsType { None, Patch, Minor, Major }` (Ord in that order), `CsType::parse(&str)`.
- `pub struct Changeset { pub id: String, pub releases: Vec<(String, CsType)>, pub summary: String }`
- `pub fn parse(id: &str, text: &str) -> Result<Changeset, String>`; `pub fn render(c: &Changeset) -> String`.
- `pub fn read_dir(root: &Path) -> Result<Vec<Changeset>, HoistError>` (sorted by id; exit 3 names file).
- `pub struct CsConfig { base_branch, changelog: bool, fixed: Vec<Vec<String>>, linked, ignore, update_internal_dependencies: CsType, changed_file_patterns: Vec<String>, only_peer_out_of_range: bool, dependents_always: bool, access: Option<String> }`
- `pub fn read_config(root: &Path) -> Result<Option<CsConfig>, HoistError>` (None = no file = commits mode).
- `pub fn glob_match(pat: &str, s: &str) -> bool` (`*` within a segment, `**` any, `?`).

- [x] Tests first: `parses_changesets_cli_output_and_crlf` (`---\n"@scope/a": minor\n'b': patch\nc: major\n---\n\nline1\nline2\n` and the same with `\r\n`), `empty_changeset_has_no_releases`, `render_matches_changesets_write_bytes`, `render_then_parse_roundtrips`, errors (exit 3 text contains file name): `missing or invalid frontmatter`, `file is empty`, type `huge`, duplicate name, nested YAML; `read_dir_skips_readme_and_dotfiles`, `legacy_changes_json_dir_is_an_error`; config: `init_defaults_parse`, `unknown_key_is_exit_3_naming_key`, `commit_true_refused`, `custom_changelog_refused`, `changelog_false_ok`, `base_branch_defaults_to_master`, `glob_patterns`.
- [x] Run `cargo test --test changeset_format` → FAIL (module missing).
- [x] Implement; frontmatter: line-based, `name: type`, name bare/`"…"`/`'…'` (no escapes beyond `\"`).
- [x] fmt, clippy, `cargo test` → PASS.
- [x] Commit `feat(changeset): read and write the changesets format`.

### CS-2 `feat(cli): hoist changeset add and init` — PR #65

**Files:** modify `src/cli.rs` (`Command::Changeset { Add, Init, Status }` — Status stub
returns exit 3 "not yet" until CS-5), create `src/ops/changeset/{mod,add,init}.rs`,
`src/changeset/words.rs` (≈60 adjectives, nouns, verbs); spec §3 CLI table, README; test
`tests/cli_changeset_add.rs`.

**Interfaces:** consumes CS-1 `render`, `Changeset`. Produces `pub fn default_id(dir:&Path) -> String`
(branch with `/`→`-`, detached HEAD → words).

- [x] Tests first: `add_writes_changesets_bytes` (`hoist changeset add keel-a:minor -m "x"` → `.changeset/<branch>.md` exact bytes, JSON item `changeset/<id>` `create`), `add_empty`, `add_reads_summary_from_stdin`, `add_unknown_package_exit_3`, `add_launcher_name_exit_3_names_crate`, `add_existing_id_exit_3`, `init_writes_config_and_readme` (bytes as spec §4.2, `baseBranch` from `origin/HEAD` else `main`), `init_twice_exit_3`, `no_http_requests` (API base `http://127.0.0.1:9`).
- [x] Run → FAIL; implement; run → PASS; fmt/clippy.
- [x] Commit `feat(cli): hoist changeset add and init`.

### CS-3 `feat(release): plan versions from changesets`

**Files:** create `src/changeset/plan.rs`, `src/changeset/range.rs`; modify
`src/release/planner.rs` (input enum), `src/ops/release/plan.rs` (item `release/source`, reason
"changesets: <ids>"), `src/release/mod.rs` (dependency kinds on edges if not kept today; same-name
crate/npm exit 3 in changesets mode); spec §9.1 note; test `tests/cli_release_changesets.rs`.

**Interfaces:**
- `pub enum Source { Commits(History), Changesets { sets: Vec<Changeset>, cfg: CsConfig, last_tag: HashMap<String, Option<String>> } }`; `plan_versions(m, &Source, pins)` keeps its return type; `NextVersion` gains `changesets: Vec<String>` (ids) and `dep_updates: Vec<(String, String)>` (name, new version).
- `range.rs`: `pub fn npm_satisfies(range:&str, v:&Version) -> Option<bool>`, `pub fn cargo_satisfies(req:&str, v:&Version) -> Option<bool>` (None = unparseable → no rewrite, as changesets), `pub fn rewrite(old:&str, new:&Version) -> String` (keeps `^ ~ >= =`/exact, keeps `workspace:*|^|~`).

- [ ] Tests first: `highest_type_wins`, `major_on_zero_goes_to_one`, `dependent_in_range_not_bumped`, `dependent_out_of_range_patch`, `cargo_zero_caret_out_of_range`, `dev_dependency_none_but_rewritten`, `peer_minor_makes_major`, `fixed_group_all_release`, `linked_only_releasing`, `hoist_fixed_plus_config_fixed_exit_3`, `ignored_mixed_changeset_exit_3`, `launcher_follows_crate`, `pin_overrides_changeset`, `awaiting_publish_no_bump`, `no_changesets_nothing_releases`, `commits_mode_unchanged` (existing tests green), `plan_sends_only_get`.
- [ ] Run → FAIL; implement (loop: dependents → fixed → linked until stable, spec §5); run → PASS.
- [ ] Commit `feat(release): plan versions from changesets`.

### CS-4 `feat(release): prepare consumes changesets and writes their changelog`

**Files:** modify `src/ops/release/prepare/*` (delete consumed files in the release commit; PR
body lists ids), `src/release/edit.rs` (`prepend_changelog` changesets layout; `changelog_section`
also matches a bare `## <version>` line), `src/release/gitlog.rs`
(`pub fn added_by(dir, path) -> Result<Option<String>, HoistError>`, 7-char sha); spec §9.2/§9.3;
test `tests/cli_release_prepare_changesets.rs`.

- [x] Tests first: `release_commit_deletes_consumed_changesets` (README.md, config.json stay), `empty_changesets_deleted_with_release`, `awaiting_publish_keeps_its_changeset`, `changelog_layout` (exact text of spec §8 incl. two-space continuation and `Updated dependencies [sha]`), `changelog_false_writes_none`, `unreleased_moves_under_version`, `publish_reads_bare_heading`, `pr_body_lists_changesets`.
- [x] Run → FAIL; implement; run → PASS (all existing prepare/publish tests green).
- [x] Commit `feat(release): prepare consumes changesets and writes their changelog`.

### CS-5 `feat(changeset): hoist changeset status` (PR #69)

**Files:** create `src/ops/changeset/status.rs`; modify `src/cli.rs` (`--since <rev>`), spec §3,
README; test `tests/cli_changeset_status.rs`.

**Interfaces:** consumes `read_dir`, `read_config`, `glob_match`, the release model's package dirs.

- [x] Tests first (temp git repos): `changed_crate_without_changeset_is_drift_exit_1`, `named_in_changeset_ok`, `none_type_counts`, `empty_changeset_covers_all`, `root_crate_excludes_other_package_dirs`, `launcher_dir_counts_for_crate`, `changed_file_patterns_filter` (docs-only with `["src/**"]` → ok), `changesets_dir_changes_alone_ok`, `release_branch_is_exempt` (`GITHUB_HEAD_REF=hoist/release`), `bad_changeset_exit_3`, `since_head_parent_on_merge_commit`, `default_base_is_origin_base_branch`, `no_http_requests`, JSON shape.
- [x] Run → FAIL; implement (`git diff --name-only --no-renames <merge-base>..HEAD`; added = `--diff-filter=A` under `.changeset/`); run → PASS.
- [x] Commit `feat(changeset): hoist changeset status`.

### CS-6 `feat(changeset): init --from-commits`

**Files:** modify `src/ops/changeset/init.rs`; test in `tests/cli_changeset_add.rs`.

- [x] Tests first: `from_commits_writes_pending_bumps` (feat → minor, fix → patch, breaking below 1.0 → minor, as §9.1; summary = entries one per line; file `hoist-migrate.md`), `from_commits_nothing_pending_writes_no_changeset`, `then_plan_gives_same_versions` (commits-mode plan before == changesets-mode plan after, dependents aside).
- [x] Run → FAIL; implement via `read_history` + `classify`; run → PASS.
- [x] Commit `feat(changeset): init --from-commits`.

### CS-7 `ci: hoist uses changesets` (then the orchestrator releases hoist)

**Files:** `.changeset/config.json`, `.changeset/README.md` (via `cargo run -- changeset init
--from-commits`), `.github/workflows/ci.yml` (in job `ci`: `fetch-depth: 2`; step
`if: github.event_name == 'pull_request'` → `cargo run --locked -- changeset status --since HEAD^1`),
`CLAUDE.md` (one agent line, spec §7), `README.md` release section.
`changedFilePatterns` per O3 (recommended `["src/**","Cargo.toml","Cargo.lock","build.rs","npm/**"]`).

- [ ] Verify locally: `cargo run -- changeset status --since origin/main` on this branch → ok;
  `cargo run -- release plan` → `release/source` changesets, same next version as before.
- [ ] Commit `ci: hoist uses changesets`. After merge, the orchestrator runs the release (D-015).

### CS-8 `feat: changesets are fragments` (bosun)

**Files (bosun):** `src/core/risk.rs` (`AUTONOMOUS_ALLOWED` + `.changeset/**`; keep
`changelog.d/**` until CS-9 lands), `src/core/guard.rs` (edit lock skip for `.changeset/`),
`src/core/touch.rs` (`FRAGMENTS` + `.changeset/*.md`), their unit tests (mirror the
`changelog.d/x.md` cases with `.changeset/x.md`); `.changeset/README.md` and `config.json` are
not fragments (no edit-lock skip).

- [ ] Tests first → FAIL → implement → bosun local CI mirror → PASS. Commit.

### CS-9 `ci: bosun uses changesets` (bosun)

**Files (bosun):** run `bash scripts/fold-fragments.sh` once (folds the last `changelog.d/`
fragments into `## Unreleased`), delete `changelog.d/`; `hoist changeset init` (hoist from npm at
the CS-7 release); `scripts/fold-fragments.sh` and
`channel/pieces/core/files/.bosun/fold-fragments.sh` keep only `docs/progress.d`;
`channel/pieces/core/files/sections/agents.md`, `AGENTS.md`, `CLAUDE.md` (changeset line);
`docs/releasing.md` (drop step 0, step 1 says changesets); `.github/workflows/ci.yml` (one step in
job `ci`, `npm install -g @keelstackhq/hoist@<ver>` already pinned the same way as release.yml,
`hoist changeset status --since HEAD^1` on PRs); `.github/workflows/release.yml` hoist pin bump;
`tests/release_config.rs` new `changeset_config_is_valid` (file exists, JSON keys ⊆ 2.31.1 set,
`baseBranch == "main"`, `changelog` git, every `.changeset/*.md` names only `keel-bosun`);
remove `changelog.d/**` from risk.rs list.
Split if over 400 lines: CS-9a (fragments → changesets, docs, tests), CS-9b (workflow step).

- [ ] Tests first (`changeset_config_is_valid` fails before init) → implement → local CI → PASS. Commit.

### CS-10 `feat: seed changesets for new repos` (hull, bosun channel)

**Files:** hull's rendered seeds: `.changeset/config.json`, `.changeset/README.md`, the CI step in
the rendered `ci.yml`, agent line in the generated `CLAUDE.md`; bosun channel piece for the agent
section (already done in CS-9) and the shipped workflow step if the channel ships `ci.yml`. One PR
per repo.

### CS-11 `ci: <repo> uses changesets` (berth, splice, hoard)

Per repo, the CS-7 recipe with hoist from npm: `hoist changeset init --from-commits`, CI step,
agent line, `changedFilePatterns` per O3. One PR per repo; repos without a hoist release flow
(splice tags `v1`) skip until they have one.

### CS-12 `feat(changeset): pre-release mode` (not scheduled, O5)

Spec §8 last bullet: `hoist changeset pre enter <tag>|exit`, `.changeset/pre.json` bytes as
changesets, `semver.rs` pre-release parse/bump, npm `--tag`, GitHub Release `prerelease: true`.
Split into CS-12a (semver + plan) and CS-12b (publish) to stay ≤ 400 lines.

## Owner decisions (from spec §11)

O1 changelog heading with date (rec.) · O2 strict per-package check (rec.) · O3
`changedFilePatterns` source paths only (rec.) · O4 remove commits fallback one release after
the last repo moves (rec.) · O5 pre-release later (rec.).
