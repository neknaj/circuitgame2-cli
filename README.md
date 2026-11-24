# circuitgame2-cli

`ncg2` は neknaj circuit game 2 の回路ソースを扱う CLI。ソースの検査と組み合わせ関数の評価ができる。

ラッチや RAM を含む順序回路を動かすには、開発版のインストール手順を使う。[順序回路に対応した開発版の README](https://github.com/neknaj/circuitgame2-cli/tree/5a1a9d2669c31518fd77a0b1872be3c485c79ad6#readme)

## インストールして NOR を試す

Rustup と Git を用意し、次のコマンドを実行する。[ツールチェーン](rust-toolchain.toml)は Rust 1.99.1 を指定する。

```sh
git clone https://github.com/neknaj/circuitgame2-cli.git
cd circuitgame2-cli
cargo install --path . --locked
ncg2 --version
ncg2 check examples/nor.ncg2
ncg2 eval --func ::gate --input 00 examples/nor.ncg2
```

`ncg2` が見つからない場合は、Cargo が表示した実行ファイルの保存先を `PATH` に加える。

[例題](examples/nor.ncg2)の検査に成功すると `検査成功（1 ファイル）` と表示される。`eval` は `::gate` に 0 と 0 を入力し、NOR の出力 `1` を表示する。`--input 01` では `0` が返る。

## 自分のファイルを使う

回路プロジェクトのルートへ移動し、使う UTF-8 の `.ncg2` ファイルを相対パスで並べる。プロジェクト全体で `(using nor)` を一つ指定する。

```sh
ncg2 check gates.ncg2 main.ncg2
ncg2 eval --func ::lib::gate --input 01 gates.ncg2 main.ncg2
```

- `check` は構文、基底指定、宣言と公開指定、別名、定義内の参照を検査する。
- `eval` は同じ検査に加え、型と結線を検査し、指定したファイル群の `func` をネットリストに変換する。変換に成功すると、完全修飾名で指定した関数を評価する。マクロ展開が必要な箇所はエラーになる。
- 入力は `0` と `1` をポートの宣言順に並べ、各ポートの中では最下位ビットから記す。出力も同じ順で一行に表示する。
- 評価時の上限は、型のビット幅が 65,536、関数内の入力・定義・出力の配列がそれぞれ 1,048,576 ビット、展開した関数一つあたりの NOR ゲートが 1,048,576 個。
- 診断と警告は標準エラー出力に書く。`[開始..終了]` は UTF-8 のバイト範囲で、開始を含み終了を含まない。エラー時の終了コードは `1`。

## Wasmtime で実行する

CLI リポジトリのルートで WASI 向けにビルドし、Wasmtime で実行する。`--dir=.` で実行中のディレクトリを渡す。

```sh
rustup target add wasm32-wasip2
cargo build --locked --target wasm32-wasip2
wasmtime run --dir=. target/wasm32-wasip2/debug/ncg2.wasm check examples/nor.ncg2
wasmtime run --dir=. target/wasm32-wasip2/debug/ncg2.wasm eval --func ::gate --input 00 examples/nor.ncg2
```

## 開発

```sh
cargo test --locked
```

回路の解析と評価には [circuitgame2-core](https://github.com/neknaj/circuitgame2-core) を使う。依存するコミットは [Cargo.toml](Cargo.toml) と [Cargo.lock](Cargo.lock) に固定している。

- [NCG2 の仕様と開発計画](https://github.com/neknaj/circuitgame2)
- [不具合報告と質問](https://github.com/neknaj/circuitgame2-cli/issues)
- [ライセンス（MIT）](LICENSE)
