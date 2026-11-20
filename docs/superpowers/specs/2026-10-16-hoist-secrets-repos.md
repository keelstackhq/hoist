# hoist design, §15: secrets repos

Split from [the main spec](2026-10-16-hoist-design.md); section numbers (§15.x) are unchanged.

## 15. Secrets repos

Copied on 2026-10-27 from keelstackhq/hull
`docs/superpowers/specs/2026-10-27-secrets-and-ci-providers-design.md` §3 to §5 (the "design").
The design stays the source for the parts outside hoist (hull templates, bosun, CircleCI). This
section is the source for hoist's behaviour. `design §N` points at a section of the design that was
not copied. Plan: `docs/superpowers/plans/2026-10-27-m5-secrets-repos.md`.

### 15.1 Layout (rendered from `template-secrets/`)

```
README.md              what is here, who may decrypt what, how to rotate (seed)
access.toml            recipients and rules (shared, toml merge) — the only file humans edit for access
.sops.yaml             generated from access.toml by `hoist secrets sync` (owned by hoist, CI-checked)
CODEOWNERS             * @<owner> (owned)
<env>/<bundle>.sops.yaml   encrypted bundles, e.g. prod/cloudflare.sops.yaml, preview/cloudflare.sops.yaml
.github/workflows/check.yml   `hoist secrets check` on pull_request + weekly expiry check (owned)
.circleci/…            only with the `circleci` piece (design §7)
```

Each secrets repo is private, has a ruleset (PR required, CODEOWNERS review, no force-push,
`check` required) and contains no plaintext ever. Suggested repos: `keelstackhq/secrets-shared`
(Cloudflare account tokens, shared SaaS) and one `keelstackhq/secrets-<project>` per project with
its own blast radius. A consumer may use several.

### 15.2 `access.toml`

```toml
version = 1

[[recipient]]
name = "owner-2026"            # a human key; the owner keeps the private key offline
kind = "human"
age = "age1…"
expires = "2027-10-27"

[[recipient]]
name = "keelstack-prod"        # one key per (consumer repo, environment)
kind = "ci"
repo = "keelstackhq/keelstack"
env = "prod"
provider = "github"            # github | circleci: where hoist stored the private key
age = "age1…"
expires = "2027-10-27"

[[rule]]
path = "prod/**"
recipients = ["owner-2026", "keelstack-prod"]

[[rule]]
path = "preview/**"
recipients = ["owner-2026", "keelstack-preview"]
```

Rules: names `[a-z0-9-]+`, unique; every rule has ≥ 1 `human` recipient (recovery); a `ci`
recipient appears only in rules whose path starts with its `env/`; globs are evaluated first match
wins, in file order; `expires` ≤ 13 months from `added`. `.sops.yaml` gets one `creation_rules`
entry per rule (path regex derived from the glob, `age` = the recipients' public keys, comma-joined,
in `access.toml` order) so the output is deterministic.

### 15.3 Consumer manifest `secrets.toml` (consumer repo root, seed)

```toml
version = 1

[sources]
shared = { repo = "keelstackhq/secrets-shared" }                 # ref defaults to "main"
site   = { repo = "keelstackhq/secrets-keelstack", ref = "main" }

[env.preview]
CLOUDFLARE_API_TOKEN = "shared:preview/cloudflare.sops.yaml#CLOUDFLARE_API_TOKEN"

[env.prod]
CLOUDFLARE_API_TOKEN = "shared:prod/cloudflare.sops.yaml#CLOUDFLARE_API_TOKEN"
SENTRY_AUTH_TOKEN    = "site:prod/sentry.sops.yaml#token"

[env.prod.files]                                                   # whole value to a file; the
GCP_CREDENTIALS_FILE = "site:prod/gcp.sops.yaml#service_account"   # env var holds its path
```

- Left side: the env var name in the child process (`[A-Z_][A-Z0-9_]*`).
- Right side: `<source>:<path>#<key>`. `<path>` must match `^[a-z0-9-]+/[a-z0-9._-]+\.sops\.yaml$`
  and start with the env name the consumer was granted (narrowed in §15.9).
- The environment name (`preview`, `prod`, `release`, …) equals the GitHub environment name and the
  CircleCI context suffix (§15.8.3), so one name links manifest, key and protection.

### 15.4 Commands (hoist; who may run them)

| Command | What | Runs | Decrypts? |
|---|---|---|---|
| `hoist secrets check` | in a secrets repo: `access.toml` valid, `.sops.yaml` == generated, every `*.sops.yaml` encrypted (sops metadata + MAC present, no plaintext value except `*_unencrypted` keys), each file's `sops.age[].recipient` set == its rule, ≥ 1 human, one recipient per `age` key, expiry warnings (30 days). In a consumer repo: `secrets.toml` valid; with `--remote`, every referenced file and key exists (key names are plaintext) | agents, CI | no |
| `hoist secrets sync [--rotate]` | regenerate `.sops.yaml`, then `sops updatekeys -y` on every file whose recipients changed; `--rotate` also `sops rotate -i` on every file. Any path it cannot open or enter (a symlink, an odd name, an unreadable directory) is drift, as in `check`. A file that lost a recipient or got a new data key is a human step `rotate:<file>` (rotate the values it holds; exit 2) | owner | data key only (needs an owner age key) |
| `hoist secrets grant <repo> --env E [--name N] [--replace \| --public age1…] [--provider github\|circleci]` | refuses unless the consumer environment exists and only branch `main` (for `release`: tags `*-v*`, with a required reviewer) may deploy to it: every custom deployment rule must be exactly that, and "protected branches" mode is refused, since without a classic protection rule it lets any branch deploy (§15.8.2; grant does not create it). `age-keygen` in memory; private key straight to the consumer's GitHub environment secret `SOPS_AGE_KEY` (sealed box, existing code) or CircleCI context `<repo>-<env>` env var. An existing `SOPS_AGE_KEY` is refused unless `--replace`, which adds a human step: re-grant this consumer in every other secrets repo whose `access.toml` names it. `--public age1…` adds the consumer's existing key instead (the environment must already hold `SOPS_AGE_KEY`) and writes no secret. The public key becomes `ci` recipient `N` (default `<repo name>-<env>`) of every rule under `E/` in `access.toml`; refused when that consumer, name or key is already a recipient. Then `sync`, then it prints the exact git commands (branch, commit, push) and the PR to open; the owner commits (hoist makes no git or PR writes). **One key per consumer environment**, shared by every secrets repo it reads (`exec` has one `SOPS_AGE_KEY`). Private key is zeroized, never printed or written | owner | data key only |
| `hoist secrets revoke <recipient> [--delete-secret]` | for a **suspected exposure** (or a consumer that leaves): remove from `access.toml`, then `sync --rotate` (new data keys on the affected files), then print every key the recipient could read: **those values must be rotated** (git history keeps old ciphertext readable by the revoked key). A `ci` key is shared by every secrets repo that lists the consumer, so its CI secret stays; a human step says to delete it once no secrets repo lists it. `--delete-secret` deletes it only when no other secrets repo may list it: refused while another recipient here has the same consumer (e.g. mid-rotation) or when the consumer's `secrets.toml` (default branch) reads a secrets repo other than this one (from `origin`); when hoist cannot tell (no `origin`, the file unreadable), it stops with a human step and changes nothing. A list of values to rotate also names every file the key's rule or text points at that hoist cannot read ("assume every value") | owner | yes (owner) |
| `hoist secrets rotate-key <recipient> [--public age1…]` | **routine** rotation of a `ci` key (yearly via `expires`; for a suspected exposure use `revoke`): a new key `<name>-YYYYMM` (a trailing `-YYYYMM` is replaced; twice in one month is refused) joins exactly the old recipient's rules, sealed over the consumer's `SOPS_AGE_KEY` (branch rules as in `grant`); the old recipient leaves and `sync --rotate` gives every file a new data key. It lists no values to rotate, not even sync's per-file steps (the old key was never exposed). Before any write it checks that the owner's key opens every file of the old key's rules (they are re-keyed after the PUT; sops runs with a deadline in its own process group) that no path under its env is one sync cannot open or enter, and that every file already has its rule's recipients (another pending change would lose its steps: run `sync` first). The key is shared: in every other secrets repo that lists the consumer, `rotate-key <its recipient> --public <new key>` does the same with the key already sealed (the environment must hold one) and writes no secret; until then `exec` there fails, and the first run lists that as a human step. Prints the git commands like `grant` | owner | yes (owner) |
| `hoist secrets edit <file>` | `sops edit` with the owner's key in their `$EDITOR`, on a `*.sops.yaml` under a rule, named in `[A-Za-z0-9._-]` (a new file gets its rule's recipients); refused while `.sops.yaml` or the file's recipients differ from `access.toml` (`sync` first), or when its directory is missing or leads outside the repo; sops runs without the variables that would pick other recipients (`SOPS_AGE_RECIPIENTS` …), and a written file whose recipients still differ is drift; no `--json` (the editor owns the terminal); prints the git commands like `grant` | owner | yes (owner) |
| `hoist secrets rotate [NAME] --to <src>:<path>#<key>` | existing Cloudflare rotation (mint, verify, revoke old) writes the new value into the sops file instead of a GitHub secret | owner | yes (owner) |
| `hoist secrets repo init <owner/name> [--require-check NAME]… [--dry-run]` | create the private repo if missing (`/user/repos` for the token's own account, else `/orgs/<owner>/repos`; a public or internal repo is refused: `visibility` must be `private`) and its own ruleset `hoist: secrets repo` (a PR with one approval, no force push, no deletion, the given required checks; `apply`'s `hoist: default branch` is a separate ruleset and both apply). Rulesets are read before any write (unreadable is an error, not "missing"); an existing `hoist: secrets repo` of the repo itself (`source_type: Repository`; an organization's of that name does not count) that protects less is drift: not `active`, not a `branch` target, the default branch not included (`~DEFAULT_BRANCH`, `~ALL` or its name) or excluded (anything but the plain name of another branch, since GitHub matches with fnmatch), a bypass actor, no approval, or a missing rule or check. Environments none. Without `--require-check`, a human step says to add the template CI's gate once it runs; the default becomes `ci-ok` when hull's `template-secrets` lands. Prints the `splice new gh:keelstackhq/hull//template-secrets` command. `--dry-run` reads only and exits 1 when something would be made | owner | no |
| `hoist secrets exec --env E --keys A,B [--workflow-run-path P] -- cmd…` | the CI runtime (§15.7); `secrets.toml` from `-C`; unix only (exit 3 on Windows); no `--json` | CI only | yes (CI key) |

Every decrypting command refuses to run when `CLAUDECODE` or `CLAUDE_CODE_REMOTE` is set (agent
sessions), exit 2 with "secrets: decryption is owner- or CI-only". `exec` also refuses outside CI
(§15.7.2). Agents may run `check`, author `access.toml`/`secrets.toml` changes in PRs, and render
`template-secrets`; the owner runs `sync` after merging an access change.

### 15.5 Rotation and revocation

- **Value rotation:** `hoist secrets edit` or `hoist secrets rotate`; consumers pick it up on the
  next run (design decision S5).
- **Data key rotation:** `sops rotate -i` per file; `hoist secrets sync --rotate` for all files,
  each then a human step `rotate:<file>` (exit 2): a new data key does not change old commits;
  yearly, and always after a revoke.
- **Consumer key rotation:** `rotate-key`, yearly via `expires`; the weekly `check` run opens an
  issue `secrets:expiring` 30 days before.
- **Revoking a consumer:** `revoke`, then rotate the listed values at the provider (Cloudflare etc.).
  A revoked key cannot read new ciphertext, but can read old commits; only value rotation ends that.
- **Lost owner key:** a second human recipient (a second owner key in a safe) is recommended;
  `check` warns when a rule has exactly one human recipient.

### 15.6 Optional KMS later

sops `creation_rules` can add `kms`/`gcp_kms`/`azure_keyvault` next to `age`. `access.toml` would
get `kind = "kms"` recipients; `check` treats them like `ci` recipients. Not built in v1.

### 15.7 The runtime: `hoist secrets exec`

#### 15.7.1 Steps

1. Read `secrets.toml`; select `[env.E]` and `[env.E.files]`; keep only `--keys` (required; an
   unknown key is exit 3). Nothing else is ever fetched.
2. Token: `HOIST_SECRETS_TOKEN` if set (GitHub action passes the token from
   `actions/create-github-app-token`), else mint an installation token from `SECRETS_READ_APP_ID`
   + `SECRETS_READ_APP_KEY` (RS256 JWT, 9 min), restricted to the listed source repos with
   `contents: read`. On CircleCI the second path is used. The key is PEM, PKCS#1 or PKCS#8; the
   JWT is issued a minute back for clock drift; the sources must share one owner (one
   installation, owners and names compared case-insensitively); a token granted more than
   `contents: read` (beyond GitHub's `metadata: read`) or any repo but the sources is refused and
   revoked. A key pasted with literal `\n` works; variables are trimmed and an empty one counts as
   unset; a 401 names the App's ID, key and the clock. Errors never quote the key or a token.
   `HOIST_SECRETS_TOKEN` is used as-is, unchecked: the HO-5 wrapper must scope it with
   `actions/create-github-app-token` to the source repos with `contents: read`. The installation
   lookup and the mint follow no redirect off the API host; a grant entry without a repo name
   counts as wider than asked.
3. Fetch each needed file once through the contents API (JSON, base64; files up to 1 MB, a larger
   one is "too large for the contents API") into memory, through a client that follows no redirect
   off the API host (`/\host` counts as another host). No `git clone`, nothing on disk. A minted
   token is revoked (`DELETE /installation/token`) after the last fetch and before anything else
   runs, also when a fetch failed; a failed revoke is a warning (`::warning::` on GitHub, stderr)
   and the run goes on (the token is `contents: read` on ciphertext and expires within the hour).
   No token ever reaches the command.
4. Decrypt each file with the constant `sops` (absolute `PATH` entries only, an executable file)
   as `sops decrypt --input-type yaml --output-type json /dev/stdin`: the file's `sops:` metadata
   is checked first (age only, so sops tries no PGP, KMS or Vault), the file goes on stdin, sops
   runs from `/` in its own process group with `SOPS_AGE_KEY` as its only variable and stderr
   dropped. sops is killed with its group, failing the run, past 60 s (per file) or past 6 × the file + 64 KiB
   of output (sops prints `<`, `>`, `&` as 6 bytes); what it leaves in its group is killed after it
   exits. Only the requested keys are copied out of its JSON (into zeroized memory); other values
   are skipped, never copied. A number or bool comes back as its JSON text, so a value must be a
   quoted string: sops/YAML reads `00123` as 83, `0x1F` as 31 and a long number as a float. Not
   wiped: the pipes, serde_json's escape buffer, and the copies of the age key that std's `Command`
   and the OS keep for sops.
5. File secrets (`[env.E.files]`): create `/dev/shm/hoist-<96 random bits>/` (0700), write each
   value to a new 0600 file, set the variable to its path. Without `/dev/shm` (macOS), exit 2:
   never a fall-back to disk. The directory is removed when the command exits, on any later error,
   and after a passed-on signal (step 8); only a signal hoist does not handle (SIGKILL, SIGQUIT,
   SIGUSR1, …) leaves it.
6. Run `cmd` in its own process group with stdin `/dev/null` (exec is not interactive) and the
   parent environment minus `SOPS_AGE_KEY`, `SECRETS_READ_APP_KEY`, `SECRETS_READ_APP_ID` and
   `HOIST_SECRETS_TOKEN`, plus the requested values. A key is asked once (`--keys A,A` is exit 3);
   two variables may name one key. A secret may not set a variable that changes how the command,
   its shell or runtime behaves, or what CI and git read (exit 3): `PATH`, `HOME`, `CI`, `IFS`,
   `ENV`, `BASH_ENV`, `SHELL`, `SHELLOPTS`, `BASHOPTS`, `PS4`, `PROMPT_COMMAND`, `CDPATH`, `ZDOTDIR`,
   `TMPDIR`, `NODE_OPTIONS`, `NODE_PATH`, `NODE_EXTRA_CA_CERTS`, `RUBYOPT`, `RUBYLIB`, `PERLLIB`,
   `JAVA_TOOL_OPTIONS`, `_JAVA_OPTIONS`, `SSL_CERT_FILE`, `SSL_CERT_DIR`, `HTTP_PROXY`,
   `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`, the four key variables, and anything starting `LD_`,
   `DYLD_`, `GITHUB_`, `RUNNER_`, `ACTIONS_`, `CIRCLE_`, `GIT_`, `PYTHON`, `PERL5`, `NPM_CONFIG_`
   or `HOIST_`. The keys never sit in hoist's environment while the command runs: at start hoist
   reads them into zeroized memory, hands them through a pipe (at most 8 KiB) to a fresh image of
   itself (`execve` of `/proc/self/exe` on Linux, of its own path elsewhere; same process) started without them, and on Linux each
   image first sets `PR_SET_DUMPABLE` to 0, so a same-user process cannot read hoist's memory or
   `/proc/<pid>/environ`. The marker of that restart (`HOIST_SECRETS_EXEC_STAGE`) set by hand next to
   a key is exit 3. **Limit:** root can still read hoist's memory, and GitHub-hosted runners give
   the `runner` user passwordless `sudo`: code the command runs can read the keys while it runs.
   The binding control stays the environment's branch rule (§15.8.2).
7. Redact: child stdout/stderr are read line-buffered and every occurrence of each value, its
   base64 and its URL-encoded form (values ≥ 4 bytes) is replaced by `***` before it reaches the
   log. On GitHub, hoist also prints `::add-mask::` for each value (and each line of multi-line
   values; ≥ 4 bytes, UTF-8) before starting the child. Redaction also covers any URL-encoding
   spelling (hex case, `+`) and JSON escapes (e.g. `\"`, `\/`, `\n`, `\uXXXX`). A line whose
   matching work runs ahead of 4096 + 64 steps per byte scanned (credit saved on cheap stretches is
   capped at 64 KiB worth), or that is over 1 MiB, fails closed: all of it but its line break
   becomes `***`. The reader must pass whole lines; values sharing a prefix are tried once. Out
   of scope: layered encodings (double URL, URL of JSON), a value split across output lines, hex,
   HTML entities and shell quoting. Base64 is caught standard and URL-safe, inside a longer
   encoding at any byte offset, and wrapped (standard alphabet) at widths that are a multiple of 4
   and ≥ 16; up to 3 bytes on each side of a wrapped line break stay visible, so a 4–6 byte value
   split there can show in full. The base64 of each value line's first 4–12 bytes is matched too,
   so unrelated base64 sharing a common start (indentation, `export `) can be partly `***`: a
   kubectl- or Kubernetes-style base64 dump of indented YAML may show `***` runs. Output is cut into
   lines at `\n` and at `\r` (a progress bar shows at once), each line redacted, written and
   flushed; output with no break is held up to 1 MiB, past that it shows as `***`. `\r` is a line
   break for the split rule too: a value split by it can show. The read buffer and the pipes are
   not zeroized. On GitHub the `::add-mask::` lines print each value once to stdout before the
   command starts, so **never redirect or `tee` hoist's stdout to a file or artifact**: the
   runner's log is the only place it may go.
8. SIGTERM, SIGINT and SIGHUP are passed on to the command's process group; a command still
   running 5 s later is killed with its group. hoist polls until the command exits (a command that
   closed its output still gets signals), then waits at most 5 s more for output from what it left
   running, removes the shm dir, zeroizes, and returns the command's exit code (128 + the signal if
   it was killed). hoist's own runtime errors and warnings start with `hoist: ` on stderr (clap's
   usage errors do not), so its exit 2 (refused) or 3 (error) can be told from the command's own; a
   stream hoist cannot write (a dead stdout) kills the command's group and is exit 3.

#### 15.7.2 Guards (exit 2, before any fetch)

- `CI` must be `true` and exactly one of `GITHUB_ACTIONS`/`CIRCLECI` set.
- GitHub: `GITHUB_EVENT_NAME` must be one of `push`, `workflow_dispatch`, `workflow_run`,
  `schedule`, `release`; everything else (`pull_request*`, `merge_group`, `issue_comment`,
  `pull_request_review*`, `status`) is refused. `GITHUB_REF` must be the default branch or a release
  tag (`workflow_run` runs with the default-branch ref, so `preview-deploy` passes; a workflow pushed
  on an agent's branch does not). `workflow_run`: the triggering run's head repository must be this
  repository (no fork artifacts), its event `push` or `pull_request`, and its `workflow_run.path`
  equal to `--workflow-run-path <.github/workflows/x.yml>`, which a `workflow_run` requires (the
  wrappers pass their own trigger file). It pins the file name, not its content: a same-repo PR can
  edit that file, which fits previews that deploy PR code anyway.
- Accepted release tags ⊆ the owner's `*-v*` tag ruleset: hoist accepts only
  `^[a-z0-9][a-z0-9._-]*-v<SemVer 2.0>$` (hull spec §4.2, hull PR #35): a lowercase name, no `/` or `@`
  (GitHub's `*` never matches `/`): `v1.2.0`, `cli@1.2.0`, `@scope/cli-v1.2.0`, `cli-v1.2` are refused.
- CircleCI: refused when `CIRCLE_PR_REPONAME` or `CIRCLE_PR_USERNAME` is set (a fork), or when
  `CIRCLE_BRANCH` is not the default branch; every tag is refused (releases are GitHub-only,
  §15.7.4). No override flag.
- **Binding rule for the wrappers:** `secrets exec` runs before any untrusted code in the same job,
  because an earlier step can rewrite the event file (`GITHUB_EVENT_PATH`) or `GITHUB_REF`. The
  wrappers start it as `exec hoist secrets exec …`, so no shell above hoist keeps the keys in its
  environment while the command runs, and they never redirect or `tee` its stdout (step 7).
- The guards are defence in depth; the binding controls are the environment branch rules (§15.8.2) and
  context expression restrictions (§15.8.3), because a branch-pushed workflow can skip hoist and read
  `SOPS_AGE_KEY` directly if its environment allows that branch.
- Agent session variables (§15.4).

#### 15.7.3 Predefined CI wrappers

**GitHub composite action** `keelstackhq/hoist/secrets` (in the hoist repo, `secrets/action.yml`):

```yaml
- uses: keelstackhq/hoist/secrets@<sha>   # v0.x.y
  with:
    env: prod                         # also the job's `environment:`
    keys: CLOUDFLARE_API_TOKEN,SENTRY_AUTH_TOKEN
    run: pnpm turbo run deploy --filter=./apps/*
    app-id: ${{ vars.SECRETS_READ_APP_ID }}
    app-key: ${{ secrets.SECRETS_READ_APP_KEY }}
    age-key: ${{ secrets.SOPS_AGE_KEY }}
    workflow-run-path: .github/workflows/preview.yml   # only in a workflow_run job
```

Consumers pin it by full commit SHA of a release tag with the tag in a comment (`@<sha> # v0.x.y`;
Renovate bumps it); a hull test fails on a tag-only reference in a decrypting job. Linux x64 and
arm64 runners only. It installs sops 3.13.3 (SHA-256 pinned: amd64 as in hoist's CI, arm64 from
`sops-v3.13.3.checksums.txt`) and the
hoist release named by the `version` in the action's own `Cargo.toml`, from the public npm
platform package `@keelstackhq/hoist-linux-<arch>` (anonymous: hoist's repository and its release
assets are private). That trusts npm and the `@keelstackhq` publishers (no provenance while the
repo is private); the sha512 check against `dist.integrity` only guards the download, and npm
never reuses a version number. The binary's checksum cannot be written into the action itself:
the action is committed before the release builds it. Then it runs `hoist secrets exec --env --keys [--workflow-run-path] --
bash -eo pipefail -c "<run>"` with the inputs in `env:`, never interpolated into `run:` text, and
hoist mints its own token from `SECRETS_READ_APP_ID` and `SECRETS_READ_APP_KEY` (step 2 of
§15.7.1: `contents: read` on the sources only, revoked after the fetch; the step sets
`HOIST_SECRETS_TOKEN` empty, so a job-level one is never used), so no other action sees the key
or a token. While hoist is private, only repos owned by `keelstackhq` can use the action, and only
with hoist's Settings → Actions → Access set to "Accessible from repositories in the keelstackhq
organization" (an owner step); other owners need hoist, or the action, public. A `workflow_run` job (hull's `preview-deploy`) passes `workflow-run-path`.

**Reusable workflow** `keelstackhq/hoist/.github/workflows/secrets-run.yml@<sha>` for job-level use:
inputs `environment`, `keys`, `run`, `ref` (default `github.sha`), `node` (default true:
pnpm/node setup + install); `secrets: inherit`. It sets `environment: ${{ inputs.environment }}`
so environment protection applies to the caller's repo.

**CircleCI command** `secrets-exec` (reference copy `hoist/secrets/circleci/secrets-exec.yml`;
hull's `secrets` piece ships a byte-identical `.circleci/src/commands/secrets-exec.yml` when
`circleci` is present):
parameters `env`, `keys`, `run`; installs pinned hoist + sops with SHA-256 check; reads
`SOPS_AGE_KEY`, `SECRETS_READ_APP_ID`, `SECRETS_READ_APP_KEY` from contexts (§15.8.3).

#### 15.7.4 Which jobs may decrypt

| Job | GitHub | CircleCI |
|---|---|---|
| `ci-ok` / `bosun-ok` (PR path) | never | never |
| `preview` build | never | never |
| `preview-deploy` | `workflow_run` of `preview`, environment `preview`, same-repo head only, deploys the artifact | not offered: CircleCI cannot run a protected (default-branch) definition for a PR, so previews (build and deploy) stay on GitHub in every mode — design §8 |
| `deploy` (prod) | `push` to main, environment `prod` (required reviewer optional, deployment branch `main`) | workflow on `main` only, `type: approval` hold, context `<repo>-prod` with expression restriction `pipeline.git.branch == "main"` |
| `release` | tag push, environment `release` (owner approves, D-008) | GitHub only (design §8) |
| `desktop`, Storybook, nightly | never (no secrets needed) | never |

### 15.8 Security model

#### 15.8.1 Threats and controls

| Threat | Control |
|---|---|
| An agent reads plaintext | agents never get `SOPS_AGE_KEY` (owner never sets it in Claude environments); decrypting hoist commands refuse in agent sessions; bosun hook denies `sops decrypt/-d/edit/exec-*`, `age -d`, `hoist secrets edit|revoke|rotate*|sync`, reads of `keys.txt`, and `printenv`/`echo` of `SOPS_AGE_KEY` (BO-1) |
| A PR changes a workflow to dump secrets | design decision S12: no decrypting job runs PR-controlled workflow code or PR code; `pull_request` jobs have no environment with `SOPS_AGE_KEY` |
| A PR changes the deploy workflow, merged unseen | bosun `R-secrets` and the critical floor make changes to `secrets.toml`, `.sops.yaml`, `access.toml`, `**/*.sops.yaml`, `.github/workflows/**`, `.github/actions/**`, `.circleci/**` owner-approved |
| Fork PR gets secrets | GitHub: no secrets on fork `pull_request`; `preview-deploy` checks head repo. CircleCI: "Pass secrets to builds from forked pull requests" off; contexts project- and expression-restricted |
| `pull_request_target`/`issue_comment` jobs (risk, verdict, main-open) leak | they never reference a secrets environment; `exec` refuses those events; hull test: no template workflow with those triggers has `environment:` or `SOPS_AGE_KEY` |
| PR code runs inside `preview-deploy` | the artifact holds only pre-bundled output (Worker bundle, static assets); `wrangler.jsonc`, `package.json` and the lockfile come from the default-branch checkout; deploy uses `wrangler deploy --no-bundle` / `wrangler pages deploy <dir>` with no build step; the job rejects an artifact containing `wrangler.json*`, `package.json`, `*.toml` or symlinks (hull test) |
| A malicious preview artifact at runtime | it runs only on Cloudflare preview Workers/Pages with preview-only bindings; the preview token is a separate Cloudflare token from prod; prod deploy builds from `main` |
| Values in logs | §15.7.1 step 7 redaction + GitHub masks; hoist never prints values, even with `-v`; tests feed a fixture value through every error path |
| Plaintext on disk | none in the workspace; file secrets only on `/dev/shm`, removed on exit |
| Stolen App key | ciphertext only (design decision S7); rotate the App key; secrets repos stay private |
| Stolen consumer age key | `revoke` + value rotation (§15.5); per-(repo, env) keys bound the damage |
| Forged `ci-ok` status in circleci-only mode | the mirror (design §7.4) trusts only terminal statuses created by the CircleCI app; the ruleset pins `ci-ok`'s expected source to GitHub Actions. That does not stop a same-repo PR from adding a workflow with `statuses: write` that posts `ci-ok` (the same hole as editing `ci.yml` in github mode): the binding control is the required `risk-check`, which makes any `.github/workflows/**` or `.circleci/**` change owner-approved (R-ci, critical floor) |

#### 15.8.2 GitHub setup per consumer

Environments `preview`, `prod`, `release` with secret `SOPS_AGE_KEY` each (written by
`hoist secrets grant`). **Every** secrets environment has deployment-branch rules, otherwise any
workflow pushed on any branch (by an agent, no PR needed) could name the environment and print the
key: `preview` and `prod`: branch `main` only (`preview-deploy` runs via `workflow_run` with the
default-branch ref, so it qualifies); `release`: tags `*-v*`, owner reviewer (D-008). The owner pushes release tags and approves
the run, so "Prevent self-review" stays off. Residual risk: whoever holds the owner's credentials can
tag and self-approve; the tag ruleset limits who creates `*-v*` tags, and agents never get the
owner's token or a PAT. The environment's "Allow administrators to bypass" (`can_admins_bypass`,
on by default) also lets an admin deploy without the reviewer; grant does not refuse it, and owners may
turn it off. `hoist secrets
grant` refuses to store a key in an environment that is missing or allows any other ref; the owner
creates it with these rules first (`check --remote` flags drift). The check and the write are separate
requests: the owner must not loosen the rules while granting. Whether the environment already holds
`SOPS_AGE_KEY` is read before the PUT, also a separate request; that race is accepted (only the owner
grants). If the PUT succeeds but `access.toml` cannot be written, grant reports drift and prints
`grant … --public <key>` to finish; after a crash between the two, `grant --replace` recovers. Org variable `SECRETS_READ_APP_ID`, org secret `SECRETS_READ_APP_KEY` restricted to the
consumer repos.

#### 15.8.3 CircleCI setup per consumer

Org context `secrets-read` (`SECRETS_READ_APP_ID`, `SECRETS_READ_APP_KEY`), project restriction
to the consumer projects. Per environment a context `<repo>-<env>` (e.g. `keelstack-prod`) holding
`SOPS_AGE_KEY`, with a project restriction and an expression restriction
`pipeline.git.branch == "main"` on **every** env context (a release context may allow the release
tag pattern instead). Security-group restrictions exist only for OAuth-connected orgs;
GitHub-App orgs use project and expression restrictions (CircleCI docs, 2026). Project settings:
build forked PRs on, pass secrets to forks **off**.

### 15.9 hoist additions to the design

- **`added` is optional** in `access.toml` and never in the future. `expires` must be at most 13
  months after `added` (the day clamped to the month end: 2026-01-31 → 2027-02-28), or after
  today without `added`, and not before `added`. An expired recipient parses and is drift (exit
  1) in `check`; one that expires within 30 days is an `ok` item with a warning. `env` matches
  `[a-z0-9][a-z0-9-]*`, `repo` is `owner/name`; `age` is checked for charset and length (`age1` +
  58 bech32 characters), not the bech32 checksum. There is at least one `[[rule]]`; rule paths
  are unique by exact string match (`prod/**` and `prod/*/**` are different rules).
- **Rule globs:** `<env>/<glob>` with a literal first segment (no wildcard), characters
  `[a-z0-9._*?/-]`, no empty, `.` or `..` segment; `**` matches any characters including `/`, `*`
  and `?` stay within one segment; the first match wins.
  `rule_for` matches no rule for a file path with a leading `/`, such a segment, or a `\` or `:`.
  `.sops.yaml` lists each rule's keys in that rule's `recipients` order. Only X25519 `age1…`
  recipients are accepted in v1; post-quantum (`age1pq1…`) and plugin recipients are not.
- **What `secrets check` can see.** It reads sops metadata without the data key. It checks each
  file's recipient labels (`sops.age[].recipient`, each entry exactly `recipient` + `enc`), not
  that each `enc` really wraps the data key to that recipient. It catches accidental plaintext
  (unencrypted values, plaintext comments), not a disguised one (a well-formed fake `ENC[…]` or
  `#ENC[` line): only decrypting verifies the MAC. Top-level keys must match `[A-Za-z0-9_.-]+`.
- **`secrets check` in a secrets repo** reads (never decrypts, sends no request) and reports
  `sops-config` (`.sops.yaml` equals the generated one), `recipient` (expired: drift; 30 days or
  less: warning), `rule` (warns on one human or an unreachable rule) and `file` for every path
  (`/`-joined; only the top `.git` skipped). Drift: a symlink (never followed), a name outside
  `[A-Za-z0-9._-]` (shown with `?`), a non-regular file, a nested sops config, a `*.sops.yaml`
  outside every rule, another file under a rule, a parse error, plaintext, a repeated or wrong
  recipient set. Outside every rule only the repo's own files may exist, unscanned (README.md,
  access.toml, .sops.yaml, CODEOWNERS, .gitignore, LICENSE, `.github/**`, `.circleci/**`);
  anything else there is drift. Exit 0/1; 3 for an invalid `access.toml`.
- **`secrets check` in a consumer repo** (no `access.toml`, a `secrets.toml`): one `secret` item
  per `<env>.<VAR>`. `--remote` reads each referenced file once (GET `contents/{path}?ref={ref}`).
  Drift: a 404 (missing file or ref, or a repo the token cannot see), a missing key, a directory or
  submodule, a file over 1 MB, a file that is not clean sops. Exit 3: a 403, no token, an API error
  (needs Contents: read), or `--remote` in a secrets repo.
  A top-level `.git` file or directory is skipped; a name with any character outside
  `[A-Za-z0-9._-]` (a literal `?` too) is drift.
- **`secrets check` in a repo with neither file** is not an error: exit 0 with one item, `secrets`
  `config` (action `ok`, reason "no secrets config; nothing to check", source `default`), so a CI
  step can run it everywhere.
- **`secrets exec` guards (§15.7.2):** the GitHub default branch comes from the event payload
  (`repository.default_branch`, non-empty; a `schedule` payload has none and uses its own branch
  ref; else refused). Payload and CI-variable text in a refusal is quoted with escapes, so it
  cannot inject a workflow command. A release tag (§15.7.2) counts only for `push` and `release`
  events, so a dispatch, schedule or workflow_run on a tag is refused; this assumes a tag ruleset
  where only the owner creates `*-v*` tags (D-008). `workflow_run` compares repository ids. CircleCI
  decrypts on `main` only, never for a tag (no default-branch variable; matches §15.8.3; releases
  are GitHub-only, §15.7.4). An agent session is refused first. All refusals exit 2.
- **Owner commands** call sops with `--config <repo>/.sops.yaml` and absolute file paths, run on
  POSIX only (sops on Windows would match `prod\a`), and refuse a symlinked file.
- **Errors never quote values.** `secrets.toml`/`access.toml` errors name file, line and key, never
  the value, so a pasted token is not echoed; toml's messages lose the source line and strings.
- **`secrets.toml` names:** source and env `[a-z0-9][a-z0-9-]*`. In
  `<source>:<env>/<bundle>.sops.yaml#<key>` the bundle is `[a-z0-9_-][a-z0-9._-]*` (no leading
  `.`) and the key `[A-Za-z0-9_.-]+`.

#### 15.9.1 Pinned external tools

hoist runs `sops` and `age-keygen`; it never reimplements them (design decision S10). Every place
that installs them (hoist CI, `secrets/install.sh`, the action) checks these SHA-256 values.
Checked on 2026-10-27:

| Tool | Asset | SHA-256 | Source |
|---|---|---|---|
| sops 3.13.3 | `sops-v3.13.3.linux.amd64` | `e5bec3346a873ae91d871550f3e698c1aad962aff462a080e40f25fde17fef6b` | the release asset `sops-v3.13.3.checksums.txt` (github.com/getsops/sops); the downloaded binary matched |
| sops 3.13.3 | `sops-v3.13.3.linux.arm64` | `53b0abacd38ef1b12a66d6c100956691b9cefce018d91f81e73ddf7438b94d77` | the same checksums file (binary not downloaded) |
| age 1.3.2 | `age-v1.3.2-linux-amd64.tar.gz` | `cbe24006683f8eb669266162894b9a522a1af52f2665fbc63a4bb032ed26ac10` | see below |
| age 1.3.2 | `age-v1.3.2-linux-arm64.tar.gz` | `6b8dc4333c53a5a57c9e5834e3a48f92605d7154014cd07269ff3327db5d37f4` | see below |

age publishes no checksum file. It publishes a Sigsum proof (`<asset>.proof`) next to each
release asset (`SIGSUM.md` in the age repo at v1.3.2). What was verified for the two age
tarballs:

1. The SHA-256 above was computed from the official GitHub release asset.
2. The proof's leaf signature verifies. It is the ed25519 signature over
   `"sigsum.org/v1/tree-leaf\0" || SHA-256(SHA-256(file))`, made with one of the two age Sigsum
   keys copied into the script from `SIGSUM.md` at v1.3.2 (`…xXLebukZEZFS`). A short local Python
   script checked it with the `cryptography` library; it reads only the proof's `leaf=` line.
3. Log inclusion and the witness cosignatures (`sigsum-verify -k age-sigsum-key.pub -P
   sigsum-generic-2025-1`, `sigsum-verify@v0.13.1`): not run in the author's session (blocked);
   the orchestrator's reviewer reported exit 0 for both tarballs on 2026-10-27 (cross-session
   report, not reproduced here). hoist CI (`ci.yml`) runs it on every run and fails when it
   fails. The sops checksums file's own signature (cosign) was not checked.
