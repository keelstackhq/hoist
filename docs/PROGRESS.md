# Progress

## Status

Milestone 1 (read-only), plan `docs/superpowers/plans/2026-10-16-m1-read-only.md`: Tasks 1-4 done.

## Done

- 2026-10-16 Task 1: crate scaffold, CLI skeleton (`explain|plan|check|auth check`), report JSON and table rendering, exit codes, CI workflow.
- 2026-10-16 Task 2: `hoist.toml` loading (`config.rs`): root and target configs with spec §5 defaults, unknown keys rejected naming file and dotted key, TOML syntax errors name file and line.
- 2026-10-16 Task 3: `repo/git.rs`: parse GitHub remotes (https, scp, ssh), read `origin` and the default branch (`origin/HEAD`, else `main`); a missing or non-GitHub origin is a config error (exit 3).
- 2026-10-16 Task 4: `repo/workflows.rs`: jobs (ids and `name:`), secrets (regex, no `GITHUB_TOKEN`, first source wins), environments (string or `{ name }`) and the `merge_group` trigger from `.github/workflows/*.y{a,}ml`, each with a `path:line` source; invalid YAML is a config error naming the file.

## Blocked

Nothing yet.
