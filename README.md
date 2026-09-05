# reproductive-nix-cache

複数の Builder が独立して実行した Nix ビルドの Evidence を比較し、結果が一致した
成果物だけを配信する Binary Cache ゲートウェイです。

Commit-Reveal によって各 Builder の事前コミットとビルド結果を検証し、合意した成果物を
登録済みの Binary Cache からプロキシします。Rust 製の CLI、Builder Node、Registry Server
で構成されています。
