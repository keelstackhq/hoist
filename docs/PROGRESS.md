# Progress

## Status

Milestone 1 (read-only), plan `docs/superpowers/plans/2026-10-16-m1-read-only.md`: Tasks 1-3 done.

## Done

- 2026-10-16 Task 1: crate scaffold, CLI skeleton (`explain|plan|check|auth check`), report JSON and table rendering, exit codes, CI workflow.
- 2026-10-16 Task 2: `hoist.toml` loading (`config.rs`): root and target configs with spec §5 defaults, unknown keys rejected naming file and dotted key, TOML syntax errors name file and line.
- 2026-10-16 Task 3: `repo/git.rs`: parse GitHub remotes (https, scp, ssh), read `origin` and the default branch (`origin/HEAD`, else `main`); a missing or non-GitHub origin is a config error (exit 3).

## Blocked

Nothing yet.
