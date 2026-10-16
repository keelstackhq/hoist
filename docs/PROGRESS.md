# Progress

## Status

Milestone 1 (read-only), plan `docs/superpowers/plans/2026-10-16-m1-read-only.md`: Tasks 1-12 done (all code tasks).

## Done

- 2026-10-16 Task 1: crate scaffold, CLI skeleton (`explain|plan|check|auth check`), report JSON and table rendering, exit codes, CI workflow.
- 2026-10-16 Task 2: `hoist.toml` loading (`config.rs`): root and target configs with spec §5 defaults, unknown keys rejected naming file and dotted key, TOML syntax errors name file and line.
- 2026-10-16 Task 3: `repo/git.rs`: parse GitHub remotes (https, scp, ssh), read `origin` and the default branch (`origin/HEAD`, else `main`); a missing or non-GitHub origin is a config error (exit 3).
- 2026-10-16 Task 4: `repo/workflows.rs`: jobs (ids and `name:`), secrets (regex, no `GITHUB_TOKEN`, first source wins), environments (string or `{ name }`) and the `merge_group` trigger from `.github/workflows/*.y{a,}ml`, each with a `path:line` source; invalid YAML is a config error naming the file.
- 2026-10-16 Task 5: `repo/targets.rs`: deploy targets (wrangler.jsonc `main` → Workers, `assets` → Pages, `hoist.toml` provider overrides), npm packages (`private: false` or `publishConfig`), crates (not `publish = false`), `renovate.json`; workspace globs limited to literal paths and trailing `/*`; JSONC errors name the file.
- 2026-10-16 Task 6: `desired.rs` joins config and inference into `Desired` (required checks, Pages project default `<repo>-<target>`, account id from config or env, apps, workers); `hoist explain` lists every element sorted by (area, kind, key) with its source; missing account id with Cloudflare targets exits 3; insta snapshot for the `site` fixture.
- 2026-10-16 Task 7: `http.rs` (ureq 3, GET only, rate-limit retries, GitHub Link and Cloudflare page pagination, `Fetched`/`HttpError` with no headers or bodies in messages, `-v` logs `GET <path> <status> <ms>`), `auth.rs` (`Secret` prints `***`, token lookup incl. `gh auth token`), fake API in `tests/common/fake.rs`; `canary_never_printed` stays ignored until Task 10.
- 2026-10-16 Task 8: `github.rs` (`Fetch<T>`, `LiveGithub`, `read_github`: settings, rulesets with details, classic protection, actions permissions, environments, secrets, app installations; 403/404 become `Unknown`) and `diff.rs` (`diff_github` returns items plus human steps; `managed = external` turns create/update into drift); fixtures in `tests/fixtures/api/github/`.
- 2026-10-16 Task 9: `cloudflare.rs` (`LiveCloudflare`, `PagesProject`, `read_cloudflare`: Pages project and Workers script settings; 404 means absent, 403 becomes `Unknown`) and `diff_cloudflare` in `diff.rs` (shares `Ctx` with `diff_github`, now area-aware; pages project create/update, custom domain create, missing worker pending); fixtures in `tests/fixtures/api/cloudflare/`.
- 2026-10-16 Task 10: `ops/plan.rs` and `hoist plan [--area ..]`: reads GitHub (and Cloudflare only when a deploy target exists, so bare repos need no Cloudflare token), merges items sorted by (area, kind, key) and human steps, exit 0 (preview), exit 3 on config/auth errors; `-v` wired into every `Http`; `canary_never_printed` enabled and passing.
- 2026-10-16 Task 11: `verify.rs` (required checks against workflow job ids/names as `github/required_check` items, Pages URL and custom domain anonymous GETs via `http::probe`, `HOIST_PAGES_BASE` for tests; a 404 on an existing project is a failed verify) and `hoist check` (`ops/check.rs`: plan plus verify, exit 1 on create/update/drift/failed verify, else 2 on pending, else 0; `unknown` never fails).
- 2026-10-16 Task 12: `ops/auth_check.rs` and `hoist auth check`: read-probes per permission (`github:administration|secrets|environments|actions:read`, `cloudflare:token:active`, `cloudflare:pages:read`, Cloudflare only when a target exists), `token` items report the source label (`env GITHUB_TOKEN`, `env GH_TOKEN`, `gh auth token`, `env CLOUDFLARE_API_TOKEN`), exit 0 all ok / 1 any missing / 3 no token; `auth.rs` gains `*_with_source`; `secrets_canary` now asserts requests were made and exit codes, with 200, 401 and 500 routes.

## Blocked

Nothing yet.
