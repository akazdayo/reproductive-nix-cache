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
