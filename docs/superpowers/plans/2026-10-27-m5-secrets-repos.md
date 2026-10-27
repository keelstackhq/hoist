# hoist — secrets repos (sops + age) and the CI runtime

> Copied on 2026-10-27 from keelstackhq/hull `docs/superpowers/plans/2026-10-27-secrets-ci-hoist.md`.
> "S§" below means the design in hull; its §3 to §5 are hoist spec §15.

## Split of HO-1 (orchestrator-approved 2026-10-27)

HO-1 is about 1,200 changed lines, so it lands as PRs of 400 lines or fewer, in this order:

- [ ] HO-1a `feat(secrets): secrets.toml model`: `src/secrets/{mod,manifest}.rs`,
  `tests/secrets_manifest.rs` (#13)
- [ ] HO-1b0 `feat(timeutil): calendar dates`: `src/timeutil.rs` (#16)
- [ ] HO-1b `feat(secrets): access.toml model and .sops.yaml`: `src/secrets/access.rs`,
  `tests/secrets_access.rs` (after HO-1b0); its spec text lands in a parallel docs PR
- [ ] HO-1c `docs(secrets): spec §15 and the M5 plan`: this file and hoist spec §15 (parallel
  with HO-1a)
- [ ] HO-1d `feat(secrets): secrets check`: the rest of HO-1 below (after HO-1b)

Spec: hull `docs/superpowers/specs/2026-10-27-secrets-and-ci-providers-design.md` (S§). HO-1 copies
S§3-§5 into hoist's spec as a new §15 "Secrets repos" and this plan into
`docs/superpowers/plans/2026-10-27-m5-secrets-repos.md`.
Tech stack: no new crates planned. The App JWT is RS256 signed with `ring` 0.17.14 (already in
`Cargo.lock` via rustls; HO-2 adds it as a direct dependency at the same version — if that is not
acceptable, ask the owner before choosing `jsonwebtoken`/`rsa`). External binaries, pinned and
SHA-256 checked where hoist installs them: `sops` 3.13.3, `age-keygen` (age 1.3.2) — only for owner
commands (`grant`, `rotate-key`) and tests.
Test keys: generated at test time with `age-keygen` into `tempfile` dirs; tests that need sops/age skip
with a message when the binaries are missing locally; hoist CI installs them pinned. Never commit a
private key; a test asserts no tracked file contains `AGE-SECRET-KEY-`.

Rules from hoist's CLAUDE.md apply (exit codes, one JSON object, GET-only reads, never leak, rustls,
`forbid(unsafe_code)`). PRs do not edit `docs/PROGRESS.md` or `CHANGELOG.md`; HO-6 does.

## HO-1: models + `hoist secrets check`

Files: `src/secrets/mod.rs`, `src/secrets/manifest.rs` (`secrets.toml`, S§3.3),
`src/secrets/access.rs` (`access.toml`, S§3.2; `sops_yaml(&Access) -> String` deterministic),
`src/secrets/sopsfile.rs` (parse sops YAML metadata without decrypting: `sops.age[].recipient`,
`sops.mac`, encrypted values match `^ENC\[AES256_GCM,`), `src/ops/secrets/check.rs`, `src/cli.rs`
(`secrets check [--remote]`), `tests/secrets_{manifest,access}.rs`, `tests/cli_secrets_check.rs`,
`tests/fixtures/secrets/**` (encrypted fixtures produced by a `#[ignore]` generator test that uses
throwaway keys; the committed fixtures contain only ciphertext and **public** keys), hoist spec §15.
- Tests first: manifest parse errors (bad env var name, bad ref syntax, path outside env) exit 3 with
  the line; access rules (≥ 1 human, ci recipient only under its env, unique names, expiry);
  `sops_yaml` golden output; check in a secrets-repo fixture: ok → 0; a plaintext value → 1; a
  recipient mismatch → 1; `.sops.yaml` drift → 1; expiry in 20 days → 0 with a warning item;
  consumer mode `--remote` with the fake GitHub server: missing key → 1; GET-only asserted.
- Commit: `feat(secrets): secrets.toml + access.toml models, secrets check`

## HO-2: `hoist secrets exec`

Files: `src/secrets/token.rs` (App JWT with `ring`; installation token for named repos,
`contents: read`), `src/secrets/fetch.rs` (contents API raw, in memory), `src/secrets/decrypt.rs`
(spawn `sops decrypt --input-type yaml --output-type json /dev/stdin`, `SOPS_AGE_KEY` only in that
child env), `src/secrets/redact.rs` (line-buffered replace of value, base64, URL-encoded; ≥ 4 bytes),
`src/secrets/guard.rs` (S§4.2), `src/ops/secrets/exec.rs`, `src/cli.rs` (`secrets exec`),
`Cargo.toml` (`ring`), `tests/secrets_exec.rs`, `tests/secrets_redact.rs`.
- Tests first:
  - guards: outside CI → 2; `GITHUB_EVENT_NAME=pull_request_target` → 2; `CLAUDECODE=1` → 2; fork
    `workflow_run` → 2; CircleCI fork → 2; each before any HTTP request (fake server sees none);
  - happy path (fake GitHub + real sops + test key): child sees exactly the requested keys; parent's
    `SOPS_AGE_KEY`/`SECRETS_READ_APP_KEY`/`HOIST_SECRETS_TOKEN` absent in child; file secrets under
    `/dev/shm/hoist-*` mode 0600, dir gone after exit and after SIGTERM;
  - redaction: child prints the value, its base64 and URL forms and a multi-line value → only `***`
    in captured output; GitHub mode prints one `::add-mask::` per value/line before the child starts;
  - error paths with a fixture value (`hoist-test-value-…`) never print it (stderr scan);
  - exit code is the child's.
- Commit: `feat(secrets): secrets exec runtime`

## HO-3: owner commands (GitHub side)

Files: `src/ops/secrets/{sync,grant,revoke,rotate_key,edit,repo_init}.rs`, `src/secrets/agekey.rs`
(spawn `age-keygen`, read private key from stdout into `Zeroizing<String>`), `src/github_write.rs`
(environment secret PUT/DELETE: reuse `seal.rs`; repo create + ruleset), `src/cli.rs`,
`src/ops/secrets/rotate.rs` (`--to <src>:<path>#<key>` writes via `sops set`), `tests/cli_secrets_owner.rs`.
- Tests first: every decrypting command refuses with `CLAUDECODE`/`CLAUDE_CODE_REMOTE` (exit 2);
  `grant` against the fake server PUTs `SOPS_AGE_KEY` to `environments/<env>/secrets` sealed (fake
  server decrypts with its test key and checks it parses as an age identity), updates `access.toml`,
  regenerates `.sops.yaml`, runs `sops updatekeys -y` (fixture repo in a temp git dir), and stdout/
  stderr never contain `AGE-SECRET-KEY-`; `revoke` removes the recipient, rotates the data key
  (file MAC changes), DELETEs the secret and lists affected keys as `human_steps`; `rotate-key` =
  grant + revoke; `repo init` creates a private repo + ruleset (`--json` plan in `--dry-run`).
- Commit: `feat(secrets): sync, grant, revoke, rotate-key, edit, repo init (GitHub)`

## HO-4: CircleCI contexts

Files: `src/circleci_write.rs` (create context, set/delete env var, add project + expression
restrictions; base URL from `HOIST_CIRCLECI_API`, token from `CIRCLECI_TOKEN` env only),
`src/ops/secrets/grant.rs` + `revoke.rs` (`--provider circleci`), `tests/circleci_write.rs`,
`tests/fixtures/api/circleci/*.json`.
- Tests first: grant with `--provider circleci` creates `<repo>-<env>` if missing, sets
  `SOPS_AGE_KEY`, adds the project restriction and, for `prod`, the expression
  `pipeline.git.branch == "main"`; revoke deletes the var; the token never appears in output.
- Commit: `feat(secrets): CircleCI contexts for grant/revoke`

## HO-5: the CI wrappers

Files: `secrets/action.yml` (composite, S§4.3; inputs via `env:`; installs hoist of this release and
sops 3.13.3 with SHA-256 for linux x64/arm64; `actions/create-github-app-token` pinned by commit SHA),
`secrets/install.sh` (shared installer, also used by the CircleCI command), `secrets/circleci/
secrets-exec.yml` (reference command; hull vendors it byte-identically), `.github/workflows/
secrets-run.yml` (reusable, `workflow_call`, `environment: ${{ inputs.environment }}`), `secrets/
README.md`, `tests/secrets_action.rs` (YAML assertions: pinned SHAs, no `${{ inputs.* }}` inside
`run:`, checksum lines present, `permissions` minimal).
- The release workflow must publish the action's tag together with the binary (existing release
  flow; check `release` docs and adjust only if the action needs a moving `v0` tag — owner moves tags,
  D-008).
- Commit: `feat(secrets): GitHub action, reusable workflow, CircleCI command`

## HO-6: wrap-up

`docs/PROGRESS.md` (one line per HO task; Blocked: owner steps S§13 2-6), `CHANGELOG.md`, README
"Secrets repos" section, `CLAUDE.md` layout lines. Commit: `docs: secrets repos`.
