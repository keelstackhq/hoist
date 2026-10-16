# hoist

Provision and verify a repo's GitHub and Cloudflare setup from what the repo already says.
A single Rust binary: `hoist explain`, `hoist plan`, `hoist check`, `hoist auth check`.

Status: milestone 1 (read-only) in progress. Design: `docs/superpowers/specs/2026-10-16-hoist-design.md`.

```bash
hoist explain --json -C path/to/repo
hoist plan --area github
```

Exit codes: 0 in sync, 1 drift or failed verification, 2 human step pending, 3 usage, config or
auth error. `--json` prints exactly one object on stdout; logs go to stderr.
