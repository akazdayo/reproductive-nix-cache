# reproductive-nix-cache

Evidence が複数の builder で一致した成果物だけを配信する、Nix Binary Cache
ゲートウェイです。NAR の保存とアップロードは Attic などの upstream cache が担当し、
このサーバーは `.narinfo` と NAR を合意判定後にプロキシします。

## サーバー

upstream cache は事前に `attic push` など、その cache 固有の方法で投入してください。

```console
cargo run -p server -- \
  --upstream-cache https://attic.example.com/builds \
  --cache-min-builders 2
```

`--upstream-cache` は `NIX_CACHE_UPSTREAM_URL` でも指定できます。初版では認証なしの
HTTP(S) cache を1つだけ使用できます。upstream 自体は外部公開せず、ゲートウェイから
だけ到達可能にする構成を推奨します。

## Nix Binary Cache として使う

```nix
{
  nix.settings.extra-substituters = [
    "https://repro-cache.example.com"
  ];
  nix.settings.extra-trusted-public-keys = [
    "upstream-cache-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
  ];
}
```

公開鍵には upstream cache が `.narinfo` の署名に使う鍵を設定します。ゲートウェイが
書き換えるのは署名対象外の `URL` だけなので、upstream の署名をそのまま検証できます。

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
