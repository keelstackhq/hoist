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
  released, the tag checkout, QEMU and buildx, push only when `<version>` is missing, then the
  attestation (also on a re-run). It is a workflow change, so the owner approves it.
- **hoist** never runs docker and never checks the image inside `publish`, which runs before the
  `image` job:
  - adds `container` to the release model; the image follows the package's version;
  - `release plan` and `hoist check` (reads only, after the release) look at the registry. The
    manifest for `<version>` must exist for every configured platform; build-push-action's
    `unknown/unknown` attestation manifests are skipped. The attestation for its digest must exist
    (`GET /repos/{o}/{r}/attestations/sha256:<digest>`). Reading a private image needs a token
    with `packages: read`;
  - a missing manifest or attestation is a human step ("re-run the `image` job; it pushes only a
    missing tag and attests again"), never an exit that blocks `publish`;
  - `hoist check` reports drift when the `image` job lacks `packages: write`, `attestations:
    write` or `id-token: write`, or is not gated on the release output.

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
