# Builder Node + Round Manager デモ

このデモでは、Round Manager から同じビルド命令を 2 台の Builder Node に並列送信し、
両方の Node が Nix ビルド、Evidence の commit/reveal まで実行する流れを確認します。

`--cache-location` は省略しています。まずは Evidence と commit-reveal の動作を確認する
デモです。Binary Cache まで確認する場合は、各 Node に実際にアクセスできる HTTP(S) cache
の URL を `--cache-location` で追加してください。

## 前提

- リポジトリのルートで実行する
- Nix daemon と `nix develop` が利用できる
- `curl` と、結果を整形する場合は `jq` が利用できる
- 51337、51338、51339 番ポートが空いている

ローカル Git flake のため、`path:.` ではなくリポジトリルートで `nix develop` を実行します。

## 1. ビルド

```bash
cd /home/akazdayo/programs/sechack/reproductive-nix-cache
nix develop -c cargo build --workspace
```

## 2. Round Manager を起動

ターミナル A で実行します。

```bash
cd /home/akazdayo/programs/sechack/reproductive-nix-cache

NIX_CACHE_MANAGER_TOKEN=manager-secret \
NIX_CACHE_BUILDER_TOKEN=builder-secret \
nix develop -c cargo run -p server -- \
  --listen 127.0.0.1:51337 \
  --database /tmp/reproductive-nix-cache-demo.sqlite \
  --cache-min-builders 2 \
  --commit-min-builders 2 \
  --commit-window-seconds 120 \
  --reveal-window-seconds 120 \
  --builder-node http://127.0.0.1:51338 \
  --builder-node http://127.0.0.1:51339
```

`--commit-window-seconds` と `--reveal-window-seconds` は、デモ中のビルド時間に余裕を
持たせるため 120 秒にしています。

## 3. Builder Node を 2 台起動

ターミナル B:

```bash
cd /home/akazdayo/programs/sechack/reproductive-nix-cache

NIX_CACHE_BUILDER_TOKEN=builder-secret \
nix develop -c cargo run -p builder-node -- \
  --listen 127.0.0.1:51338 \
  --builder-id builder-a \
  --server 127.0.0.1:51337
```

ターミナル C:

```bash
cd /home/akazdayo/programs/sechack/reproductive-nix-cache

NIX_CACHE_BUILDER_TOKEN=builder-secret \
nix develop -c cargo run -p builder-node -- \
  --listen 127.0.0.1:51339 \
  --builder-id builder-b \
  --server 127.0.0.1:51337
```

起動確認:

```bash
curl -sS http://127.0.0.1:51337/
curl -sS http://127.0.0.1:51338/
curl -sS http://127.0.0.1:51339/
```

各レスポンスが `ok` になれば起動できています。

## 4. HTTP でビルドを要求

ターミナル D で Round Manager に要求します。

```bash
curl -sS -X POST http://127.0.0.1:51337/v1/builds \
  -H 'Authorization: Bearer manager-secret' \
  -H 'Content-Type: application/json' \
  -d '{
    "package_ref": "nixpkgs#hello",
    "substitute": true,
    "claims": []
  }' | jq
```

`substitute: true` は初回ビルドを短くするための設定です。再現性確認の `--rebuild` は
Builder Node 側で引き続き実行されます。キャッシュを使わずに試す場合は `false` にします。

成功すると、概ね次のようなレスポンスが返ります。ID は実行ごとに変わります。

```json
{
  "builders": [
    {
      "node": "http://127.0.0.1:51338/",
      "success": true,
      "builder_id": "builder-a",
      "round_id": 1,
      "evidence_id": 1
    },
    {
      "node": "http://127.0.0.1:51339/",
      "success": true,
      "builder_id": "builder-b",
      "round_id": 1,
      "evidence_id": 2
    }
  ]
}
```

確認ポイントは次の 3 つです。

- 同じ JSON が 2 台の Node に送られている
- 2 台とも `success: true` になっている
- `round_id` は同じで、`builder_id` は `builder-a` と `builder-b` に分かれている

## 5. エラー系の確認

Manager token を付けない場合は 401 です。

```bash
curl -i -X POST http://127.0.0.1:51337/v1/builds \
  -H 'Content-Type: application/json' \
  -d '{"package_ref":"nixpkgs#hello"}'
```

不正な package reference は 400 です。

```bash
curl -i -X POST http://127.0.0.1:51337/v1/builds \
  -H 'Authorization: Bearer manager-secret' \
  -H 'Content-Type: application/json' \
  -d '{"package_ref":"invalid"}'
```

同じ Node に同時に 2 件送ると、後から来た要求は Node busy の 409 になります。
長いビルド中に、別ターミナルから次を実行してください。

```bash
curl -i -X POST http://127.0.0.1:51338/v1/builds \
  -H 'Authorization: Bearer builder-secret' \
  -H 'Content-Type: application/json' \
  -d '{"package_ref":"nixpkgs#hello","substitute":true,"claims":[]}'
```

## トラブルシューティング

- `/v1/builds` が 503: server に `--builder-node` が設定されていません。
- `/v1/builds` が 401: `manager-secret` と `NIX_CACHE_MANAGER_TOKEN` が一致していません。
- Node が 401: `builder-secret` と `NIX_CACHE_BUILDER_TOKEN` が一致していません。
- 全 Node が失敗して 502: Node のログで Nix の失敗理由を確認します。
- `nix` daemon の権限エラー: 通常のユーザー環境で Nix daemon に接続できる shell から
  実行してください。

同じ SQLite ファイルを使って再実行すると、過去の Evidence と round が残ります。完全に
新しいデモにしたい場合は、別の `--database` パスを指定してください。
