# Changelog

## 0.2.0 (2026-10-22)

### Features

- release bootstrap registers trusted publishers (39aca47)
- release bootstrap publishes npm placeholders and tagged crates (e77e6f3)
- release bootstrap preflight and dry run (f735610)

### Fixes

- wait up to 5 minutes for npm after publish; table lists failed checks (837eb1c)
- bootstrap reads every npm trust config and cleans stale worktrees (e53442d)
- bootstrap hint follows the report and waits for trust (cd7a086)
- bootstrap publishes interactively so 2FA works (93a5d43)
- bootstrap confirms only real writes (36a4c75)
