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

`image` defaults to `ghcr.io/<owner>/<package name>`; `platforms` to `["linux/amd64"]`.

## Split of work

- **hull** renders the `image` job from `[publish] container`: the permissions above, actions
  pinned by SHA, the tag checkout, buildx, the push and the attestation. This is a workflow change
  and needs the owner's approval.
- **hoist** never runs docker. It:
  - adds `container` to the release model (the package releases as today; the image follows the
    same version);
  - `release plan` lists the image as a pending step;
  - `release publish` gets a done-check before the job runs: `GET` the registry manifest for
    `<version>` (anonymous token for public images, `GITHUB_TOKEN` for private). Present with an
    attestation → done. Present without one → exit 1, since tags are not re-pushed;
  - verification (§9.5): the manifest for `<version>` exists for every platform, and the
    attestation for its digest exists (`GET /orgs/{o}/attestations/sha256:<digest>`, or the user
    endpoint);
  - `hoist check` reports `packages: write` missing on the `image` job as drift, as it does for
    `id-token: write` on `publish`.

## Tasks (if scheduled)

1. Spec §5/§9: the `[publish] container` schema (strict), the model, plan items, the done-check
   and verification. Tests first with the fake API: manifest 200/404, attestation present or not,
   a private image's token, multi-platform index.
2. `release publish`: the done-check and verification; reads only (the push stays in hull's job).
3. hull: render the `image` job (a separate hull PR, owner-approved).
4. README: replace the pattern section with the typed kind.

## Open questions for the owner

- Moving tags (`latest`, `<major>.<minor>`): on by default, or opt-in?
- Registries other than ghcr.io (e.g. Docker Hub with a token secret): out of scope for v1?
- Preview images on PRs: separate name and no `packages: write` on PR code, or not at all?
