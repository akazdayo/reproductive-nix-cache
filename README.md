# reproductive-nix-cache

Evidence が複数の builder で一致した成果物だけを配信する、Nix Binary Cache
ゲートウェイです。NAR の保存とアップロードは Attic などの builder が指定した
cache が担当し、このサーバーは `.narinfo` と NAR を合意判定後にプロキシします。

## サーバー

```console
cargo run -p server -- \
  --cache-min-builders 2
```

各 builder は事前に `attic push` など、その cache 固有の方法で成果物を投入し、
Evidence の reveal と同時に `--cache-location` で HTTP(S) cache のベース URI を通知します。
合意した出力を報告した builder の URI だけが取得候補になり、`.narinfo` が合意結果と
一致しない場合や取得できない場合は次の URIを試します。

```console
$ reproductive-nix-cache build nixpkgs#hello \
    --builder-id builder-a \
    --server 127.0.0.1:51337 \
    --cache-location https://attic-a.example.com/builds
$ reproductive-nix-cache build nixpkgs#hello \
    --builder-id builder-b \
    --server 127.0.0.1:51337 \
    --cache-location https://attic-b.example.com/builds
```

`--cache-location` は複数回指定できます。URI は `http` または `https` に限られ、
credentials、query、fragmentを含められません。redirectには追従せず、`.narinfo`
内のNAR URLは通知されたcacheと同一originの場合だけ使用します。

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

公開鍵には各cacheが `.narinfo` の署名に使う鍵を設定します。複数cacheが異なる鍵を
使う場合は、そのすべてを追加してください。ゲートウェイが書き換えるのは署名対象外の
`URL` だけなので、cacheの署名をそのまま検証できます。

## Commit-reveal

Build evidence is registered in derivation-scoped rounds. Each builder first
submits a salted SHA-256 commitment, waits until the commit phase closes, and
then reveals the complete evidence. Trust and binary-cache consensus only count
builders from the same round, so a later builder cannot copy a hash revealed in
an earlier round and add it as another vote.

Run independent builders concurrently so they join the same round:

```console
$ reproductive-nix-cache build nixpkgs#hello \
    --builder-id builder-a --server 127.0.0.1:51337 \
    --cache-location https://attic-a.example.com/builds
$ reproductive-nix-cache build nixpkgs#hello \
    --builder-id builder-b --server 127.0.0.1:51337 \
    --cache-location https://attic-b.example.com/builds
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
