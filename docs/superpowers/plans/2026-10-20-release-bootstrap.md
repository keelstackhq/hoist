# hoist release bootstrap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `hoist release bootstrap` takes every missing package from "not on the registry" to "ready for trusted publishing", in one local run with the maintainer's own logins.

**Architecture:** A new command in `src/ops/release/bootstrap/` reuses the M3 pieces: the release model (with launcher platform packages), the anonymous registry reads, the `Tool` runner and the workflow/environment lookup that `plan` uses. It runs in four steps:
1. preflight (reads only);
2. npm placeholders, published from a temp dir;
3. crates, published from a `git worktree` at the release tag;
4. trusted publishers (`npm trust list` + interactive `npm trust github`).

Each step checks before it writes, and the first failed write stops the run with the M2/M3 stopped-report shape.

**Tech Stack:** No new crates. External tools at runtime: `npm` ≥ 11.15.0 (for `npm trust`), `cargo` and `git`, run through the `HOIST_NPM` / `HOIST_CARGO` / `HOIST_GIT` overrides.

**Spec:** `docs/superpowers/specs/2026-10-16-hoist-design.md` §3 (CLI line) and §9.6 (Bootstrap); §13 for the unverified facts.

## Global Constraints

- CLI: `hoist release bootstrap [--dry-run] [--yes] [--json]`, plus the global `-C`, `--no-color`, `-v`.
- Exit codes follow CLAUDE.md:
  - 3 only before the first write;
  - 1 for a failed write, with items done / failed / not attempted;
  - 2 when human steps alone remain;
  - 0 when everything is done or already done.
- `GITHUB_ACTIONS=true` exits 3: "bootstrap runs locally with your own logins; CI publishes through `hoist release publish`".
- A package that exists on its registry is reported `ok` and never written to.
- Read-only: `--dry-run` sends only GETs and runs only read-only tools (`npm whoami`, `npm --version`, `npm trust list`, `git tag`). A test asserts this with `assert_only_get` plus stub logs.
- Confirmation: unless `--yes` or `--dry-run`, list the planned writes on stderr and ask `[y/N]` on a TTY. With no TTY and no `--yes`, exit 3 with "use --yes in non-interactive runs". Declining exits 1 with every planned item "not applied: declined" (M2 R18).
- npm placeholder:
  - Written to a new temp dir, with `package.json` keys only `name`, `version: "0.0.0"`, `description`, `repository`, `license`.
  - `description` is exactly `placeholder for trusted publishing; releases come from <repo URL>`, where `<repo URL>` is `https://github.com/<o>/<r>`. `repository` is `{"type":"git","url":"git+https://github.com/<o>/<r>.git"}`.
  - `license` is copied from the package's own `package.json` when present, else omitted.
  - `README.md` holds the description sentence.
  - Command: `npm publish --access public`, run in the temp dir.
- Crates: no placeholder.
  - The tag is `tag_for(pkg, <manifest version>)`. If that tag doesn't exist locally, the item is `pending` with "run the release first; bootstrap publishes the tagged version".
  - `git worktree add --detach <tmp> <tag>`, then `cargo publish -p <name> --locked` in it, then `git worktree remove --force <tmp>` (always, on success or failure).
  - Crate publish output goes through `run_redacted` (no Secret is passed; cargo uses its own login).
- Trust:
  - The workflow file and environment are the same as plan's `release/workflow` (`DesiredRelease.workflow`, else `release.yml`) and `DesiredRelease.environment`.
  - npm, in order:
    1. `npm trust list <pkg> --json`. An entry whose repository is `<o>/<r>`, whose workflow file equals ours and whose environment equals ours means `ok`. An unparsable answer counts as "not trusted".
    2. Otherwise, on a TTY: `npm trust github <pkg> --file <workflow> --repo <o>/<r> --env <env> --allow-publish`, with inherited stdin, stdout and stderr.
    3. Without a TTY: a human step `npm-trust:<pkg>` whose title is that exact command.
  - Crates: a human step `crates-trust:<name>` with URL `https://crates.io/crates/<name>/settings`.
- Item kinds (area `release`): `bootstrap_npm`, `bootstrap_crate`, `bootstrap_trust`; key = package name.
- The final hint reads:
  - when every publish item is `ok` or done: `next: re-run the failed "release publish" job at <tag>, or run the release workflow`;
  - otherwise: `next: finish the steps above, then run bootstrap again`.

  It goes on stderr in table mode, and in `Report.text` for `--json` (which never prints it).
- Never read, store or print npm or crates.io credentials. `npm` and `cargo` keep their own login state, and their output goes to stderr.
- Tests never reach a real registry; they use the FakeApi and stub tools from `tests/common`.
- Fast checks before every commit: `cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test`.

## Review Focus

1. **A run interrupted halfway** (npm placeholders done, crate failed). The re-run skips the published placeholders (registry 200), publishes only the crate, and the worktree from the failed run doesn't block it. Test in Task 2.
2. **A checkout with uncommitted changes, or not at the tag.** Crates are still published from the worktree at the tag, never from the working tree. Test in Task 2.
3. **`npm trust list` output in an unexpected shape**, or npm older than 11.15.0. An unparsable answer means "not trusted", and the command is shown or run. Old npm exits 3 in preflight when trust work is needed. Test in Task 3.
4. **Run inside GitHub Actions by mistake.** Exit 3 before any request or tool call. Test in Task 1.
5. **A launcher repo.** All 5 platform packages and the launcher get placeholders and trust, and nothing from `npm/` in the checkout is published. Test in Task 2.

---

## File Structure

```
src/ops/release/bootstrap/mod.rs      release_bootstrap(), BootstrapOpts, preflight, report + hint
src/ops/release/bootstrap/publish.rs  npm placeholder (temp dir) and crate publish (worktree)
src/ops/release/bootstrap/trust.rs    npm trust list parsing, npm trust github, crates link
src/release/tools.rs                  + run_interactive(tool, dir, args) -> Result<(), ToolError>
src/cli.rs, src/lib.rs                + `release bootstrap`
tests/cli_release_bootstrap.rs        integration tests (FakeApi + stub npm/cargo/git)
```

---

### Task 1: Command, preflight and dry run

**Files:**
- Create: `src/ops/release/bootstrap/mod.rs`
- Modify: `src/cli.rs`, `src/lib.rs`, `src/ops/release/mod.rs` (or `src/ops/release.rs`, whichever declares the release submodules)
- Test: `tests/cli_release_bootstrap.rs`

**Interfaces:**
- Consumes:
  - `read_release_model`;
  - `Package`, `PkgKind` and `Package.launcher` (platform packages);
  - `registry::package_exists`;
  - `Desired::read_release` / `DesiredRelease { environment, workflow, packages }`;
  - `tools::{run, run_capture, Tool}`;
  - `repo::git::read_repo_local`;
  - `ops::apply::ask` (prompt helper);
  - `check::exit_code`.
- Produces:
  - `pub struct BootstrapOpts { pub dry_run: bool, pub yes: bool, pub tty: bool }`;
  - `pub fn release_bootstrap(dir: &Path, opts: BootstrapOpts, verbose: bool) -> Result<Report, HoistError>`;
  - `pub(crate) enum Step { NpmPlaceholder(Package), Crate { pkg: Package, tag: String }, NpmTrust(Package), CrateTrust(Package) }`;
  - `pub(crate) fn plan_steps(...) -> (Vec<Step>, Vec<Item>)`: steps to run, plus `ok` and `pending` items.
- The CLI sets `tty` from `std::io::stdin().is_terminal()`. Tests call `release_bootstrap` through the CLI with stdin from `/dev/null` (tty false), or through the library with `tty: true`.

- [ ] **Step 1: Write failing tests:**
  - `refuses_in_actions` (Review Focus 4): with `GITHUB_ACTIONS=true`, exit 3, the message names "release publish", and the FakeApi and stub logs are empty.
  - `dry_run_lists_missing_packages`: npm `GET /<pkg>` 404 and crates `GET /api/v1/crates/<name>` 404 give exit 0 with:
    - `bootstrap_npm` items, action `create`;
    - a `bootstrap_crate` item, action `create`, with the tag in the reason;
    - `bootstrap_trust` items, action `create`.

    `assert_only_get` holds, and the stub npm log shows only `whoami`, `--version` and `trust list` calls.
  - `existing_packages_are_ok_and_untouched`: registry 200 gives `bootstrap_npm` / `bootstrap_crate` items `ok`, and no publish appears in the stub logs.
  - `crate_without_tag_is_pending`.
  - `npm_not_logged_in_exits_3`: stub `npm whoami` exits 1, giving exit 3 with "npm login".
  - `old_npm_exits_3_when_trust_needed` (Review Focus 3): stub `npm --version` prints `11.14.0`, giving exit 3 with "11.15.0".
  - `missing_cargo_exits_3`: a crate is missing and `HOIST_CARGO` points at a non-existent path, giving exit 3 with "cargo".
  - `non_tty_without_yes_exits_3`: no `--dry-run`, no `--yes`, stdin `/dev/null`, missing packages → exit 3 "use --yes", no publish.
- [ ] **Step 2: Run** `cargo test --test cli_release_bootstrap` → FAIL (unknown subcommand).
- [ ] **Step 3: Implement** the preflight in the constraint order: Actions check, then model, then registry reads, then tool checks. Then build the steps and the items for the dry run. A non-dry run with no confirmation exits 3 here. Writing comes in Task 2, so for now a confirmed run returns its plan items unchanged.
- [ ] **Step 4: Run** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: release bootstrap preflight and dry run`

### Task 2: npm placeholders and crate publish

**Files:**
- Create: `src/ops/release/bootstrap/publish.rs`
- Modify: `src/ops/release/bootstrap/mod.rs` (execute steps in order, stopped report, confirmation and decline)
- Test: `tests/cli_release_bootstrap.rs`

**Interfaces:**
- Consumes: `Step`, `plan_steps`, `BootstrapOpts` (Task 1); `tools::{run, run_redacted}`; `ReleaseModel::tag_for`.
- Produces:
  - `pub(crate) fn placeholder_files(pkg: &Package, repo: &GithubRepo, license: Option<&str>) -> (String /* package.json */, String /* README.md */)`;
  - `pub(crate) fn publish_placeholder(pkg: &Package, repo: &GithubRepo, dir: &Path) -> Result<(), HoistError>`;
  - `pub(crate) fn publish_crate(dir: &Path, pkg: &Package, tag: &str) -> Result<(), HoistError>`.
- Order: npm placeholders in model order (a launcher's platform packages before the launcher), then crates. Each write re-checks `package_exists` first, so a race or a re-run skips it.

- [ ] **Step 1: Write failing tests:**
  - `placeholder_has_only_the_allowed_keys`: a unit test on `placeholder_files`; the JSON keys are exactly `name`, `version`, `description`, `repository`, `license`, with values from the constraints.
  - `npm_placeholder_published_from_temp_dir` (Review Focus 5): on the release-launcher fixture with `--yes`, the stub npm log shows `publish --access public` once per platform package and once for the launcher. Each was run in a dir outside the repo (the stub logs `$PWD`) that holds only `package.json` and `README.md`. The checkout's `npm/` stays unchanged (`git status --porcelain` is empty).
  - `crate_published_from_worktree_at_tag` (Review Focus 2):
    - The test tags `v0.1.0`, then commits a change and leaves an uncommitted edit.
    - The stub cargo logs `$PWD`, and `git -C $PWD rev-parse HEAD` equals the tag's commit.
    - After the run, `git worktree list` shows only the main checkout.
  - `rerun_skips_done_and_recovers_worktree` (Review Focus 1): run 1 fails at cargo (exit 1, crate item `failed:`, placeholders done). Run 2 has the registry now 200 for npm and publishes only the crate.
  - `declining_exits_1`: through the library with `tty: true` and the prompt input `n`, every item reads "not applied: declined" and nothing is published.
  - `npm_publish_failure_stops_the_run`: the second placeholder fails, giving exit 1 with items done / failed / not attempted, and no crate publish.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.**
  - Temp dirs come from `std::env::temp_dir()` with a unique name, removed afterwards.
  - The worktree path is `<temp>/hoist-bootstrap-<name>-<pid>`. Before adding it, run `git worktree prune` so a dead worktree from an interrupted run doesn't block.
  - The confirmation prompt reuses `ops::apply::ask`, or a sibling with the bootstrap wording "hoist release bootstrap will publish N package(s)".
- [ ] **Step 4: Run** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: release bootstrap publishes npm placeholders and tagged crates`

### Task 3: Trusted publishers, hint, docs and dogfood

**Files:**
- Create: `src/ops/release/bootstrap/trust.rs`
- Modify: `src/release/tools.rs` (`run_interactive`), `src/ops/release/bootstrap/mod.rs`, `README.md`, `docs/PROGRESS.md`, `CLAUDE.md` (the read-only list gains `release bootstrap --dry-run`; the write list gains `release bootstrap`)
- Test: `tests/cli_release_bootstrap.rs`, unit tests in `trust.rs`

**Interfaces:**
- Consumes: `Step::{NpmTrust, CrateTrust}`, the `DesiredRelease` workflow and environment, `tools::run`.
- Produces:
  - `pub fn run_interactive(tool: Tool, dir: &Path, args: &[&str]) -> Result<(), ToolError>` in tools.rs: stdin, stdout and stderr inherited, with the same env handling as `command()` (no GH_TOKEN/GITHUB_TOKEN).
  - `pub(crate) fn trusted(list_json: &str, repo: &GithubRepo, workflow: &str, env: &str) -> bool`, which is false on any parse error.
  - `pub(crate) fn trust_command(pkg: &str, workflow: &str, repo: &GithubRepo, env: &str) -> Vec<String>`.

- [ ] **Step 1: Write failing tests:**
  - `trusted_matches_repo_workflow_env`: unit tests, with the inputs below.
    - true for a matching entry, accepting either `"repository": "o/r"` or `{"owner":"o","name":"r"}`, and a workflow given as `release.yml` or `.github/workflows/release.yml`;
    - false for another env;
    - false for garbage (Review Focus 3);
    - false for `[]`.
  - `trust_runs_interactively_on_tty`: through the library with `tty: true`, the stub npm log shows `trust list <pkg> --json`, then `trust github <pkg> --file release.yml --repo acme/x --env release --allow-publish`.
  - `trust_skipped_when_listed`: the stub `trust list` prints a matching entry, giving item `ok` "already trusted" and no `trust github` call.
  - `trust_without_tty_is_a_human_step`: `--yes` with stdin `/dev/null` gives exit 2, with a human step `npm-trust:<pkg>` whose title is the exact command.
  - `crates_trust_is_a_link`: a human step `crates-trust:keel-hoist` with URL `https://crates.io/crates/keel-hoist/settings`.
  - `hint_after_success`: stderr contains `next: re-run the failed "release publish" job at v0.1.0`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.**
  - Trust steps run after all publishes.
  - A failed `npm trust github` (non-zero exit) is a failed item, exit 1, and the run stops there.
- [ ] **Step 4: Docs.**
  - README "Releasing" section: replace the manual first-publish steps with `hoist release bootstrap` (dry run first, then real), noting npm ≥ 11.15.0, `npm login`, `cargo login`, and that npm prompts for 2FA.
  - docs/PROGRESS.md: a Done line, and change the hoist first-release item under Blocked to "run `hoist release bootstrap` locally, then re-run the failed publish job".
- [ ] **Step 5: Read-only dogfood.**
  - Run `cargo build --release --locked && target/release/hoist release bootstrap --dry-run -C . --json` on hoist itself, with no real logins.
  - `npm whoami` may fail in the sandbox, giving exit 3. Record exactly what happens in the report.
  - Capture output in the scratchpad (not the repo).
  - Run the token-prefix grep (ghp_, github_pat_, gho_, ghs_ = 0).
- [ ] **Step 6: Run** fast checks → PASS.
- [ ] **Step 7: Commit** `feat: release bootstrap registers trusted publishers`
