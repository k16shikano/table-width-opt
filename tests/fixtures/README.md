# テスト用フィクスチャ

本書由来の表ではなく、列幅最適化の挙動を検証するための合成表です。

## 構成

- `two-col/` … 2列×5行。`bad.metrics.json` のみ（PDF は未同梱）。
- `three-col/` … 3列×7行。`bad.pdf`、`optimized.pdf`、`bad.metrics.json`、`mid/_table_width_opt.pdf`。

各ディレクトリの `table.tex` が表本体です。`%%COLSPEC%%` を列指定に置換して使います。

## 再生成（任意）

`examples/minimal-preamble.tex`と各`table.tex`を、`table-width-opt compile`（uplatex）で組版し、生成PDFを`observe`してmetricsを更新します。
