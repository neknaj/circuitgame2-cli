# circuitgame2-cli

`ncg2` は neknaj circuit game 2 の回路ソースを扱う CLI。ソースの検査、組み合わせ関数の評価、順序回路の実行ができる。

## インストールする

Rustup と Git を用意し、順序回路の実行に対応したブランチを取得する。[ツールチェーン](rust-toolchain.toml)は Rust 1.99.1 を指定する。

```sh
git clone --branch stage3/lsp https://github.com/neknaj/circuitgame2-cli.git
cd circuitgame2-cli
cargo install --path . --locked
ncg2 --version
ncg2 --help
```

`ncg2` が見つからない場合は、Cargo が表示した実行ファイルの保存先ディレクトリを `PATH` に加える。

## 付属の NOR を試す

CLI リポジトリのルートで [NOR の例題](examples/nor.ncg2)を検査し、組み合わせ関数 `::gate` を評価する。

```sh
ncg2 check examples/nor.ncg2
ncg2 eval --func ::gate --input 00 examples/nor.ncg2
```

検査に成功すると `検査成功（1 ファイル）` と表示される。`eval` は 0 と 0 を入力し、NOR の出力 `1` を表示する。`--input 01` では `0` が返る。

## 順序回路を動かす

```sh
ncg2 run --input 11 --input 00 examples/latch.ncg2
```

[ラッチの例題](examples/latch.ncg2)ではラッチの出力 `q` と NOR の出力を観測する。各行は tick 番号と出力ビット列を空白で区切る。

```text
0 00
1 11
2 10
```

先頭行は reset 直後の出力。1 tick 目の `11` でラッチに 1 を書き込み、2 tick 目の `00` では値を保持する。NOR は各 tick の開始時点のラッチ出力を反転する。`(def q held)` は後に定義する `held` を参照し、同じ tick の値を `q` として公開する。

- 開始点は既定で `::main`。別のモジュールは `--entry ::lib::main` のように完全修飾名で指定する。
- `--input` 一つが 1 tick 分の入力になる。ポートの宣言順に、各ポート内では最下位ビットから並べる。入力ポートがない回路は `--input=` で 1 tick 進める。`--input` をすべて省くと reset の出力だけを表示する。
- ソースは引数に並べる。分割したモジュールは本体と `module-part` を含むファイルをすべて渡す。入力ポートの値はコマンドから直接指定する。
- モジュールの定義の右辺と実引数には、信号名や、信号名を起点にした `bit`・`slice` を使える。右辺には信号名やビットリテラルをつなぐ `concat` を書ける。基底 NOR、ラッチ、RAM、組み合わせ関数（`func`）、子モジュールも右辺から直接呼び出せる。実引数にはビットリテラルも使える。
- 型のビット幅、配線のビット数、NOR ゲート数には `eval` と同じ上限を適用する。ラッチと RAM の状態領域は合計 1,048,576 ビットが上限。RAM の容量には読み出し出力も含む。

[モジュールの tick と状態素子の仕様](https://neknaj.github.io/circuitgame2/docs/simulation.html)

### 信号からビットを選ぶ

[ビット選択の例題](examples/selected-latch.ncg2)では、`(def sampled (slice data 0 2 2))` が `data` のビット 0 と 2 を選んで `sampled` を定義する。ラッチには `sampled` を渡し、`(bit data 1)` で書き込み許可ビットを選ぶ。ビット 0 は最下位ビット。

```sh
ncg2 run --input 0110 --input 1000 examples/selected-latch.ncg2
```

1 tick 目でラッチに `01` を書き込む。2 tick 目は許可ビットが 0 なので `01` を保持する。

```text
0 00
1 01
2 01
```

### ビット列を下位からつなぐ

[連接の例題](examples/concat.ncg2)の `(concat low word 1)` は 1 ビットの入力 `low`、2 ビットの入力 `word`、定数 `1` を下位から順に並べる。

```sh
ncg2 run --input 010 examples/concat.ncg2
```

```text
0 0001
1 0101
```

reset 直後は入力がすべて 0 で、定数 1 は出力の最後に現れる。1 tick 目は `low` に 0、`word` に低位から `10` を入力し、その tick の出力に `0101` を得る。

### RAM の書き込みと読み出しを試す

[RAM の例題](examples/ram.ncg2)はアドレス、2 ビットの書き込み値、書き込み許可をこの順に受け取る。

```sh
ncg2 run --input 1101 --input 1011 --input 1000 --input 0000 examples/ram.ncg2
```

```text
0 00
1 00
2 10
3 01
4 00
```

最初の 2 tick でアドレス 1 に `10`、`01` の順に書き込む。書き込む tick の読み出しには更新前の値が出る。3 tick 目に同じアドレスを読むと `01` が得られ、4 tick 目にアドレス 0 を読むと初期値 `00` が得られる。

### 子モジュールごとの RAM を試す

[階層 RAM の例題](examples/hierarchy-ram.ncg2)では、`::main` が子モジュール `memory-port` を二度呼び、読み出し結果を `left` と `right` に接続する。入力はアドレス、左のデータと書き込み許可、右のデータと書き込み許可の順に並べる。

```sh
ncg2 run --input 11100 --input 10111 --input 10000 examples/hierarchy-ram.ncg2
```

```text
0 00
1 00
2 10
3 01
```

毎 tick、左右の RAM のアドレス 1 を読む。1 tick 目は左に 1 を書く。2 tick 目は左に 0、右に 1 を書き、同じ tick の出力には更新前の値 `10` が出る。3 tick 目は読み出しだけを行い、更新後の値 `01` が出る。

## 設定ファイルから実行する

ソースが複数ある回路では、UTF-8 の `circuitgame2.toml` にソース一覧と開始点を書く。[プロジェクトの例題](examples/project/circuitgame2.toml)は、CLI リポジトリのルートで実行できる。

```sh
ncg2 run --project examples/project/circuitgame2.toml --input 0 --input 1
```

```text
0 0
1 1
2 0
```

`sources` のパスは設定ファイルがあるディレクトリからの相対パスとし、区切りには `/` を使う。`entry` を省略すると `::main` を実行する。`run --project` では、ソースファイルの列挙と `--entry` は設定ファイルにまとめる。

`check --project` は設定に列挙したファイルを検査する。`eval --func … --input … --project …` も同じソース集合から、`--func` で指定した関数を評価する。この開発版で実行できるのは `.ncg2` の回路である。外部デバイスの接続を指定すると、その設定を診断する。

### 設定で公開した RAM を読む

[VRAM の例題](examples/vram/circuitgame2.toml)は、読み出し信号 `read-data` を公開名 `screen` に対応付ける。`--vram` を指定すると、回路の出力の後ろに RAM の全記憶を表示する。

```sh
ncg2 run --project examples/vram/circuitgame2.toml --vram screen --input 1101 --input 1000
```

```text
0 00 0000
1 00 0010
2 10 0010
```

3 列目は、その tick が確定した時点の記憶部である。各語のビットを最下位から並べ、語をアドレス順につなぐ。1 tick 目の回路出力は書き込み前の語を示す。同じ行の VRAM 列は書き込み後の内容を示す。

`--vram` を複数指定した場合は、指定順に列を追加する。公開名が `-` で始まる場合は `--vram=-screen` の形で指定する。`run --project` は `--vram` を省いた場合も、設定された読み出し信号を検査する。

## 定常化を調べる

入力を固定したときの定常化までの上界を `settle` で求める。同じ回路を CI で確認するときは、上限の tick 数を `--within` で指定する。

```sh
ncg2 settle --project examples/project/circuitgame2.toml
ncg2 settle --project examples/project/circuitgame2.toml --within 1 --period 2
```

例題の上界は 1 tick。条件を指定した実行では、1 tick 後から 2 tick 間隔で同じ内部状態になることを確認する。`--period` を省略すると間隔は 1 になる。

ネイティブ版の終了コードは次のとおり。

- 終了コード `0` は上界を推定できたか、指定した条件を検証できたことを表す。
- 終了コード `2` は、状態素子や閉路があるため上界を推定できない場合と、指定した条件を上界から証明できない場合に返す。
- 終了コード `1` は引数、設定、回路ソース、入出力のエラーを表す。

`settle` はゲートの深さから上界を求める。反例の探索は行わない。`wasm32-wasip2` 版を Wasmtime で実行すると、未推定・未証明と通常のエラーはどちらも終了コード `1` になる。未推定・未証明の報告は標準出力、通常のエラーは標準エラー出力に書く。

`--project` は設定のソース一覧と `entry` を使う。ファイルを直接指定するときは `--entry ::main` のように開始点を選べる。公開 VRAM と読み出し信号の対応は `run --project` で検査する。

## 自分のファイルを使う

[ラッチの例題](examples/latch.ncg2)を自分の回路用ディレクトリに `main.ncg2` として保存する。そのディレクトリへ移動して、インストールした CLI を実行する。

```sh
ncg2 check main.ncg2
ncg2 run --input 11 --input 00 main.ncg2
```

付属の例題と同じ出力が得られる。自分で書いた回路も同じ手順で実行する。UTF-8 で保存し、ソースを実行場所からの相対パスで並べる。プロジェクト全体で `(using nor)` を一つ指定する。

- `check` は構文、基底指定、宣言と公開指定、別名、定義内の参照を検査する。
- `eval` はさらに型と結線を検査し、指定したファイル群の `func` をネットリストに変換して、完全修飾名で指定した関数を評価する。マクロ展開が必要な箇所はエラーになる。
- `run` は開始点のモジュールと、そこから呼び出す回路の型や結線を検査してから実行する。
- 入出力はポートの宣言順に並べ、各ポートの中では最下位ビットから記す。`eval` の出力は一行のビット列になる。
- 評価時の上限は、型のビット幅が 65,536、関数内の入力・定義・出力の配列がそれぞれ 1,048,576 ビット、展開した関数一つあたりの NOR ゲートが 1,048,576 個。
- 診断と警告は標準エラー出力に書く。`[開始..終了]` は UTF-8 のバイト範囲で、開始を含み終了を含まない。エラー時の終了コードは `1`。

## Cargo から直接実行する

付属例は Cargo からも実行できる。CLI リポジトリのルートで、コマンドの先頭の `ncg2` を `cargo run --quiet --locked --` に置き換える。

```sh
cargo run --quiet --locked -- check examples/nor.ncg2
```

## Wasmtime で実行する

CLI リポジトリのルートで WASI 向けにビルドし、Wasmtime で実行する。`--dir=.` で実行中のディレクトリを渡す。

```sh
rustup target add wasm32-wasip2
cargo build --locked --target wasm32-wasip2
wasmtime run --dir=. target/wasm32-wasip2/debug/ncg2.wasm check examples/nor.ncg2
wasmtime run --dir=. target/wasm32-wasip2/debug/ncg2.wasm eval --func ::gate --input 00 examples/nor.ncg2
wasmtime run --dir=. target/wasm32-wasip2/debug/ncg2.wasm run --input 11 --input 00 examples/latch.ncg2
```

## エディタで構文診断を使う

ネイティブ環境でインストールすると、`ncg2` と `circuitgame2-lsp` が使える。エディタの LSP 設定で `circuitgame2-lsp` を標準入出力のサーバーとして起動し、`.ncg2` の言語 ID を `ncg2` に設定する。

開いた文書の字句、S 式、回路フォームを検査する。構文を回復できた場合は、後続のフォームの診断も表示する。

`def` の右辺の回路名・型名、`use` の参照先・別名、`module-part` の対象名から定義へ移動できる。`module-part` は同じ名前空間の `module` に移動する。構文・基底指定・宣言・`use` の検査が通れば、ほかの `def` に未解決の参照があっても、カーソル位置の参照は個別に解決できる。最寄りの `circuitgame2.toml` に対象ファイルが含まれる場合は、設定に列挙されたソースを解析する。開いているソースでは未保存の本文を使う。定義移動は回路言語だけで構成したプロジェクトに対応する。型検査と補完は今後の実装で対応する。

## 開発

```sh
cargo test --locked
```

回路の解析と評価には [circuitgame2-core](https://github.com/neknaj/circuitgame2-core) を使う。依存するコミットは [Cargo.toml](Cargo.toml) と [Cargo.lock](Cargo.lock) に固定している。

- [NCG2 の仕様と開発計画](https://github.com/neknaj/circuitgame2)
- [不具合報告と質問](https://github.com/neknaj/circuitgame2-cli/issues)
- [ライセンス（MIT）](LICENSE)
