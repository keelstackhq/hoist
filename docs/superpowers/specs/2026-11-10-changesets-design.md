# hoist — changesets as the release source

Status: design, not scheduled. Owner request (2026-11-10): "a similar approach like changesets,
ideally the same one … if there is a changeset for one of the rust packages, the workflow then
bumps the versions accordingly and afterwards a pr releases everything. same as for the npm
packages." Extends hoist spec §9 (Releasing); §9.2–§9.6 stay as they are except where this says.

## 1. Intent

Today `hoist release plan|prepare` picks the next versions from conventional commits (§9.1). With
changesets, the author of a PR states the bump and the changelog line in a file, at PR time. That is
clearer than commit subjects, reviewable in the PR, and the same for crates and npm packages.

- Goal: the `.changeset/` format of @changesets/cli 2.x (reference: tag `@changesets/cli@2.31.1`),
  read and written by hoist for Cargo crates and npm packages alike. A file written by
  `changeset add` works in hoist, and a file written by hoist works in `changeset`.
- Goal: the flow stays hoist's: push to main → `prepare` opens/updates the release PR
  (`hoist/release`) with bumped manifests, lockfiles, CHANGELOGs and the changesets deleted →
  merge → `publish` tags, creates the GitHub Release, publishes to crates.io and npm.
- Goal: CI fails a PR that changes a package without a changeset (or an explicit empty one).
- Non-goals: snapshot releases; `changeset publish`/changesets/action; custom changelog modules;
  `commit: true`; a GitHub bot. Pre-release mode is designed (§8) but is a later task.
- Anti-goals: a Node runtime in Rust-only repos; two tools that both write versions; a new
  Actions job, runner or trigger (D-007).

## 2. Approaches

| | (a) native in hoist (chosen) | (b) @changesets/cli for npm + cargo adapter | (c) knope or sampo |
|-|-|-|-|
| crates | yes | adapter must fake packages; `changeset` errors on names not in the npm workspace | yes |
| launcher follows crate (§9.3) | kept | changesets bumps the launcher and platform packages on its own; conflicts | no launcher model |
| Node in Rust repos | no | yes | no |
| format | `.changeset/*.md` + `config.json`, byte-compatible | same | knope: same `.md`, config `knope.toml`; sampo: `.sampo/`, keys `cargo/<name>` |
| publish (OIDC, launcher, assets) | hoist, unchanged | hoist | would still need hoist |

(a) wins: one tool owns versions; the file format stays the changesets one. The reimplementation
is small: parse, merge, dependents, fixed/linked, changelog lines. knope-dev's `changesets` crate
was considered as a parser; not used (a new crate needs owner approval, and the format is ~40 lines).

## 3. Mode

A repo is in changesets mode when `.changeset/config.json` exists. Then `release plan|prepare`
ignore commit history for bumps (no conventional-commit parsing). Without it, §9.1 applies as today
(fallback, for repos not migrated yet). `.hoist/release.toml` pins and skips work in both modes; a
pin wins over the changesets bump. `release plan` adds item `release/source` (`ok`, "changesets"
or "commits").

## 4. Files

### 4.1 Changeset (`.changeset/<id>.md`)

Read rules (as `@changesets/read` and `@changesets/parse`):

- Entries directly in `.changeset/` that end in `.md`, do not start with `.`, and are not
  `README.md` (case-insensitive). The id is the file name without `.md`. Subdirectories are ignored
  (legacy `changes.json` folders are not supported; exit 3 if one exists).
- Content: optional leading whitespace, `---`, frontmatter, a line `---`, summary
  (regex `\s*---([^]*?)\n\s*---(\s*(?:\n|$)[^]*)`; summary trimmed, kept verbatim).
- Frontmatter: a YAML mapping `name: type`. hoist accepts the subset the writer emits plus what
  people type: one entry per line, the name double-quoted, single-quoted or bare, the type one of
  `major`, `minor`, `patch`, `none`. Anything else (nested YAML, duplicate name, unknown type) is
  exit 3 naming the file and line. No YAML crate.
- Empty frontmatter (`---\n---\n`) is an empty changeset: no release, it only satisfies the check
  (§6).
- Errors exit 3 naming the file: "missing or invalid frontmatter", "file is empty", a name that is
  no package (§4.3), a type outside the four.

Write rules (as `@changesets/write`, before prettier; prettier changes nothing in this shape):

```
---
"<name>": <type>
---

<summary>
```

One line per package in the order given, names always double-quoted, a final newline. An empty
changeset is `---\n---\n` plus `\n<summary>\n` when a summary is given.

### 4.2 `.changeset/config.json`

hoist reads the 2.31.1 key set. Unknown keys are exit 3 (hoist rule for config files).

| key | hoist |
|-|-|
| `$schema`, `commit` (only `false`), `access`, `privatePackages`, `prettier`, `snapshot`, `bumpVersionsWithWorkspaceProtocolOnly` | accepted; `access` must match `[publish] npm.access` when both are set, else exit 3; `commit: true` exit 3 |
| `changelog` | `"@changesets/cli/changelog"`, `"@changesets/changelog-git"` (same output) or `false` (no CHANGELOG writes). Anything else exit 3 |
| `baseBranch` | default for `changeset status --since` (default `"master"` when absent, as changesets) |
| `fixed`, `linked` | arrays of arrays of names or globs (`*`, `?` only); unmatched pattern, a package in two groups or in both kinds: exit 3. `versioning = "fixed"` in `hoist.toml` equals one fixed group of all packages; with non-empty `fixed`/`linked` it is exit 3 |
| `ignore` | names or globs; ignored packages never release; a changeset that mixes ignored and other packages is exit 3 |
| `updateInternalDependencies` | `patch` (default) or `minor`, §5.2 |
| `changedFilePatterns` | default `["**"]`, §6 |
| `___experimentalUnsafeOptions_WILL_CHANGE_IN_PATCH` | `onlyUpdatePeerDependentsWhenOutOfRange`, `updateInternalDependents` (`out-of-range` default, `always`) |

`hoist changeset init` writes what `changeset init` writes: `$schema`
`https://unpkg.com/@changesets/config@3.1.4/schema.json` (the config package of cli 2.31.1), `changelog`
`"@changesets/cli/changelog"`, `commit false`, `fixed []`, `linked []`, `access "restricted"` (or
the `[publish] npm.access`), `baseBranch` = the default branch, `updateInternalDependencies "patch"`,
`ignore []`; 2-space indent, final newline; plus the stock `.changeset/README.md` text.

### 4.3 Package names

A frontmatter name is a package of the release model (§9): the crate name (`[package] name`) or
the npm `name`. Launchers and platform packages are not nameable: exit 3 "`@keelstackhq/bosun`
follows `keel-bosun`; name `keel-bosun`" (same rule as pins). A crate and an npm package with the
same name: exit 3 at model read in changesets mode. A name that is a private or unpublished
workspace package (not in the model): exit 3 "not a releasable package" (my call: hoist releases
only what it publishes).

## 5. Version algorithm (as `@changesets/assemble-release-plan`)

### 5.1 Own bumps

Per package, the highest type over all changesets (`none` < `patch` < `minor` < `major`). New
version: semver increment, also below 1.0.0 (`major` on 0.4.1 → 1.0.0; changesets has no 0.x
rule; §9.1's "breaking below 1.0 bumps minor" does not apply here). Awaiting publish (R26) is
unchanged: a package awaiting publish gets no bump, and its changesets stay until it is published.

### 5.2 Dependents (loop until nothing changes)

Internal edges come from the model: npm `dependencies`, `optionalDependencies`, `peerDependencies`,
`devDependencies`; Cargo `[dependencies]`, `[build-dependencies]`, `[dev-dependencies]`
(dev ≙ npm dev) that point at another package (path or `workspace = true`).

- A dependent that is not releasing gets `patch` when the dependency's new version leaves the
  dependent's range (or always, with `updateInternalDependents = "always"`); a dev edge gives
  `none` (range rewrite only). Ranges: npm semver (`^ ~ >= = x *`, `workspace:*|^|~|<range>`);
  Cargo requirements (bare = caret, `^ ~ = >=`, Cargo's 0.x caret rules).
- npm `peerDependencies`: a `minor`/`major` dependency makes the dependent `major` (unless
  `onlyUpdatePeerDependentsWhenOutOfRange` and still in range).
- Range rewrite in a releasing dependent: when out of range, or the dependency's type ≥
  `updateInternalDependencies`. New range keeps the prefix (`^`, `~`, `>=`, exact); `workspace:*|^|~`
  are kept; Cargo writes the new version with its old operator, in the dependent's `Cargo.toml` or
  root `[workspace.dependencies]` (existing §9.2 editors).
- This replaces §9.1's "internal dependents always get a patch bump" in changesets mode.

### 5.3 Groups

- fixed: if any member releases, every member gets the group's highest type, from the group's
  highest current version.
- linked: only releasing members; each gets the highest type in the group from the highest current
  version.
- Launchers follow their crate after groups (§9.3).

## 6. `hoist changeset status` (the PR check)

`hoist changeset status [--since <rev>]` — local git only, no HTTP. Base: `--since`, else
`origin/<baseBranch>`; diff `merge-base(base, HEAD)..HEAD`.

- Changed package: a changed file (not under `.changeset/`) that matches `changedFilePatterns`
  (relative to the package dir) and lies in the package's dir and not in another package's dir
  (the root crate owns the rest). A launcher's or platform package's dir counts for its crate.
- Each changed package must be named in a changeset added in the diff (any type, `none` too),
  unless the diff adds an empty changeset. Stricter than `changeset status` (which passes on any
  changeset); my call, owner decision O2.
- Items: `changeset/<pkg>` `ok` ("brave-dogs-sing: minor") or `drift` ("changed without a
  changeset; run `hoist changeset add <pkg>:patch -m …` or `hoist changeset add --empty`").
  Exit 1 on drift, 0 otherwise, 3 on a bad file.
- The release PR is exempt: when `GITHUB_HEAD_REF` (or the current branch) is `hoist/release`, one
  item `changeset/status` `ok` "release PR".
- Also validates every changeset file and `config.json` (exit 3), so a bad file fails the PR, not
  the release.

CI (D-007): one step in the existing `ci` job, no new job or trigger. On `pull_request` the checkout
is the merge commit, so `actions/checkout` with `fetch-depth: 2` and
`hoist changeset status --since HEAD^1` is enough; skip the step on `push`.

## 7. `hoist changeset add` and `init`

- `hoist changeset add <name>:<type>… -m <summary> [--id <id>]`, or `--empty [-m …]`.
  Non-interactive (agents). `-m -` reads the summary from stdin. Id default: the current branch with
  `/` as `-` (bosun's fragment rule: one file per PR, no clashes), else a human-id style
  `adjective-noun-verb` from a built-in word list. An existing file is exit 3. Item
  `changeset/<id>` `create`. Writes nothing else.
- Agents may write the file by hand; the check validates it.
- `hoist changeset init [--from-commits]`: writes `config.json` and `README.md` (exit 3 if
  `config.json` exists). `--from-commits` also writes one changeset `hoist-migrate.md` with each
  package's pending bump and entries from §9.1 (so commits merged since the last tag are not lost
  in the switch).
- Agent docs: one line in each repo's CLAUDE.md/AGENTS.md: "A PR that changes a package adds
  `.changeset/<branch-with-dashes>.md` (`hoist changeset add`), or an empty one."

## 8. `prepare` and `publish` changes

- `prepare` (§9.2 step 3) in changesets mode also deletes each consumed changeset (every changeset
  whose packages all release in this PR; empty ones too) in the release commit. The PR body lists
  the changesets per package.
- CHANGELOG entries, changesets layout (changelog-git lines) with hoist's heading:

  ```
  ## 1.2.0 (2026-11-10)

  ### Minor Changes

  - abc1234: summary first line
    further summary lines, indented two spaces

  ### Patch Changes

  - Updated dependencies [def5678]
    - keel-core@0.3.1
  ```

  Sections `Major Changes`, `Minor Changes`, `Patch Changes`, empty ones omitted; `none` gives no
  line; sha = the commit that added the file (`git log --diff-filter=A -1 --format=%h -- <file>`),
  none → `- <summary>`. Existing `## Unreleased` entries move under the new version as today
  (migration bridge for bosun's folded fragments). The heading keeps ` (<date>)` (owner decision
  O1); `publish` reads `## <version> (` and, new, a bare `## <version>` line, so changelogs
  written by `changeset version` also work.
- `publish`: unchanged. Tags keep `[release] tag_format` (default `{name}@{version}`, single package
  `v{version}`, the same as changesets).
- Pre-release mode (later task, O5): `.changeset/pre.json` as changesets (`mode`, `tag`,
  `initialVersions`, `changesets`), versions `x.y.z-<tag>.<n>`, changesets kept until `pre exit`;
  needs pre-release support in `src/release/semver.rs`, npm `--tag <tag>`, GitHub Release
  `prerelease: true`. `hoist changeset pre enter|exit`.

## 9. Other repos

- bosun: `changelog.d/` is replaced by `.changeset/` (a changeset summary is the changelog line);
  `docs/progress.d/` and its fold stay. Code: `src/core/risk.rs` `AUTONOMOUS_ALLOWED`,
  `src/core/guard.rs` edit lock, `src/core/touch.rs` `FRAGMENTS` gain `.changeset/*.md`;
  `channel/pieces/core/files/.bosun/fold-fragments.sh` and `scripts/fold-fragments.sh` drop the
  changelog part; the channel's agents section and AGENTS/CLAUDE.md say "changeset" instead of
  "changelog.d fragment". `docs/releasing.md` step 0 goes; `tests/release_config.rs` adds: config
  exists, parses, `baseBranch = "main"`, no changeset names an unknown package. Migration: fold
  the last `changelog.d/` fragments into `## Unreleased` once (§8 bridge), then delete the dir.
- hull (renders `release.yml`/seeds) and the bosun channel: seed `.changeset/` and the CI step for
  new repos.
- berth, splice, hoard, hoist: one adoption PR each (`init --from-commits`, CI step, agent line).
- D-016: release paths are gated: ≤ 400 changed lines per PR (tests excluded), one task each.

## 10. Calls I made (check while reading)

Strict YAML subset, no crate; launcher names refused; private packages refused; 0.x follows
changesets; changesets dependents rule replaces always-patch; mode by file presence (no new
`hoist.toml` key); branch-named ids; `--from-commits` migration; release branch stays
`hoist/release` and title `release: …` (not "Version Packages").

## 11. Open owner decisions

- O1 Changelog heading: keep `## <v> (<date>)` (recommended, hoist's existing files) or bare
  `## <v>` as changesets writes.
- O2 Check strictness: every changed package named (recommended) or changesets' "any changeset".
- O3 `changedFilePatterns` default per repo: `**` (docs/tests-only PRs need an empty changeset) or
  source paths only (e.g. `src/**`, `Cargo.toml`, `build.rs`, `npm/**`) (recommended).
- O4 Keep the commit-history fallback after every repo migrated, or remove it (recommended:
  remove one release after the last repo moves).
- O5 Pre-release mode now or later (recommended: later, no repo needs it yet).
