# 题库质检报告：<库名 / 批次>

> 用法：复制本文件到被检目录之外（如 `review/`），逐节填充。**每个数字后面都要跟一条能复现它的命令**；写不出命令的数字删掉。

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

一句话：<差什么、为什么不能放行；例如"内容质量可用，但 3 道题的选项在学生端不可分辨、2 道题缺边界用例">。

| 项 | 结果 |
|---|---|
| 完整模式门禁 `python verify.py <dir>` | errors=<N> warnings=<N> / <PASS\|FAIL> / <耗时> |
| 补充检查 `python qc.py <dir>` | errors=<N> warnings=<N> review=<N> |
| 管理端导入 `load_paths` + `ExamBank.model_validate` | <5/5 OK>，`answer_problems=<N>` |
| 抽样实跑 | choice <N>/<N>、fill <N>/<N>、fix <N>/<N>、多选 <N>/<N> |
| 跨题引用 / 跨库同题干 | <0> / <0> |

## 2. 统计

| 库 | 题数 | 难度分布 | A/B/C/D | ABCD 循环 | 骨架种类 | 最大族 | 含代码题 | kp 数 |
|---|---|---|---|---|---|---|---|---|
| choice | <500> | <{1:150,2:150,3:150,9:50}> | <113/113/112/112> | <125/450 = 27.8%> | <500> | <2> | <327> | <49> |
| fill | <50> | <17/17/16> | - | - | <50> | <1> | <0> | <49> |
| fix | <50> | <17/17/16> | - | - | <49> | <2> | <0> | <50> |
| rutuan | <4> | <1/2/3/4> | - | - | <4> | <1> | - | <4> |
| zhaosheng | <4> | <1/2/3/4> | - | - | <4> | <1> | - | <4> |

口径声明（必填）：

- 难度行数口径：<可执行行（去 `#include`/`main`/花括号/`return 0`） / 非空行>
- kp 体系：<51 点大纲 / 其它（写明与大纲的映射关系）>
- `pick` 语义：<整库出卷 / 按比例抽题，比例 = …>

## 3. 必改项

> 一行一条，`级别` 只用 A（阻断：答案错/学生答对被判错/判题跑不起来）或 B（必修：违反硬性条款/分层抽题失真）。每条必须带**复验证据**（命令 + 输出片段）。

| # | 级别 | 位置 | 原因 | 修法 | 复验证据 |
|---|---|---|---|---|---|
| 1 | A | <C-0167 选项 B> | <`'fedcba '` 与 C `'fedcba'` 只差尾随空格，渲染后同形> | <换成 `'fecdba'`（实测为 `i<=n/2` 的真实输出）> | <编译运行 `stdout='fedcba'`，唯一命中 C> |
| 2 | B | <F-0013 judge.tests> | <3 条用例无边界（spec §4 硬性）> | <加 `a → A`（单字符）> | <官方 4/4 通过；`qc.py --blank-audit` 无未钉死空> |

## 3.5 抽样记录

| 项 | 数量 | 结果 |
|---|---|---|
| 代码选择题编译运行 | 20 | __/20 命中 |
| fill 回填 + 注入 | 10 | __/10 通过；注入 __/__ 被抓住 |
| fix 四段判定 | 10 | __/10 成立 |
| 多选答案键 | 10 | __/10 正确 |
| 难度语义 | 15 | 错标 __ 处 |

## 4. 遗留项（不阻断，但需裁决/说明）

| # | 项 | 为什么不阻断 | 谁裁决 / 后续动作 |
|---|---|---|---|
| 1 | <难度行数口径未定：≥9 行 219 题 vs 150 个 d3 名额> | <门禁只校验配额，两种口径下都可通过> | <规范方定口径，之后统一重标> |
| 2 | <fill/fix 一题一 kp，"每 kp ≥2 档"不可满足> | <50 题铺 49–50 个 kp，结构冲突> | <规范方修改条款或允许例外> |
| 3 | <13 处语义等价空（如 `&&`→`&`）> | <定义域内输出一致，判题无法区分> | <收紧题干域，或接受等价> |
| 4 | <`rt-19`/`zsb-19` 无标准样例（d4 开放题）> | <开放设计题没有固定 I/O> | <考务确认样例口径> |

## 5. 复现

```bash
cd D:\DSHworkspace\ees-banks
python verify.py <dir>                        # 完整模式，2–3 分钟
python qc.py <dir> --blank-audit --report review/qc-report.md
uv run --project D:\DSHworkspace\EncryptedExamSystem\admin python admin_validate.py
```

原始输出（原样粘贴，不要改写）：

```text
$ python verify.py <dir>
errors=0 warnings=0
PASS
real  2m10.902s
```

```text
$ python qc.py <dir>
errors=0 warnings=0 review=8
PASS
```

```text
$ uv run --project …\admin python admin_validate.py
choice.json   model_validate=OK exam_id=… pick=50 q=500 answer_problems=0
…
load_paths: loaded=5 errors=0 warnings=0
```

## 6. 自查（提交前逐项打勾）

- [ ] 门禁是**完整模式**跑的，耗时与编译器路径都记了（`warnings=0` 且非"未找到编译器"）
- [ ] 报告里每个数字都来自本次输出，未沿用上一轮
- [ ] 所有 A 级项都有"修前 → 修后"的实测证据，而不是"已修"
- [ ] 抽样覆盖三个难度档与 ≥5 个 kp，样本构成写在正文里
- [ ] 指纹与输出同批（先后顺序：改文件 → 算指纹 → 跑门禁 → 记输出）
- [ ] 我只读了被检目录，没写题库、没改仓库
- [ ] 遗留项逐条写了"为什么不阻断 + 谁裁决"
