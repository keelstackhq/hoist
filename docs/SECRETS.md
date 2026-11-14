# hoist secrets

`[github] secret_scope = "environment"` keeps secrets out of the repository: each secret goes to
the environments of the jobs that read it (deploy in `prod`, preview-deploy in `preview`, release
jobs in `release`), and counts as present only when all of them hold it. A job that reads a secret
without an `environment:` is drift, and a repository copy is a human step to delete it (spec
§8.6). The default, `"repo"`, is unchanged.

`hoist secrets set NAME` seals a value into the repo's Actions secret `NAME`. The value comes from
env `NAME`, else a hidden prompt on a terminal, else exit 3; there is no value flag, and
`secrets set NAME VALUE` or `secrets set NAME=VALUE` exits 3 without echoing the value. `NAME` must
match `^[A-Z_][A-Z0-9_]*$` and not start with `GITHUB_` (exit 3 before any request). It needs
admin on the repo (same preflight as `apply`). The hoist credential names `CLOUDFLARE_API_TOKEN`,
`CLOUDFLARE_ACCOUNT_ID` and `GH_TOKEN` never read their env var: they take the value only from the
prompt, and without a terminal exit 3 (for `CLOUDFLARE_API_TOKEN`, `hoist apply` mints a scoped
token instead). A failed PUT exits 1.

`hoist secrets rotate [CLOUDFLARE_API_TOKEN]` replaces the minted deploy token. Only
`CLOUDFLARE_API_TOKEN` can be rotated; another name exits 3. Before any write it runs apply's
preflights and requires hoist-managed Cloudflare with a deploy target, an account id and a
readable token list (else exit 3). Order: mint a new token, verify it (`GET /user/tokens/verify`
with the new token, up to 3 tries about 1 s apart), seal it into the secret, revoke the old
`hoist:<owner>/<repo>:deploy` tokens. After the first write failures exit 1: a failed verify or
seal revokes the new token and leaves the old secret untouched; a failed revoke names the token id
and adds a human step to revoke it in the dashboard. Report: `github/secret/CLOUDFLARE_API_TOKEN`
is `ok` ("rotated"); each revoked token is a `cloudflare/api_token` item keyed by its name with
the id in the reason.

`hoist secrets check` checks a secrets repo (`access.toml`) offline without decrypting, or a
consumer's `secrets.toml`; `--remote` also looks up each referenced file and key (GET, a token
with Contents: read). Drift exits 1. Details: spec §15.9.

`hoist secrets exec --env E --keys A,B [--workflow-run-path P] -- cmd…` is the CI runtime (Linux
and macOS runners). It refuses outside CI, in an agent session, on a pull request or fork, and off
the default branch or a release tag (exit 2, before any request). It fetches only the asked keys of
`[env.E]` in `secrets.toml`, decrypts them with `sops` and runs `cmd` with them; stdin is
`/dev/null`, and stdout and stderr are redacted (`***`; a line too costly to scan or over 1 MiB is
`***` in full). The age key and the token never reach `cmd`, and hoist keeps them out of its own
environment. Root (`sudo` on GitHub-hosted runners) can still read hoist's memory while `cmd` runs.
- File secrets (`[env.E.files]`) are files in `/dev/shm`, removed afterwards; without `/dev/shm`
  (macOS) it exits 2.
- Write secret values as quoted strings: sops/YAML turns `00123` into 83.
- On GitHub, never redirect or `tee` its stdout: the `::add-mask::` lines carry the values.
- SIGTERM/SIGINT reach `cmd`, which is killed 5 s later if it is still running. The exit code is
  `cmd`'s; hoist's own runtime errors start with `hoist: ` (exit 2 refused, 3 error; a usage
  error in the arguments is clap's own `error:`, exit 3).

On GitHub, use the action `keelstackhq/hoist/secrets`, pinned by the full SHA of a hoist release
tag (Linux runners). It installs sops and that release of hoist (from npm, which it trusts; the
sha512 check guards the download) and runs `hoist secrets exec`; inputs `env`, `keys`, `run`,
`app-id`, `app-key`, `age-key`, and `workflow-run-path` in a `workflow_run` job (spec §15.8).
While hoist is private, only `keelstackhq` repos can use it, after the owner sets hoist's Settings
→ Actions → Access to the organization.

A consumer repo whose release runs decrypt secrets must tag releases `<name>-v<version>` with a
lowercase name and no `/` or `@` (so not `@scope/x`): set `[release] tag_format =
"{name}-v{version}"`. `secrets exec` refuses any other tag, including hoist's defaults
`v{version}` and `{name}@{version}` (spec §15.7.2).

