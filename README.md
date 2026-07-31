# Nix Binary Cache として使う

```nix
{
  nix.settings.extra-substituters = [
    "https://cache.example.com?trusted=true"
  ];
}
```

## Commit-reveal

Build evidence is registered in derivation-scoped rounds. Each builder first
submits a salted SHA-256 commitment, waits until the commit phase closes, and
then reveals the complete evidence. Trust and binary-cache consensus only count
builders from the same round, so a later builder cannot copy a hash revealed in
an earlier round and add it as another vote.

Run independent builders concurrently so they join the same round:

```console
$ reproductive-nix-cache build nixpkgs#hello \
    --builder-id builder-a --server 127.0.0.1:51337
$ reproductive-nix-cache build nixpkgs#hello \
    --builder-id builder-b --server 127.0.0.1:51337
```

The server closes the commit phase after two distinct builders or 60 seconds,
whichever comes first. The reveal phase also lasts at most 60 seconds. These
defaults can be changed with `--commit-min-builders`,
`--commit-window-seconds`, and `--reveal-window-seconds` (or their
`NIX_CACHE_COMMIT_MIN_BUILDERS`, `NIX_CACHE_COMMIT_WINDOW_SECONDS`, and
`NIX_CACHE_REVEAL_WINDOW_SECONDS` environment variables).

The v1 protocol endpoints are:

- `POST /v1/evidence/commitments`
- `GET /v1/evidence/rounds/{round_id}`
- `POST /v1/evidence/reveals`
- `GET /v1/evidence?round_id={round_id}`

Direct `POST /v1/evidence` submissions are intentionally disabled.
