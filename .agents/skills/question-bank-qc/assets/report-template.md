# 题库质检报告：<库名 / 批次>

> 用法：复制本文件到被检目录之外（如 `review/`），逐节填充。**每个数字后面都要跟一条能复现它的命令**；写不出命令的数字删掉。
> 下表括号里的数字是 2026-11-18 最终库的实测样本，**填表一律换成本轮实测值**。

- 质检人：<姓名> ｜ 日期：<YYYY-MM-DD HH:MM> ｜ 轮次：<首检 / 第 N 轮整改后复审>
- 对象：`<绝对路径>`（只读；本轮未修改任何题库文件）
- 门禁版本：`scripts/verify.py`（sha256 `<…>`）｜补充检查：`scripts/qc.py`（sha256 `<…>`）
- 输入指纹（Windows：`certutil -hashfile <文件> SHA256`；Linux/macOS：`sha256sum <文件>`）：

```text
choice.json              sha256=<…>  <bytes> B
fill.json                sha256=<…>  <bytes> B
fix.json                 sha256=<…>  <bytes> B
rutuan-q16-19.json       sha256=<…>  <bytes> B
zhaosheng-b-q16-19.json  sha256=<…>  <bytes> B
```

## 1. 结论

**<PASS（可交付） / FAIL（N 处必修）>**

一句话：<差什么、为什么不能放行；例如“内容质量可用，但 3 道题的选项在学生端不可分辨、2 道题缺边界用例”>。

| 项 | 结果 |
|---|---|
| 完整模式门禁 `python verify.py <dir>` | errors=<N> warnings=<N> / <PASS\|FAIL> / <耗时> |
| 补充检查 `python qc.py <dir> --blank-audit` | errors=<N> warnings=<N> review=<N> |
| 难度复算 `python score.py <dir>` | 五库分布见 §2；<与规范一致 / 偏差…> |
| 管理端导入 `load_paths` + `ExamBank.model_validate` | <5/5 OK>，`answer_problems=<N>` |
| 抽样实跑 | choice <N>/<N>、fill <N>/<N>、fix <N>/<N>、多选 <N>/<N> |
| 跨题引用 / 跨库同题干 | <0> / <0> |

## 2. 统计

| 库 | 题数 | 难度分布 | A/B/C/D | ABCD 循环 | 骨架种类 | 最大族 | 含代码题 | kp 数 |
|---|---|---|---|---|---|---|---|---|
| choice | <500> | <{1:180,2:137,3:133,9:50}> | <113/113/112/112> | <125/450 = 27.8%> | <500> | <1> | <327> | <49> |
| fill | <60> | <26/19/15> | - | - | <60> | <1> | <0> | <49> |
| fix | <61> | <28/18/15> | - | - | <60> | <2> | <0> | <50> |
| rutuan | <30> | <全部 3> | - | - | <30> | <1> | - | <21> |
| zhaosheng | <30> | <全部 3> | - | - | <30> | <1> | - | <18> |

口径声明（必填）：

- 难度口径：`S = 0.35K + 0.45A + 0.20C`；小档 1.90/2.55、大档 1.75/2.60/3.30；多选 `difficulty=9` 且真实 S∈[2.00,2.20]；
- C 行数口径：<题干围栏块 + `body.template` + `body.code` 的非空行；大题按 `answer.reference` 核心算法行>；
- kp 体系：<51 点大纲 / 其它（写明与大纲的映射关系）>；
- `pick` 语义：<整库出卷 / 按比例抽题，比例 = …>。

## 3. 必改项

> 一行一条，`级别` 只用 A（阻断：答案错/学生答对被判错/判题跑不起来）或 B（必修：违反硬性条款/分层抽题失真）。每条必须带**复验证据**（命令 + 输出片段）。

| # | 级别 | 位置 | 原因 | 修法 | 复验证据 |
|---|---|---|---|---|---|
| 1 | A | <C-0167 选项 B> | <`'fedcba '` 与 C `'fedcba'` 只差尾随空格，渲染后同形> | <换成 `'fecdba'`（实测为 `i<=n/2` 的真实输出）> | <编译运行 `stdout='fedcba'`，唯一命中 C> |
| 2 | B | <F-0013 judge.tests> | <3 条用例无边界（spec §4 硬性）> | <加 `a → A`（单字符）> | <官方 4/4 通过；`qc.py --blank-audit` 无未钉死空> |

## 3.5 抽样记录

| 项 | 数量 | 结果 |
|---|---|---|
| 代码选择题编译运行 | <20 或 327> | __/__ 命中 |
| fill 回填 + 注入 | <10> | __/10 通过；注入 __/__ 被抓住 |
| fix 四段判定 | <10> | __/10 成立 |
| 多选答案键 + 真实 S | <10 或 50> | __/__ 正确；S 全部 ∈[2.00,2.20] |
| 难度语义（K/A/C 独立打分） | <15> | 错标 __ 处；偏差 >1 档 __ 处（应 0） |
| 抽样覆盖 | - | 三档齐全；≥5 个 kp；含 S 距阈值 <0.05 的边界题 |

## 4. 遗留项（不阻断，但需裁决/说明）

| # | 项 | 为什么不阻断 | 谁裁决 / 后续动作 |
|---|---|---|---|
| 1 | <多选主观分布 13/30–31/6–7，目标 10/35/5> | <多 1–2 道难、中档少 4–5 道；S 与答案键全部合格> | <复核人记录；下一轮重写 1–2 题陈述> |
| 2 | <fill/fix D3 各 15 题，正好卡下限> | <满足 ≥15；但任何降档立刻 error> | <考务/规范方；扩容时只增 D3> |
| 3 | <C-0017 选项 A 带尾随空格；F-0047 行内代码 52 字符> | <qc.py 记 warning，客户端不可见/可读> | <下一轮批量修> |
| 4 | <跨库同任务 1/30 = 3.3%> | <10% 阈值内；骨架族 ≤4 更严> | <记录即可> |
| 5 | <域内等价答案（如 `&&`→`&`）N 处> | <定义域内输出一致，判题无法区分> | <收紧题干域，或接受等价> |

## 5. 复现

```bash
export PYTHONIOENCODING=utf-8
SKILL=<本 skill 绝对路径>
BANK=<题库目录>
work=$(mktemp -d) && cd "$work"          # verify.py 把 eesverify_* 建在 cwd

python "$SKILL/scripts/verify.py" "$BANK"                      # 完整模式，≈2 分钟
python "$SKILL/scripts/score.py" "$BANK"                       # 难度分布（dry-run）
python "$SKILL/scripts/qc.py" "$BANK" --blank-audit --report "$work/qc.md"
python "$SKILL/scripts/check_choice.py" "$BANK" --all --jobs 8
python "$SKILL/scripts/fill_inject.py" "$BANK" --per-blank 4 --jobs 8 --strict
PYTHONPATH=/path/to/EncryptedExamSystem/admin \
  uv run --project /path/to/EncryptedExamSystem/admin python "$SKILL/scripts/admin_validate.py" "$BANK"
```

原始输出（原样粘贴，不要改写）：

```text
$ python verify.py <dir>
errors=0 warnings=0
PASS
real  2m04.656s
```

```text
$ python qc.py <dir> --no-compile
errors=0 warnings=3 review=0
PASS
```

```text
$ uv run --project …\admin python admin_validate.py
choice.json   model_validate=OK exam_id=… pick=50 q=500 answer_problems=0
…
load_paths: loaded=5 errors=0 warnings=0
```

## 6. 自查（提交前逐项打勾）

- [ ] 门禁是**完整模式**跑的，耗时与编译器路径都记了（`warnings=0` 且耗时 >1 分钟）
- [ ] 报告里每个数字都来自本次输出，未沿用上一轮（尤其 150/150/150、17/17/16、4 题 65 分这类旧口径）
- [ ] 所有 A 级项都有“修前 → 修后”的实测证据，而不是“已修”
- [ ] 抽样覆盖三个难度档与 ≥5 个 kp，样本构成写在正文里；多选逐题确认 S∈[2.00,2.20]
- [ ] 指纹与输出同批（先后顺序：改文件 → 算指纹 → 跑门禁 → 记输出）
- [ ] 我只读了被检目录，没写题库、没改仓库
- [ ] 遗留项逐条写了“为什么不阻断 + 谁裁决”
- [ ] 临时目录与 `eesverify_*` 不在交付包里
