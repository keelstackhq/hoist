# Quickstart

```bash
npm install -g @keelstackhq/hoist        # or: cargo install keel-hoist
export GITHUB_TOKEN=...                  # or `gh auth login`; never pass tokens as arguments
cd your-repo                             # needs a GitHub `origin`
```

1. `hoist explain`: the desired model and where each item came from. No network writes.
2. `hoist plan`: desired vs live GitHub (and Cloudflare, if the repo has a target).
3. `hoist auth check`: which permissions your tokens have vs need.
4. `hoist apply`: shows the plan, asks to confirm (`--yes` in CI), writes, then verifies.
5. `hoist check`: plan + verify, read-only. Run it in CI or after every change.

Exit codes: 0 in sync, 1 drift or failure, 2 a human step is pending, 3 usage, config, auth or
API error. With `--json`, stdout is one object, also on exit 3 (`ok: false`, `error`).

Settings live in the repo files hoist already reads; add a `hoist.toml` only to override.
Releasing: `hoist release plan`, then the `release` action (README "The release action").
Agents: read `skills/hoist/SKILL.md` first. Full docs: README.md.
