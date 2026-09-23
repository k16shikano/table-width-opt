# table-width-opt 計画書

## 1. 目的

任意の TeX 表断片に対し、**列幅（colspec 内 p 列の pc 値）だけ**を調整して、組版 PDF が視覚的に妥当になる colspec を返す汎用 CLI。

評価根拠は **PDF の計測のみ**。colspec 探索は Z3 と有限幅集合の厳密列挙で行う。

## 2. スコープ

- 決定変数: colspec 内 pc のみ。列種（`p`/`m` 等）は固定
- 表結合なし、縦罫なし（observe 前提）
- RST 書き戻しは明示指示まで行わない
- `\tabulary` 第1引数、RST `\textwidth` 正規化後のゼロ和射影、SolverMode、セルごとの layout Bool 変数、個別表向け分岐は使わない

## 3. アーキテクチャ

```
compile → observe → score   （PDF から metrics と P）
        ↕
   optimize: measure → Z3 solve → compile verify → remeasure（反復）
```

- **cell_break** … 計測 PDF からセルごとの改行応答表（区分関数）を構築。compile なし
- **allocate** … 初期 colspec と `W_in`/`W_max`/列下限を metrics から算出。compile なし
- **solve** … 列幅 `w_j` のみを Z3 変数とし、応答表から行数・必要幅・均衡指標を区分関数で導入
- **optimize** … 各反復で現行 metrics → solve → compile → remeasure。seen 幅で停止

## 4. 数学モデル

### 4.1 決定変数

- `w_j` … 列 j の p 列内容幅（pc）。離散刻み 0.05pc

### 4.2 セル応答（区分関数）

計測 PDF の glyph/ink 幅と合法な折返し位置から、セル c の候補 layout 列を構築する。各候補 k は閾値 `T_{c,k}`（pc 単位）と `(n_lines, line_chars, width_pt)` を持つ。

列幅 `w_j` が与えられたとき、セル c の layout は **唯一** に定まる:

```
layout(c, w_j) = argmax_k { T_{c,k} | T_{c,k} ≤ w_j }
```

（同一閾値では行数が少ない候補を優先）

行数、必要幅、intra、extra_lines はすべて `w_j` の区分関数として Z3 に `ite` 鎖で符号化する。**layout を選ぶ Bool 変数は置かない。**

### 4.3 硬制約

| 条件 | 意味 |
|---|---|
| column_excess = 0 | 各列 `w_j` = 列内 `max_c req_c(w_j)`（最長セルにぴったり） |
| table width | `sum(w_j)` ≤ 利用可能な内容幅上限 |

`column_excess` は、PDF 上の列内容領域右端と、その列にある全行のうち最も右まで達した行の advance 端との差である。0.05pc の離散幅未満だけを量子化誤差として許す。

`W_max` は **内容幅のみ** の上限。PDF の `column_bounds` と入力 colspec から

```
table_w_pt = column_bounds[n] − column_bounds[0]
content_pt = Σ w_j × pt_per_pc_col[j]
fixed_overhead_pt = table_w_pt − content_pt
available_content_pt = table_w_pt + table_slack_right − fixed_overhead_pt
W_max = available_content_pt / (content_pt / Σ w_j)
```

### 4.4 目的関数

硬制約を満たす幅について、次の順で最小化する。

1. `extra_lines`: 各セルの `行数−1` の総和
2. 次の二項目の Pareto 最適集合
   - `inter_line_imbalance`: 各列のセル行数について `最大−最小` を求め、全列分を合計した値
   - `intra_line_imbalance`: 各複数行セルの行 advance 幅について `最大−最小` を求め、全セル分を合計した値

追加の改行によって列を縮める候補は、その列の他セルが候補幅からはみ出さず、かつ当該セルの行数と他の全セルの最大行数との差が2以下の場合だけ許す。

第2段の二項目には順序も重みも付けない。Pareto 最適な列幅をすべて返す。
表高、1文字行、セル間余白差、既存の重み付きペナルティは、この最小化問題の目的に含めない。

## 5. cell_break

- 計測 PDF 各セルについて、合法な改行位置（日本語は文字境界、識別子は観測分割点）から layout 候補を列挙
- 1 行統合候補と計測 layout（`measured_layout`）を含める
- 候補は **応答表の材料** であり、ソルバが layout を選ぶ変数ではない
- 列下限 `L_j` = 列 j で `layout(c, w_j)` が成立する最小の `w_j` の材料

## 6. allocate

compile を呼ばない。measure から `L_j`、`W_in`、`W_max`、初期 colspec 候補を構成する。最終出力は optimize 経由。

## 7. optimize

```
1. compile(入力 colspec) → observe → score
2. 反復（max_iter、seen 幅で停止）:
     a. allocate.build_plan(metrics)
     b. solve(problem) → 幅ベクトル
     c. compile → observe → score
     d. PDF 計測と応答表が一致しなければ応答表を更新
3. Pareto 最適な colspec を標準出力へすべて表示し、先頭の1件を指定ファイルへ出力
```

## 8. ゲート

| Gate | 条件 |
|---|---|
| O | observe 成功 |
| S | 改行数と二つのレンジが指定した優先関係どおり |
| Opt | `column_excess=0`、列幅合計が上限以下、PDF と解析目的値が一致 |

## 9. 禁止事項

- セル layout の独立 Bool 選択
- SolverMode / Narrow / Layout / Expand 分岐
- 個別表定数・手書き colspec 分岐
- 100 回 compile 前提の総当たり
- 硬条件だけで「最適化完了」と報告すること

## 10. 完了定義

- 列幅のみの最小化が SPEC と一致
- `cargo test --lib` 通過
- Gate O / S / Opt 通過（対象表）
