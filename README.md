# circuitgame2-cli

NCG2 の回路ソースをコマンドラインで検査する。組み合わせ関数を評価し、順序回路を tick ごとに実行する。

## NOR を試す

Rustup と Git を用意し、順序回路の実行に対応したブランチを取得する。[ツールチェーン](rust-toolchain.toml)は Rust 1.99.1 を指定する。

```sh
git clone --branch stage2/clocked-run https://github.com/neknaj/circuitgame2-cli.git
cd circuitgame2-cli
cargo run --quiet --locked -- check examples/nor.ncg2
cargo run --quiet --locked -- eval --func ::gate --input 00 examples/nor.ncg2
```

[例題](examples/nor.ncg2)の検査に成功すると `検査成功（1 ファイル）` と表示される。`eval` は `::gate` に 0 と 0 を入力し、NOR の出力 `1` を表示する。`--input 01` では `0` が返る。

## 自分のファイルを使う

CLI リポジトリのルートで `cargo install --path . --locked` を実行すると、`circuitgame2` コマンドをインストールできる。

回路プロジェクトのルートへ移動し、使う UTF-8 の `.ncg2` ファイルを相対パスで並べる。プロジェクト全体で `(using nor)` を一つ指定する。

```sh
circuitgame2 check gates.ncg2 main.ncg2
circuitgame2 eval --func ::lib::gate --input 01 gates.ncg2 main.ncg2
```

- `check` は構文、基底指定、宣言と公開指定、別名、定義内の参照を検査する。
- `eval` は同じ検査に加え、型と結線を検査し、指定したファイル群の `func` をネットリストに変換する。変換に成功すると、完全修飾名で指定した関数を評価する。マクロ展開が必要な箇所はエラーになる。
- 入力は `0` と `1` をポートの宣言順に並べ、各ポートの中では最下位ビットから記す。出力も同じ順で一行に表示する。
- 評価時の上限は、型のビット幅が 65,536、関数内の入力・定義・出力の配列がそれぞれ 1,048,576 ビット、展開した関数一つあたりの NOR ゲートが 1,048,576 個。
- 診断と警告は標準エラー出力に書く。`[開始..終了]` は UTF-8 のバイト範囲で、開始を含み終了を含まない。エラー時の終了コードは `1`。

## 順序回路を動かす

```sh
cargo run --quiet --locked -- run --input 11 --input 00 examples/latch.ncg2
```

[ラッチの例題](examples/latch.ncg2)ではラッチの出力 `q` と NOR の出力を観測する。各行は tick 番号と出力ビット列を空白で区切る。

```text
0 00
1 11
2 10
```

先頭行は reset 直後の出力。1 tick 目の `11` でラッチに 1 を書き込み、2 tick 目の `00` では値を保持する。NOR は各 tick の開始時点のラッチ出力を反転する。

- 開始点は既定で `::main`。別のモジュールは `--entry ::lib::main` のように完全修飾名で指定する。
- `--input` 一つが 1 tick 分の入力になる。各ポートのビット順は `eval` と同じ。入力ポートがない回路は `--input=` で 1 tick 進める。`--input` をすべて省くと reset の出力だけを表示する。
- ソースは引数に並べる。分割したモジュールは本体と `module-part` を含むファイルをすべて渡す。入力ポートの値はコマンドから直接指定する。
- モジュールの定義の右辺では基底 NOR、ラッチ、RAM を直接呼ぶ。実引数には信号名とビットリテラルを使う。
- 型のビット幅、配線のビット数、NOR ゲート数には `eval` と同じ上限を適用する。ラッチと RAM の状態領域は合計 1,048,576 ビットが上限。RAM の容量には読み出し出力も含む。

[モジュールの tick と状態素子の仕様](https://neknaj.github.io/circuitgame2/docs/simulation.html)

## Wasmtime で実行する

CLI リポジトリのルートで WASI 向けにビルドし、Wasmtime で実行する。`--dir=.` で実行中のディレクトリを渡す。

```sh
rustup target add wasm32-wasip2
cargo build --locked --target wasm32-wasip2
wasmtime run --dir=. target/wasm32-wasip2/debug/circuitgame2.wasm check examples/nor.ncg2
wasmtime run --dir=. target/wasm32-wasip2/debug/circuitgame2.wasm eval --func ::gate --input 00 examples/nor.ncg2
wasmtime run --dir=. target/wasm32-wasip2/debug/circuitgame2.wasm run --input 11 --input 00 examples/latch.ncg2
```

## 開発

```sh
cargo test --locked
```

回路の解析と評価には [circuitgame2-core](https://github.com/neknaj/circuitgame2-core) を使う。依存するコミットは [Cargo.toml](Cargo.toml) と [Cargo.lock](Cargo.lock) に固定している。

- [NCG2 の仕様と開発計画](https://github.com/neknaj/circuitgame2)
- [不具合報告と質問](https://github.com/neknaj/circuitgame2-cli/issues)
- [ライセンス（MIT）](LICENSE)
