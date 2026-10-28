# Proposal: `[publish] container` (not scheduled)

A proposal for the owner to schedule or drop. Until then, images follow the documented pattern
(design spec §9.7): a separate `image` job in `release.yml` with `packages: write`,
`attestations: write` and `id-token: write`, pushing `ghcr.io/<owner>/<image>:<version>` by
digest with a provenance attestation.

## Shape

```toml
[publish]
container = { image = "ghcr.io/acme/app", platforms = ["linux/amd64", "linux/arm64"],
              context = ".", dockerfile = "Dockerfile" }
```

`image` defaults to `ghcr.io/<owner>/<package name>`, lowercase, without an npm scope; the tag is
the bare version (`1.2.3`). `platforms` defaults to `["linux/amd64"]`.

## Split of work

- **hull** renders the `image` job from `[publish] container`, as in §9.7: gated on the run having
  released, one run per version (`concurrency`), the tag checkout, QEMU and buildx, a push only
  when the registry answers 404 for `<version>`, the attestation only for a digest the job built,
  and for a tag that exists a `gh attestation verify --signer-workflow` that fails without one.
  It is a workflow change, so the owner approves it.
- **hoist** never runs docker and never checks the image inside `publish`, which runs before the
  `image` job:
  - adds `container` to the release model; the image follows the package's version;
  - `release plan` and `hoist check` (reads only, after the release) look at the registry. The
    manifest for `<version>` must exist for every configured platform; build-push-action's
    `unknown/unknown` attestation manifests are skipped. The attestation for its digest must exist
    (`GET /repos/{o}/{r}/attestations/sha256:<digest>`). Reading a private image needs a token
    with `packages: read`;
  - a missing manifest is a human step ("re-run the failed `image` job in the same workflow run;
    it pushes only a missing tag"). A manifest without this repo's attestation is a human step
    too, but not a re-run: nothing signs an image after the fact, so "do not deploy
    `<image>:<version>`; release the next version". Neither is an exit that blocks `publish`;
  - `hoist check` reports drift when the `image` job lacks `packages: write`, `attestations:
    write` or `id-token: write`, or is not gated on the release output.

## Facts to verify before hull renders the job

- What GHCR answers `docker buildx imagetools inspect` with the job's `GITHUB_TOKEN`
  (`packages: write`) for a package that does not exist yet. Checked 2026-10-28: a missing tag
  of an existing package is 404 `MANIFEST_UNKNOWN` (containerd: `<ref>: not found`); a
  never-pushed package refuses an anonymous token with 403 `DENIED`. If the token's answer is
  `denied` too, the first push needs another signal (for example the Packages API's 404 for the
  package, if `GITHUB_TOKEN` may read it), never a looser text match.
- The buildx version hull pins (`setup-buildx-action`'s `version`) still prints a manifest 404 as
  the last line `ERROR: <ref>: not found`; re-check on each bump.
- That `gh attestation verify oci://… --signer-workflow` works in the job with `GITHUB_TOKEN`
  for a private repo and a private package.

## Tasks (if scheduled)

1. Spec §5/§9: the `[publish] container` schema (strict), the model, plan items and the checks.
   Tests first with the fake API: manifest 200/404, attestation present or not, a private image's
   token, a multi-platform index with `unknown/unknown` entries.
2. `release plan` / `hoist check`: the registry and attestation reads (the push stays in hull's
   job).
3. hull: render the `image` job (a separate hull PR, owner-approved).
4. README: replace the pattern section with the typed kind.

## Open questions for the owner

- Moving tags (`latest`, `<major>.<minor>`): on by default, or opt-in?
- Registries other than ghcr.io (e.g. Docker Hub with a token secret): out of scope for v1?
- Preview images on PRs: separate name and no `packages: write` on PR code, or not at all?
