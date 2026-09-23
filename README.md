# table-width-opt

TeX表の列幅（colspecの`p`/`m`列のpc値）を、組版PDFの計測結果から最適化するCLIです。
探索はZ3による幅のみの辞書式最小化です。

## 依存

- Rust 2021
- [Z3](https://github.com/Z3Prover/z3)（`libz3`。ビルド時に`cmake`が必要になることがあります）
- 組版: `uplatex`、`dvipdfmx`
- PDF計測: [pdfium-render](https://crates.io/crates/pdfium-render)（システムのpdfium共有ライブラリ）

## ビルド

```bash
cargo build --release
```

## 使い方（概要）

```bash
./target/release/table-width-opt optimize \
  --tex table-source.tex \
  --preamble examples/minimal-preamble.tex \
  --table-index 0 \
  --work-dir /tmp/two-run \
  --out-colspec out.colspec \
  --out-pdf out.pdf

./target/release/table-width-opt observe --pdf out.pdf --out metrics.json
./target/release/table-width-opt report --pdf out.pdf
```

書籍向けのプリアンブル例は`examples/book-preamble.tex`です。
独自のクラスやスタイルがある場合は`--texinputs`でパスを渡します。

設計の詳細は[docs/SPEC.md](docs/SPEC.md)を参照してください。

## テスト

```bash
cargo test --lib
```

PDF計測の回帰用フィクスチャは`tests/fixtures/`に同梱しています。
外部の書籍ツリーは不要です。
