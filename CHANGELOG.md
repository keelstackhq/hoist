# Changelog

## 0.3.0 (2026-10-28)

### Features

- keelstackhq/hoist/secrets GitHub action (#54) (335d68d)
- hoist secrets repo init (#51) (dc0398f)
- hoist secrets edit (#50) (603e9bc)
- hoist secrets rotate-key (#49) (b479444)
- hoist secrets revoke (#48) (9178671)
- hoist secrets grant (#47) (fb931d4)
- access.toml CI recipient edits for grant (#46) (7550743)
- secrets consumer guards for grant (#45) (90cf950)
- environment secrets and age key generation for grant (#44) (ad07d02)
- hoist secrets sync [--rotate] (#42) (44a18e8)
- secrets exec runtime (#39) (cfe247d)
- decrypt one secrets file with sops, keeping only the asked keys (#34) (40f5063)
- one fetch for secrets check and exec (#32) (0f6fb40)
- the token secrets exec reads with (#30) (c2ec1aa)
- stream a child's output through the redactor (#29) (c4f3897)
- redact base64 forms of values (#28) (6bfbcc9)
- redact values as written, URL-encoded or JSON-escaped (#27) (f4ae665)
- guards for secrets exec (#25) (9d7fe1e)
- secrets check --remote (consumer mode) (#22) (afb7dd1)
- secrets check (secrets repo) (#21) (43cc100)
- sops file metadata (#20) (4e176f1)
- generate .sops.yaml (#19) (d37704c)
- access.toml model (#18) (d5f3dda)
- calendar dates (#16) (59186f4)
- secrets.toml model (#13) (0e3781a)

### Fixes

- secrets sync replaces .sops.yaml atomically and flags removed recipients (#43) (6219ff2)
- harden secrets exec (keys out of environ, shm files, signals) (#40) (62f9042)
- build decrypt on unix only, so the Windows release builds (#37) (0b96ce6)
- kill what a successful sops leaves behind; fetch only through a Reader (#36) (ff231fe)
- kill a stuck or runaway sops with its process group (#35) (900dbaa)
- secrets token fails closed on nameless grants, dedupes repos case-insensitively (#33) (7de500d)
- network-path redirects, refusal message, logged hops (#31) (7445ff0)
- tighter guards for secrets exec (#26) (9b4544c)
- secrets-repo reads refuse a redirect to another host (#23) (eee8025)

## 0.2.0 (2026-10-22)

### Features

- release bootstrap registers trusted publishers (39aca47)
- release bootstrap publishes npm placeholders and tagged crates (e77e6f3)
- release bootstrap preflight and dry run (f735610)

### Fixes

- wait up to 5 minutes for npm after publish; table lists failed checks (837eb1c)
- bootstrap reads every npm trust config and cleans stale worktrees (e53442d)
- bootstrap hint follows the report and waits for trust (cd7a086)
- bootstrap publishes interactively so 2FA works (93a5d43)
- bootstrap confirms only real writes (36a4c75)
