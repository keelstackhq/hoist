# hoist M2 (provisioning) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `hoist apply` makes GitHub and Cloudflare match what the repo needs (with verification and human steps), plus `hoist import`, `hoist secrets set` and `hoist secrets rotate`, with a minted, scoped Cloudflare deploy token sealed into GitHub.

**Architecture:** M1's layers stay: inference → live read → pure diff. M2 adds a pure `plan_changes` that turns (Desired, Live) into an ordered list of typed `Change`s, and writer modules that execute one `Change` each. `apply` = read → changes → preflight → confirm → execute in order (stop on first failure) → re-read → verify. Secret values never leave memory except sealed for GitHub.

**Tech Stack:** M1's crates unchanged. New (checked on crates.io 2026-10-16, pin exactly): `crypto_box =0.9.1` (features `seal`, `std`), `base64 =0.23.1`, `zeroize =1.9.1`, `rpassword =7.5.4`, `toml_edit =0.25.17`. Ask before adding anything else.

**Spec:** `docs/superpowers/specs/2026-10-16-hoist-design.md` (§3 CLI, §6 resources, §7.1–7.5, §8.1–8.3, §8.5, §11, §13).

## Global Constraints

- All M1 constraints stay (names, exit codes `0/1/2/3`, the one-object `--json` shape, actions `create|update|ok|drift|pending|unknown`, `rustls` only, `#![forbid(unsafe_code)]`, `HOIST_GITHUB_API` / `HOIST_CLOUDFLARE_API`, unknown `hoist.toml` keys are errors).
- Only `apply`, `secrets set` and `secrets rotate` may send non-GET requests. `explain`, `plan`, `check`, `auth check` and `import` stay GET-only; their existing `assert_only_get` tests must keep passing, and `import` gets one.
- Secret values (tokens, minted token values, `secrets set` values) come only from env or a hidden prompt, never argv; they never appear in stdout, stderr, JSON, logs, error messages or any request body except as a sealed `encrypted_value`. They live in `Secret` (zeroised on drop via `zeroize`).
- Writes are idempotent and run in dependency order: Cloudflare project → domain → token (sealed into `CLOUDFLARE_API_TOKEN`) → `CLOUDFLARE_ACCOUNT_ID` secret → environment → ruleset → repo settings → Actions permissions → token revocations (`--prune`). On the first failed write hoist stops, reports done/not done, and exits 1.
- `apply` without `--yes` asks for confirmation on a TTY; without a TTY and without `--yes` it exits 3 ("use --yes in non-interactive runs").
- Names: ruleset `hoist: default branch`; minted Cloudflare token `hoist:<owner>/<repo>:deploy`; GitHub secrets `CLOUDFLARE_API_TOKEN`, `CLOUDFLARE_ACCOUNT_ID`.
- Path segments from config (project names, environment names, secret names, hostnames) are percent-encoded with one helper before they go into a URL.
- Settings hoist does not model are never touched; deletes only with `--prune`, and only for Cloudflare tokens named `hoist:<owner>/<repo>:*` that are not the active deploy token.
- Tests never reach a real API (R2 from M1: `tests/common::hoist()`); the fake API records method, path and body.
- Fast checks before every commit: `cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test`.

## Review Focus

1. **The repo's public key changes between read and write** (GitHub rotates it, or two runs race): a 422 on the secret PUT re-fetches the key once and retries; a second failure is exit 1 naming the secret. Test in Task 4.
2. **A minted token whose GitHub secret write fails**: the new token must be revoked in the same run (no orphan, no value lost in a log), and the item reports the failure. Test in Task 6.
3. **A partly applied run is re-run**: the second `apply` plans only what is still missing and does not mint a second token or recreate the project. Test in Task 7.
4. **Non-interactive shell without `--yes`** (CI, piped stdin): exit 3 before any request that writes. Test in Task 7.
5. **`import --write` on an existing `hoist.toml` with comments**: comments and unrelated keys survive; only the needed keys are added or changed. Test in Task 10.

---

## File Structure

```
src/http.rs             + send_json (POST/PUT/PATCH/DELETE), Written, seg() path encoding
src/change.rs           Change enum, plan_changes(), Change::describe()
src/github.rs           + Ruleset.id, + admin permission read, + public key read
src/github_write.rs     execute GitHub changes (settings, ruleset, actions perms, environment, secret)
src/seal.rs             seal(public_key_b64, &Secret) -> base64 sealed box
src/cloudflare.rs       + token list/expiry read, + permission groups read
src/cloudflare_write.rs execute Cloudflare changes (project, domain, mint, revoke)
src/auth.rs             Secret zeroised on drop; + read_secret_value(name)
src/ops/apply.rs        preflight, confirm, execute, verify
src/ops/secrets.rs      secrets set / rotate
src/ops/import.rs       import [--write]
src/cli.rs              + apply, import, secrets subcommands
tests/cli_apply.rs, tests/github_write.rs, tests/cloudflare_write.rs, tests/seal.rs,
tests/cli_secrets.rs, tests/cli_import.rs, tests/common/fake.rs (+ bodies)
```

---

### Task 1: HTTP writes and path encoding

**Files:**
- Modify: `src/http.rs`, `tests/common/fake.rs`, `tests/http.rs`

**Interfaces:**
- Produces:
  - `pub enum Method { Post, Put, Patch, Delete }`
  - `pub struct Written { pub status: u16, pub body: Option<serde_json::Value> }` (`None` for 204 or an empty body)
  - `impl Http { pub fn send_json(&self, method: Method, path: &str, body: Option<&serde_json::Value>) -> Result<Written, HttpError> }`: 2xx → `Ok`; 401 → `HttpError::Unauthorized`; other 4xx/5xx → new `HttpError::Rejected { service, method, path, status, message }` where `message` is the API's `message` / `errors[0].message` (never the request body). Retries (Retry-After, max 3) only for `Put`, `Patch`, `Delete`; `Post` never retries.
  - `-v` line `<METHOD> <path> <status|error> <ms>ms` (path without query, as M1).
  - `pub fn seg(s: &str) -> String`: percent-encodes everything except `A-Za-z0-9-._~`.
  - Fake: routes accept any method key (`"PUT /repos/a/b/x"`); `requests()` gains the body as `Option<String>`; `assert_only_get()` unchanged.

- [ ] **Step 1: Write failing tests** in `tests/http.rs`:
  - `put_returns_written_with_body`: fake `PUT /x` 201 `{"id":1}` → `Written{status:201, body:Some(..)}`; logged body equals the JSON sent.
  - `delete_204_has_no_body`: `DELETE /x` 204 → `body: None`.
  - `post_is_not_retried`: `POST /x` 429 with `Retry-After: 0` → `Err(Rejected{status:429,..})`, exactly 1 request.
  - `put_is_retried_once_after_429`: 429 then 200 → `Ok`, 2 requests.
  - `rejected_message_never_contains_request_body`: body `{"value":"hoist-canary-body"}`, 422 → Display lacks `hoist-canary-body`.
  - `seg_encodes_reserved`: `seg("a/b c?")` == `"a%2Fb%20c%3F"`; `seg("keelstack-landing")` unchanged.
- [ ] **Step 2: Run** `cargo test --test http` → FAIL.
- [ ] **Step 3: Implement** with ureq 3's `.post/.put/.patch/.delete` + `send_json`; reuse the M1 status/hint mapping.
- [ ] **Step 4: Run** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: http writes and path segment encoding`

### Task 2: Change model

**Files:**
- Create: `src/change.rs`, `tests/change.rs`
- Modify: `src/github.rs` (add `pub id: u64` to `Ruleset`, parsed from the list/detail), `src/diff.rs` (secret without producer → `pending` + human step)

**Interfaces:**
- Consumes: `Desired`, `LiveGithub`, `LiveCloudflare`, `Fetch`.
- Produces:
  - `pub enum Change {`
    `RepoSettings { patch: serde_json::Map<String, Value> }` (only differing modelled fields),
    `Ruleset { existing_id: Option<u64>, branch: String, required_checks: Vec<String>, merge_queue: bool }`,
    `ActionsPermissions { default: String }`,
    `Environment { name: String, branch: String }`,
    `AccountIdSecret`, (writes desired account id into `CLOUDFLARE_ACCOUNT_ID`)
    `DeployToken { name: String, replace_ids: Vec<String> }`, (mint + seal into `CLOUDFLARE_API_TOKEN`; `replace_ids` revoked after success)
    `PagesProject { name: String, production_branch: String, exists: bool }`,
    `Domain { project: String, host: String }`,
    `RevokeToken { id: String, name: String }` }
  - `pub struct ChangeOpts { pub prune: bool, pub areas: Vec<Area> }`
  - `pub fn plan_changes(d: &Desired, gh: Option<&LiveGithub>, cf: Option<&LiveCloudflare>, opts: &ChangeOpts) -> Vec<Change>`: pure, returns dependency order (Global Constraints). Only `create`/`update` items produce changes; `Managed::External` areas produce none; `unknown` produces none.
  - Producible secrets: `CLOUDFLARE_API_TOKEN` (→ `DeployToken`) and `CLOUDFLARE_ACCOUNT_ID` (→ `AccountIdSecret`) when the repo has a Cloudflare target and an account id; every other missing secret is `pending` in `diff_github` with `HumanStep { id: "secret:<NAME>", title: "set secret <NAME>", url: "hoist secrets set <NAME>", status: "pending" }`.
  - `impl Change { pub fn key(&self) -> (&'static str /*area*/, &'static str /*kind*/, String /*key*/) }` matching the M1 item scheme (token kind `api_token`, key = token name).
  - `LiveCloudflare` gains `pub tokens: Fetch<Vec<CfToken>>` with `CfToken { id, name, expires_on: Option<String>, status }` — reader added in Task 6; until then tests construct it.

- [ ] **Step 1: Write failing tests** in `tests/change.rs` (pure, constructed inputs):
  - `in_sync_has_no_changes`.
  - `order_is_dependency_order`: everything missing → kinds in order `PagesProject, Domain, DeployToken, AccountIdSecret, Environment, Ruleset, RepoSettings, ActionsPermissions`; with `prune`, `RevokeToken`s come last.
  - `existing_ruleset_is_updated_by_id`: live ruleset id 42 with other checks → `Ruleset{existing_id:Some(42),..}`.
  - `external_area_yields_no_changes`.
  - `unknown_yields_no_changes`.
  - `token_present_and_secret_present_is_no_change`; `token_present_secret_missing_mints_and_replaces`: → `DeployToken{replace_ids:[old]}`.
  - `prune_revokes_leftover_tokens_only_with_flag`: two `hoist:o/r:deploy` tokens + one `hoist:o/r:old` → without prune none; with prune `RevokeToken` for the non-newest ones; a token named `other` never.
  - `user_secret_is_pending_human_step` (diff): missing `SENTRY_DSN` → item `pending`, human step `secret:SENTRY_DSN`.
- [ ] **Step 2: Run** `cargo test --test change` → FAIL.
- [ ] **Step 3: Implement.** "Newest" token = latest `issued_on`/`modified_on` if present, else the last in list order.
- [ ] **Step 4: Run** fast checks; update snapshots that change because of the pending secret (read each `.snap.new`).
- [ ] **Step 5: Commit** `feat: typed change plan`

### Task 3: GitHub writers (settings, ruleset, Actions permissions, environment)

**Files:**
- Create: `src/github_write.rs`, `tests/github_write.rs`

**Interfaces:**
- Produces: `pub fn execute_github(http: &Http, d: &Desired, change: &Change) -> Result<(), HoistError>` for `RepoSettings` (`PATCH /repos/{o}/{r}` with the patch map), `Ruleset` (`POST /repos/{o}/{r}/rulesets` or `PUT .../rulesets/{id}`), `ActionsPermissions` (`PUT /repos/{o}/{r}/actions/permissions/workflow` `{"default_workflow_permissions": <v>}`), `Environment` (`PUT /repos/{o}/{r}/environments/{seg(name)}` with `{"deployment_branch_policy":{"protected_branches":false,"custom_branch_policies":true}}`, then `POST .../environments/{name}/deployment-branch-policies` `{"name": <branch>, "type": "branch"}`; a 303/422 "already exists" on the policy is success). Other variants → `unreachable`-free error `HoistError::Usage("not a GitHub change")`.
- Ruleset body (exact):
  ```json
  {"name":"hoist: default branch","target":"branch","enforcement":"active",
   "conditions":{"ref_name":{"include":["~DEFAULT_BRANCH"],"exclude":[]}},
   "rules":[{"type":"required_status_checks","parameters":{"strict_required_status_checks_policy":false,
     "required_status_checks":[{"context":"ci-ok"}]}}]}
  ```
  plus `{"type":"merge_queue","parameters":{...defaults}}` only when `merge_queue` is true (use GitHub's documented defaults: `merge_method":"MERGE"`, `grouping_strategy":"ALLGREEN"`, `max_entries_to_build":5`, `min_entries_to_merge":1`, `max_entries_to_merge":5`, `min_entries_to_merge_wait_minutes":5`, `check_response_timeout_minutes":60`).

- [ ] **Step 1: Write failing tests** (fake API, assert method + path + parsed body):
  - `settings_patch_sends_only_changed_fields`.
  - `ruleset_create_body_exact` (compare `serde_json::Value` with the body above).
  - `ruleset_update_uses_put_by_id`.
  - `ruleset_with_merge_queue_adds_rule`.
  - `actions_permissions_put`.
  - `environment_put_then_branch_policy`; `branch_policy_already_exists_is_ok` (422 `{"message":"…already exists"}`).
  - `forbidden_write_is_rejected_error`: 403 → `Err` whose message names the path and the API message.
- [ ] **Step 2: Run** `cargo test --test github_write` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: GitHub writers for settings, ruleset, actions and environments`

### Task 4: Sealed GitHub secrets

**Files:**
- Create: `src/seal.rs`, `tests/seal.rs`
- Modify: `src/auth.rs` (`Secret` zeroises on drop), `src/github.rs` (public key read), `src/github_write.rs`

**Interfaces:**
- Produces:
  - `pub fn seal(public_key_b64: &str, value: &Secret) -> Result<String, HoistError>`: libsodium sealed box via `crypto_box::PublicKey::seal`, output base64 (standard alphabet).
  - `pub fn put_secret(http: &Http, d: &Desired, name: &str, value: &Secret) -> Result<(), HoistError>`: `GET /repos/{o}/{r}/actions/secrets/public-key` (`key_id`, `key`) → `PUT /repos/{o}/{r}/actions/secrets/{seg(name)}` `{"encrypted_value", "key_id"}`; on 422 re-fetch the key once and retry.
  - `execute_github` handles `AccountIdSecret` via `put_secret` with the desired account id.

- [ ] **Step 1: Write failing tests:**
  - `seal_roundtrips_with_test_keypair`: generate a `crypto_box::SecretKey` in the test, seal `hoist-canary-seal`, unseal with the secret key → equal.
  - `put_secret_body_has_no_plaintext`: fake key; logged PUT body contains `encrypted_value` and `key_id`, not `hoist-canary-seal`; test unseals `encrypted_value` → canary.
  - `stale_key_is_refetched_once` (Review Focus 1): first PUT 422, key re-read, second PUT 201 → Ok, 2 key reads.
  - `second_422_is_an_error_naming_the_secret`.
  - `secret_debug_still_redacted` (existing behaviour stays).
- [ ] **Step 2: Run** `cargo test --test seal` → FAIL.
- [ ] **Step 3: Implement** (`zeroize::Zeroizing<String>` inside `Secret`).
- [ ] **Step 4: Run** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: sealed GitHub secrets`

### Task 5: Cloudflare writers (Pages project, domain)

**Files:**
- Create: `src/cloudflare_write.rs`, `tests/cloudflare_write.rs`

**Interfaces:**
- Produces: `pub fn execute_cloudflare(http: &Http, d: &Desired, change: &Change) -> Result<Option<HumanStep>, HoistError>`:
  - `PagesProject{exists:false}` → `POST /accounts/{acc}/pages/projects` `{"name", "production_branch"}`; `exists:true` → `PATCH /accounts/{acc}/pages/projects/{seg(name)}` `{"production_branch"}`.
  - `Domain` → `POST /accounts/{acc}/pages/projects/{seg(project)}/domains` `{"name": host}`. If Cloudflare rejects because the zone is not on the account (any 4xx whose message mentions `zone`), return `Ok(Some(HumanStep{id:"domain:<host>", title:"add <host>'s zone to Cloudflare or point a CNAME to <project>.pages.dev", url:"https://dash.cloudflare.com/", status:"pending"}))` instead of failing.
- [ ] **Step 1: Write failing tests:** `create_project_post_body`, `update_production_branch_patch`, `attach_domain_post`, `domain_without_zone_is_human_step`, `project_name_is_encoded` (name `a b` → path `a%20b`).
- [ ] **Step 2: Run** `cargo test --test cloudflare_write` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: Cloudflare writers for Pages projects and domains`

### Task 6: Minted deploy token

**Files:**
- Modify: `src/cloudflare.rs` (token list + permission groups), `src/cloudflare_write.rs`, `src/diff.rs` (token expiry item), `tests/cloudflare_write.rs`, `tests/cloudflare_diff.rs`

**Interfaces:**
- Produces:
  - Reader: `GET /user/tokens` (paginated) → `LiveCloudflare.tokens`, only tokens whose name starts with `hoist:<owner>/<repo>:`; 403 → `Unknown`.
  - Diff: item `cloudflare/api_token/hoist:<o>/<r>:deploy`: `create` when absent (and a deploy target exists), `update` with reason "expires in N days; run `hoist secrets rotate`" when `expires_on` is within 30 days, else `ok`.
  - `pub fn mint_deploy_token(http: &Http, d: &Desired, name: &str) -> Result<(String /*id*/, Secret /*value*/), HoistError>`: look up permission group ids via `GET /user/tokens/permission_groups` by exact name `"Pages Write"` (Pages targets) and `"Workers Scripts Write"` (Workers targets); `POST /user/tokens` with `{"name", "policies":[{"effect":"allow","resources":{"com.cloudflare.api.account.<acc>":"*"},"permission_groups":[{"id":…}]}], "expires_on": <now + 365 days, RFC 3339>}`; value from `result.value`, wrapped in `Secret` immediately. Scope is account-level (Cloudflare cannot scope Pages to one project; recorded in spec §13).
  - `pub fn revoke_token(http: &Http, id: &str) -> Result<(), HoistError>`: `DELETE /user/tokens/{seg(id)}`; 404 is success.
  - `execute_cloudflare` for `DeployToken`: mint → `put_secret(CLOUDFLARE_API_TOKEN)` (Task 4) → revoke `replace_ids`. If `put_secret` fails, revoke the new token, then return the error (Review Focus 2). For `RevokeToken`: `revoke_token`.
  - Note: `execute_cloudflare` now needs the GitHub `Http` for `DeployToken`; give it `fn execute_cloudflare(cf: &Http, gh: &Http, d: &Desired, change: &Change)` and update Task 5's call sites.

- [ ] **Step 1: Write failing tests:**
  - `mint_body_and_scope`: logged POST body has the name, the account resource and the Pages Write group id; no plaintext token in any logged body.
  - `minted_value_only_reaches_github_sealed`: fake returns `result.value = "hoist-canary-mint"`; the GitHub PUT body unseals to it; it appears in no other request body, stdout or stderr (run through `apply` later in Task 7; here assert request bodies).
  - `secret_write_failure_revokes_new_token` (Review Focus 2): GitHub PUT 403 twice → error; a `DELETE /user/tokens/<new id>` was sent.
  - `old_tokens_revoked_after_success`.
  - `expiring_token_is_update` (diff), `missing_token_is_create`, `tokens_forbidden_is_unknown`.
- [ ] **Step 2: Run** the two test files → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: mint and seal the Cloudflare deploy token`

### Task 7: `hoist apply`

**Files:**
- Create: `src/ops/apply.rs`, `tests/cli_apply.rs`
- Modify: `src/cli.rs` (`apply --yes --prune --area`), `src/lib.rs`, `src/ops/plan.rs` (`PlanState` also returns `github: Option<LiveGithub>`), `src/github.rs` (read `permissions.admin` from `GET /repos/{o}/{r}` into `RepoSettings.admin: Option<bool>`)

**Interfaces:**
- Produces: `pub fn apply(dir: &Path, areas: &[Area], yes: bool, prune: bool, verbose: bool) -> Result<Report, HoistError>`:
  1. `run_with_state` (reads) → `plan_changes`.
  2. No changes and no pending → report `ok`, exit 0, no writes.
  3. Preflight before any write: GitHub changes need `admin == Some(true)` (else exit 3 "the token needs admin on <o>/<r> (Administration: write)"); Cloudflare changes need a Cloudflare token that verifies active (`GET /user/tokens/verify`) and an account id (else exit 3).
  4. Confirmation: `yes` → continue; stdin is a TTY (`std::io::IsTerminal`) → print the change list to stderr and read `y`/`yes`; otherwise exit 3 "use --yes in non-interactive runs" (Review Focus 4).
  5. Execute in order via `execute_cloudflare` / `execute_github`; the first error stops the run: report items for done changes (reason "applied"), the failed one (reason "failed: <error>"), and not-attempted ones (reason "not attempted"); exit 1.
  6. After success: re-run `check` logic (M1) and return its report with `command: "apply"`; human steps (apps, user secrets, domain zones) → exit 2 when nothing else failed.
- Spec §7.2 step 1: reword to the preflight this task builds (GitHub admin permission from `GET /repos/{o}/{r}`, Cloudflare token verify and account id), since fine-grained GitHub tokens cannot list their own write permissions.
- [ ] **Step 1: Write failing tests** (fake API with stateful routes: a route list per key answers in order):
  - `in_sync_apply_makes_no_writes`: exit 0, `assert_only_get()`.
  - `full_apply_creates_everything`: missing project, token, secrets, environment, ruleset → writes in the Global Constraints order (assert the logged non-GET sequence), exit 0 after the post-apply reads return the new state.
  - `non_tty_without_yes_exits_3_before_writes` (Review Focus 4): stdin from `/dev/null`, no `--yes` → exit 3, only GETs logged.
  - `failure_stops_and_reports`: ruleset POST 403 → exit 1; items show applied / failed / not attempted; no request after the failed one.
  - `rerun_after_partial_apply_continues` (Review Focus 3): live state now has the project and token+secret → second run writes only environment and ruleset, sends no `POST /user/tokens`.
  - `non_admin_token_exits_3_before_writes`.
  - `pending_human_step_exits_2`.
  - `canary_mint_never_printed`: run `apply --yes -v --json` with the mint canary; stdout/stderr lack it.
  - `read_commands_still_get_only`: `plan`, `check`, `auth check` on the same fake → `assert_only_get()`.
- [ ] **Step 2: Run** `cargo test --test cli_apply` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: hoist apply`

### Task 8: `hoist secrets set`

**Files:**
- Create: `src/ops/secrets.rs`, `tests/cli_secrets.rs`
- Modify: `src/cli.rs` (`secrets set NAME`, `secrets rotate [NAME]`), `src/auth.rs`

**Interfaces:**
- Produces:
  - `pub fn read_secret_value(name: &str) -> Result<Secret, HoistError>`: env var `name` if set and non-empty; else, if stdin is a TTY, `rpassword::prompt_password("value for <name>: ")`; else exit 3 "set <name> in the environment or run in a terminal".
  - `pub fn secrets_set(dir: &Path, name: &str, verbose: bool) -> Result<Report, HoistError>`: validates `name` matches `^[A-Z_][A-Z0-9_]*$` (GitHub's rule; else exit 3), `put_secret`, report item `github/secret/<NAME>` action `ok` reason "set". Requires admin (same preflight as apply).
- [ ] **Step 1: Write failing tests:** `set_from_env_seals_value` (unseal body → canary; canary absent from output), `no_env_no_tty_exits_3`, `invalid_name_exits_3`, `value_never_in_argv_path` (the CLI has no flag that takes a value: `hoist secrets set X --value y` → exit 3 usage error).
- [ ] **Step 2: Run** `cargo test --test cli_secrets` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: hoist secrets set`

### Task 9: `hoist secrets rotate`

**Files:**
- Modify: `src/ops/secrets.rs`, `tests/cli_secrets.rs`, spec §8.3

**Interfaces:**
- Produces: `pub fn secrets_rotate(dir: &Path, name: Option<&str>, verbose: bool) -> Result<Report, HoistError>`. Only `CLOUDFLARE_API_TOKEN` is rotatable in M2 (`name` None means all rotatable; any other name → exit 3 "only minted secrets can be rotated: CLOUDFLARE_API_TOKEN"). Steps: mint new (`mint_deploy_token`), verify the new value with `GET /user/tokens/verify` using a one-off `Http` built from it, `put_secret`, revoke every other `hoist:<o>/<r>:deploy` token. Verify failure → revoke the new token, exit 1.
- Spec §8.3: replace "triggers or waits for a successful deploy check" with "verifies the new token with Cloudflare's token verify endpoint" (deploy checks need a deploy run, which M4's workflows provide).
- [ ] **Step 1: Write failing tests:** `rotate_mints_sets_and_revokes_old`, `rotate_unknown_name_exits_3`, `verify_failure_revokes_new_token`, `canary_new_value_never_printed`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: hoist secrets rotate`

### Task 10: `hoist import`

**Files:**
- Create: `src/ops/import.rs`, `tests/cli_import.rs`
- Modify: `src/cli.rs` (`import --write`), `src/lib.rs`

**Interfaces:**
- Produces: `pub fn import(dir: &Path, write: bool, verbose: bool) -> Result<Report, HoistError>`. Reads GitHub (and Cloudflare when a target exists). For each modelled field where live differs from desired and live is known, emits an override: `[github] allow_auto_merge`, `delete_branch_on_merge`, `actions_permissions`, `merge_queue`, `required_checks` (from the live `hoist: default branch` ruleset), `visibility`; `[deploy] project` per target when exactly one live Pages project starts with the target name and the inferred one is missing. Other live rulesets are listed as comments `# live ruleset "<name>" is not managed by hoist`.
- Output: report items kind `override`, key `hoist.toml:<table>.<key>` (or `apps/<t>/hoist.toml:deploy.project`), reason = the TOML line; without `--write` the TOML text also goes to stdout in table mode (not in `--json`). `--write` edits files with `toml_edit::DocumentMut`, keeping comments and unrelated keys (Review Focus 5). GET-only.
- [ ] **Step 1: Write failing tests:** `import_prints_overrides_for_differences`, `in_sync_imports_nothing`, `write_preserves_comments_and_keys` (existing file with a comment and `[cloudflare] account_id` survives byte-for-byte except the added lines), `import_is_get_only`, `after_write_plan_shows_no_updates` (run `import --write` then `plan` on the same fake → no `update` items for those fields).
- [ ] **Step 2: Run** `cargo test --test cli_import` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** fast checks → PASS.
- [ ] **Step 5: Commit** `feat: hoist import`

### Task 11: Docs, spec facts and dogfood

**Files:**
- Modify: `README.md`, `docs/PROGRESS.md`, spec §3 (no change expected), §6 (token row: account-level scope), §8.1 (write permissions now used), §13 (facts checked)

- [ ] **Step 1:** README: `apply` (preflight, confirmation, `--yes`, order, partial failure and re-run), `secrets set/rotate`, `import`, the minted token, required permissions (GitHub admin; Cloudflare "API Tokens: Edit" locally).
- [ ] **Step 2:** Dogfood on `keelstackhq/example-app` **only if the user has explicitly approved writes for this run**; otherwise run `plan` and `import` (read-only) there and record under Blocked: "dogfood `hoist apply` on keelstackhq/example-app needs the owner's go-ahead plus a GitHub token with Administration/Secrets/Environments/Actions write and a Cloudflare token with API Tokens: Edit".
- [ ] **Step 3:** Record every observed API fact (or "not verified") in spec §13 and PROGRESS with the date; never print token values (check with a prefix grep that counts, as in M1).
- [ ] **Step 4:** Fast checks → PASS.
- [ ] **Step 5: Commit** `docs: M2 docs and dogfood results`
