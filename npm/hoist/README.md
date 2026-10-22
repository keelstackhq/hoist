# @keelstackhq/hoist

npm launcher for [hoist](https://github.com/keelstackhq/hoist): provision and verify a repo's
GitHub and Cloudflare setup from what the repo already says, and release it.

```bash
npm install -g @keelstackhq/hoist
hoist --help
```

hoist is one static Rust binary. This package holds no code of its own: npm installs the binary
for your platform through an optional dependency, and `hoist` runs it with your arguments, stdio
and exit code.

| Platform | Package |
| --- | --- |
| Linux x64 (static musl) | `@keelstackhq/hoist-linux-x64` |
| Linux arm64 (static musl) | `@keelstackhq/hoist-linux-arm64` |
| macOS x64 | `@keelstackhq/hoist-darwin-x64` |
| macOS arm64 | `@keelstackhq/hoist-darwin-arm64` |
| Windows x64 | `@keelstackhq/hoist-win32-x64` |

Install without `--omit=optional` (or `--no-optional`), or the launcher cannot find the binary.
Other platforms: `cargo install keel-hoist`, or the binaries on the GitHub Releases page.

License: MIT.
