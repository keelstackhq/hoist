# hoist

Provision and verify a repo's GitHub and Cloudflare setup from what the repo already says
(`hoist explain|plan|check|auth check|import` read; `apply` and `secrets set|rotate` write after
a preflight; `release plan|prepare|publish` version, tag and publish; `release bootstrap` does the first publish locally). One static Rust binary. Part of the keelstackhq family (hull, splice, hoard).

- Spec: `docs/superpowers/specs/2026-10-16-hoist-design.md` — source of truth. Read only the
  section you touch (§3 CLI, §4 inference, §5 hoist.toml, §6 resources, §8 auth); update it in
  the same commit when behaviour changes.
- Plans: `docs/superpowers/plans/` (one per milestone; M1 read-only, M2 provisioning). Tick tasks as they land.
- Progress: `docs/PROGRESS.md` — Status, Done (one line per task), Blocked (human-only items).

## Layout

```
src/main.rs      parse (clap errors -> exit 3), run, print table or JSON, exit code
src/cli.rs       Cli, Command, Area (github|cloudflare|npm|crates|release); global -C, --json, --no-color, -v
src/lib.rs       run(Cli) -> Result<Report, HoistError>
src/error.rs     HoistError (always exit 3, message says how to fix)
src/report.rs    Action, Item, Verify, HumanStep, Report; render_json, render_table
src/http.rs      GET and write client: retries, no redirects on writes, `seg()` path encoding
src/github.rs, src/cloudflare.rs   live reads (LiveGithub, LiveCloudflare)
src/change.rs    typed write plan (`plan_changes`), live deploy token (R29/R33)
src/github_write.rs, src/cloudflare_write.rs   writers; Cloudflare token mint/revoke
src/seal.rs      sealed GitHub secrets; src/timeutil.rs  RFC 3339, HOIST_NOW
src/ops/         one module per command: explain, plan, check, apply, import, secrets, auth_check
src/release/     release model: commits, planner, git log, editors, launcher, registry, OIDC, tools
src/ops/release/ release plan, prepare, publish; src/diff_release.rs: npm/crates/release plan items
tests/           cli_*.rs integration tests; common/mod.rs builds the scrubbed `hoist` command
.github/workflows/ci.yml   job ci: fmt, clippy, test, node tests; ci-ok: required check, fails unless ci succeeded
```

## Commands

```bash
export PATH=/root/.cargo/bin:$PATH
cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings
cargo test                          # unit + integration
cargo test --test cli_basics        # one file
cargo nextest run                   # if installed
```

Run the fast checks (fmt, clippy, test) before every commit.

## Rules that must never break

- **Exit codes:** 0 in sync · 1 drift, failed write or failed verification · 2 human step pending
  · 3 usage, config, auth or API error (message says how to fix it). clap usage errors exit 3 too.
- **`--json` stdout is exactly one object** `{version:1, command, ok, exit_code, items, human_steps}`.
  Logs go to stderr.
- **Actions** are `create`, `update`, `ok`, `drift`, `pending`, `unknown`.
- **Reads stay GET-only:** `explain`, `plan`, `check`, `auth check`, `import`, `release plan`,
  `release publish --dry-run` and `release bootstrap --dry-run` send only GET requests (tests
  assert it); only `apply`, the `secrets` commands, `release prepare`, `release publish` and
  `release bootstrap` write.
- **Never leak secrets:** token values never reach stdout, stderr, JSON or logs. Tokens come
  from env or `gh auth token`, never argv.
- **rustls only**, no OpenSSL; `#![forbid(unsafe_code)]`.
- **API bases** come from `HOIST_GITHUB_API` and `HOIST_CLOUDFLARE_API`; tests point them at a
  fake server or `http://127.0.0.1:9`.
- **`hoist.toml` unknown keys are an error** (exit 3, names the key and file).
- Ruleset name `hoist: default branch`; minted-token prefix `hoist:` (M2).
- **Tests never reach a real API.** Use `tests/common::hoist()`: it removes GITHUB_TOKEN,
  GH_TOKEN, CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID. The dev environment has real tokens.

## How to work here

- Test first: failing test, watch it fail, implement, watch it pass; one task per commit,
  message as in the plan (`feat:`, `fix:`, `test:`, `ci:`, `docs:`).
- Pin crates exactly (see the plan's Tech Stack). Ask before adding any other crate.
- Read big files by section (offset/limit or grep), not whole.
- Public behaviour (flags, JSON, exit codes) changes only with the spec and README.
- Show real command output before claiming something works.
- Human-only items (tokens, repo settings, releases, npm scopes) go to Blocked in
  `docs/PROGRESS.md` with the exact action.

## Gotchas

- `TMPDIR` is `/` in some containers; set `TMPDIR` to a writable dir before `cargo test`.
- clap exits 2 on usage errors by default; `main` maps every non-help/version error to 3.
- `rust-toolchain.toml` pins 1.97; CI installs the same version.
