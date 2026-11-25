# circuitgame2-cli

`ncg2` は neknaj circuit game 2 の CLI。NCG2 の回路と NCG2M の補助関数を検査し、組み合わせ関数の評価や順序回路の実行ができる。

## インストールする

Rustup と Git を用意し、次のコマンドを実行する。[ツールチェーン](rust-toolchain.toml)は Rust 1.99.1 を指定する。

```sh
git clone https://github.com/neknaj/circuitgame2-cli.git
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

## 検査結果と構文索引を JSON で取得する

```sh
ncg2 check --format json examples/nor.ncg2
ncg2 dump examples/nor.ncg2
```

回路の `check` は構文、名前解決、回路本体の結線と型、関数呼び出しの循環を検査する。`--format json` を指定すると、検査結果を標準出力に JSON で書き、検査に失敗した場合は終了コード 1 を返す。`diagnostics` にはエラーや警告の種別とメッセージがあり、構文や結線、プロジェクト設定の診断には、場所を特定できた場合にファイルのパスと範囲が付く。展開が必要な本体では、展開箇所を報告する。回路の実行や、型が未確定の子式の検査には対応する実行・評価コマンドを使う。

`dump` は構文木、宣言、参照の索引を出力する。構文が正しければ、解決できない名前を含むファイルでも参照の位置を調べられる。両コマンドで `--project circuitgame2.toml` を指定できる。範囲の `start` は UTF-8 のバイト位置で、`end` の位置は含まない。

## NCG2M の補助関数を検査する

[補助関数の例題](examples/helpers.ncg2m)では、別の名前空間で公開した `ready` を別名で取り込み、`startup-ready` から呼び出す。

```sh
ncg2 check examples/helpers.ncg2m
ncg2 check --format json examples/helpers.ncg2m
```

`check` は `defn` の本体の型と呼び出しの循環を静的に検査する。`--project` を使う場合は、設定ファイルの `sources` に `.ncg2m` のパスを並べる。実行段階が決まるまで検査できない式や、対応していないマクロ本体は、その位置を診断して終了コード 1 を返す。

`.ncg2` と `.ncg2m` を含む場合は、両言語の構文と回路の基底を先に検査し、共有宣言、公開、別名と `defn` を調べる。これらの検査が通った場合は、回路本体の検査が未対応であることを報告し、終了コード 1 を返す。

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
- `--input` 一つが 1 tick 分の入力になる。ポートの宣言順に、各ポート内では最下位ビットから並べる。
- 入力ポートがない回路を 30,000 tick 進めるには `--ticks 30000` を指定する。`--input=` でも 1 tick 進められる。`--ticks` と `--input` は併用できない。
- `--ticks 0` を指定した場合や、`--input` と `--ticks` をどちらも省いた場合は、reset 直後の出力だけを表示する。
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

`check --project` は設定に列挙したファイルを検査する。`eval --func … --input … --project …` も同じソース集合から、`--func` で指定した関数を評価する。実行できるソースは `.ncg2` の回路である。UART 端末を設定した場合、`ncg2 check --project` は接続先の方向と幅、重複駆動、`baud`、tick 周期を検査する。外部デバイスを接続した回路の実行は準備中。

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

## RP 向けの配線表を作る

```sh
ncg2 rp-plan --project circuitgame2.toml --board board.toml
```

Pico または Pico 2 向けに回路と端子の割り当てを検査し、配線表を標準出力へ書く。対象は単一ビットの入出力を持つ順序 NOR 回路。`tick-ns` は正の整数マイクロ秒で表し、各回路入力を GPIO または UART の一つの信号で駆動する。 [GPIO の設定とビルドの例](https://github.com/neknaj/circuitgame2/tree/main/examples/rp-gpio)

UART を使う場合は `board.toml` に `[uart]` を追加する。各ポート名をルート回路の宣言に合わせ、データのビットは最下位から並べる。

```toml
[uart]
instance = 0
tx-pin = 0
rx-pin = 1
baud = 115200
rx-data = ["rx0", "rx1", "rx2", "rx3", "rx4", "rx5", "rx6", "rx7"]
rx-valid = "rx-valid"
rx-framing = "rx-framing"
rx-overrun = "rx-overrun"
rx-ready = "rx-ready"
tx-data = ["tx0", "tx1", "tx2", "tx3", "tx4", "tx5", "tx6", "tx7"]
tx-valid = "tx-valid"
tx-ready = "tx-ready"
tx-done = "tx-done"
tx-ok = "tx-ok"
tx-done-ready = "tx-done-ready"
```

生成した UART 配線表を使う場合は、ファームウェアのビルドに `uart` feature を追加する。 [RP ファームウェアのビルドと信号の扱い](https://github.com/neknaj/circuitgame2-core/tree/main/firmware/rp)

## NOR ゲート数を調べる

`nor-count` はソース内の `func` と `module` を展開し、完全修飾名と NOR ゲート数をタブで区切って1行ずつ出力する。`--entry` で宣言を一つに絞れる。

```sh
ncg2 nor-count --project examples/project/circuitgame2.toml
ncg2 nor-count --entry ::demo::invert --project examples/project/circuitgame2.toml
```

呼び出し先の NOR ゲートも展開した出現箇所ごとに数える。直結、定数、ラッチ、RAM は個数に含めない。

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

## ファイルを整形する

`ncg2 fmt` は `.ncg2` と `.ncg2m` のファイルを一つずつ扱う。回路フォームまたは NCG2M の宣言ヘッダーを検査してから、コメントと文字列の表記を保ったまま空白と改行を整える。

~~~sh
ncg2 fmt main.ncg2
ncg2 fmt --check main.ncg2
ncg2 fmt --write main.ncg2
~~~

既定では整形結果を標準出力に書く。`--check` は元の内容が整形済みなら終了コード 0、整形が必要なら 1 を返す。`--write` は整形が成功して内容が変わる場合にファイルを置き換える。構文エラーでは元ファイルのバイト範囲を表示して終了コード 1 を返す。 [整形の仕様](https://github.com/neknaj/circuitgame2/blob/main/doc/formatting.md)

WASI 版は標準出力と `--check` に対応する。

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

## エディタで診断と定義移動を使う

ネイティブ環境でインストールすると、`ncg2` と `circuitgame2-lsp` が使える。VS Code と Neovim の拡張は、`PATH` 上の `circuitgame2-lsp` を起動して `.ncg2` を解析する。 [エディタ拡張の入手と設定](https://github.com/neknaj/circuitgame2-editors#readme)

手動で LSP を設定する場合は、`circuitgame2-lsp` を標準入出力のサーバーとして起動する。`.ncg2` の言語 ID は `ncg2`、`.ncg2m` は `ncg2m` に設定する。

開いた `.ncg2` の字句、S 式、回路フォームを検査する。構文を回復できた場合は、後続のフォームの診断も表示する。

`.ncg2m` では、字句、S 式、宣言の見出し、`namespace`・`use`・`export` の形式を検査する。検査が通ると、アウトラインに名前空間、補助関数、マクロを表示する。検査に失敗した場合は、文書全体で最初に見つけた違反を診断する。アウトラインには、元の括弧で閉じ、個別の構文検査を通ったトップレベルのリストに含まれる宣言を表示する。補助関数の型と呼び出し関係を確認するには `ncg2 check` を使う。

プロジェクトの構文、宣言、別名が確定すると、開いている文書の `def` にある回路名や型名を調べる。解決できない参照が複数あればそれぞれに診断を表示し、非公開の宣言など関連箇所があれば、そのファイルも示す。開いている依存ファイルを編集すると、影響を受けた文書の診断も更新する。

`def` の右辺の回路名・型名、`use` の参照先・別名、`module-part` の対象名から定義へ移動できる。`module-part` は同じ名前空間の `module` に移動する。構文・基底指定・宣言・`use` の検査が通れば、ほかの `def` に未解決の参照があっても、カーソル位置の参照は個別に解決できる。最寄りの `circuitgame2.toml` に対象ファイルが含まれる場合は、設定に列挙されたソースを解析する。開いているソースでは未保存の本文を使う。定義移動は回路言語だけで構成したプロジェクトに対応する。型検査の診断は今後の実装で対応する。

`module-part` の対象名にカーソルを置いて補完を呼ぶと、同じ名前空間にあるモジュールの名前を候補として表示する。別の箇所に構文エラーがあっても、断片と宣言が元の括弧で閉じていれば候補を表示できる。編集中に確認できた宣言から選ぶため、候補には「暫定」と表示する。

アウトラインでは、名前空間、`func`、`module` などの宣言を確認できる。識別子にカーソルを合わせると、解決した回路の署名や、型を確認できた信号の型・ビット幅を表示する。VS Code と Neovim の拡張では、名前空間、`func`、`module`、内部の信号、入出力ポートを色で区別する。

インレイヒントに対応するエディタでは、関数とモジュールを展開できた場合、宣言名の横に「NOR 3」のようにゲート数を表示する。定常化上界を求められたモジュールには「NOR 3 · 定常化 ≤ 2 tick」のように併記する。この上界は固定入力で内部ゲートの値が揃うまでの tick 数を表す。解析には、開いている依存ファイルの未保存の本文も使う。

## 開発

```sh
cargo test --locked
```

回路の解析と評価には [circuitgame2-core](https://github.com/neknaj/circuitgame2-core) を使う。依存するコミットは [Cargo.toml](Cargo.toml) と [Cargo.lock](Cargo.lock) に固定している。

- [NCG2 の仕様と開発計画](https://github.com/neknaj/circuitgame2)
- [不具合報告と質問](https://github.com/neknaj/circuitgame2-cli/issues)
- [ライセンス（MIT）](LICENSE)
