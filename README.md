# reproductive-nix-cache

Evidence が複数の builder で一致した成果物だけを配信する、Nix Binary Cache
ゲートウェイです。NAR の保存とアップロードは Attic などの builder が指定した
cache が担当し、このサーバーは `.narinfo` と NAR を合意判定後にプロキシします。

## サーバー

```console
cargo run -p server -- \
  --cache-min-builders 2
```

server は起動設定、commit-reveal のラウンド作成とフェーズ遷移、commit/reveal 件数、
Builder の成否、出力の合意結果を標準エラーへ記録します。既定は `info` で、未合意の
判定も確認する場合は `RUST_LOG=reproductive_nix_cache_server=debug` を指定します。
commitment digest は `info`、reveal成功後のnonce、Evidence全文、cache URIは `debug` で
記録されます。Evidenceにはビルドのstdout/stderrも含まれるため、共有環境ではログの
保存先と公開範囲に注意してください。

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

## Builder Node と Round Manager

Builder Node はHTTPで受け取った命令を使って常駐ビルドし、CLIと同じ処理で
Evidenceをcommit/revealします。Builder固有のID、registry、cache locationは
Node側に固定され、ビルド命令から上書きできません。

```console
$ cargo run -p builder-node -- \
    --listen 0.0.0.0:51338 \
    --builder-id builder-a \
    --server 127.0.0.1:51337 \
    --cache-location https://attic-a.example.com/builds
```

既存serverをRound Managerとして使う場合は、Builder Nodeのoriginを複数指定します。
設定したNode数は `--commit-min-builders` 以上である必要があります。

```console
$ cargo run -p server -- \
    --build-queue-capacity 64 \
    --builder-node http://builder-a.internal:51338 \
    --builder-node http://builder-b.internal:51338
```

```console
$ curl -X POST http://127.0.0.1:51337/v1/builds \
    -H 'Content-Type: application/json' \
    -d '{"package_ref":"nixpkgs#hello","substitute":false,"claims":[]}'
# => HTTP 202 {"job_id":1,"queued":true}
```

Round Managerは要求をメモリ上のFIFOキューへ入れ、ビルド完了を待たずに
`202 Accepted`を返します。単一workerが要求を順番に取り出し、同じJSONを全Nodeへ
並列送信します。Nodeごとの成功・失敗、`builder_id`、round ID、Evidence IDは
serverの標準エラー出力へ表示されます。キューは永続化されないため、serverを
再起動すると待機中の要求は失われます。キュー満杯時は`503 Service Unavailable`を
返します。各Nodeは同時に1ビルドだけ受け付けます。
各NodeはEvidence完成後にcommitするため、Node間のビルド完了時刻の差が
`--commit-window-seconds` を超えないよう、実際のビルド時間に合わせてwindowを
設定してください。

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
