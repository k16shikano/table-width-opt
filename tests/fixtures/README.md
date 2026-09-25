# テスト用フィクスチャ

本書由来の表ではなく、列幅最適化の挙動を検証するための合成表です。

## 構成

- `two-col/table.tex` … 2列×5行
- `three-col/table.tex` … 3列×7行

各`table.tex`が表本体です。`%%COLSPEC%%`を列指定に置換して使います。

`cargo test --lib`はこれらのTeXを`examples/minimal-preamble.tex`で組版し、生成物をfixture横に残してから観測します。

## 生成物（途中過程を含む）

| ディレクトリ | 内容 |
|---|---|
| `three-col/bad/` | 狭い列指定の組版一式 |
| `three-col/optimized/` | 広い中央列の組版一式 |
| `three-col/mid/` | 中間列指定の組版一式 |
| `two-col/bad/` | 2列の組版一式 |

各ディレクトリに`out.pdf`のほか、uplatexの途中成果物（`_table_width_opt.tex`、`.log`、`.dvi`、`.aux`、`_preamble.tex`、`_measure-rules.tex`など）が残ります。

## 探索トレイル

`three-col/do.sh`は`table.tex`に対して`optimize`を回し、探索中のPDFを`three-col/optimize/`へ残します（`initial.pdf`、`candidate-iter*-*.pdf`、`optimized.pdf`）。
