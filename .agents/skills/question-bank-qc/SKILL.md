---
name: question-bank-qc
description: Use when a C-language question bank must be produced, rebuilt, reviewed, or accepted before import — 题库质检/出题验收/门禁复核/难度重标/判题回放/修复复审/多生产者盲审。Runs 五关（结构/内容/难度可复算/分布/排版）with verify.py → score.py → qc.py → check_choice.py → fill_inject.py, then a 双盲生产→第三人定稿→0/0 门禁→修复→全量重跑→独立复审 loop; includes thresholds, scripts, references and a report template.
---

# 题库质检（question-bank-qc）

一套 C 语言题库要交付，必须同时过五关：**结构**（可导入）、**内容**（答案与判题经得起实跑）、**难度可复算**（K/A/C → S → 档位，同 S 必同档）、**分布**（题量/难度/字母/骨架/重复/kp/跨库）、**排版**（代码多行缩进）。五关各有脚本与证据，缺一关都不算验收。

> 铁律一：**门禁绿 ≠ 内容对。** `verify.py` 只证明结构、配额、判题材料能跑；选项在学生端不可分辨、错答能蒙对、难度标签与 K/A/C 不符，仍可能 PASS。
> 铁律二：**难度是“算”出来的，不是“摆”出来的。** 档位只由 `S = 0.35×K + 0.45×A + 0.20×C` 与固定阈值决定；按题号/位置/配额铺档一律不接受。
> 铁律三：**修复人不得自评。** 谁改的题，谁不能签这一轮的 PASS；复审必须换人，且从第一关全量重跑。

## 0. 适用与不适用

| 适用 | 不适用 |
|---|---|
| 原生 JSON 题库（选择/填空/改错/大题）交付前质检 | 单题润色、单个字段修改 |
| 双盲复核、门禁复核、整改后复审 | 非 C 语言题库（编译与判题模型需重写） |
| 多生产者合并、难度重标、kp 归并 | 教学法评价（题出得好不好） |
| `format_version: 2` 的 questions+answers 分离格式 | 纯 UI / 管理端功能缺陷 |

## 1. 五关与出口

| 关 | 查什么 | 命令 | 出口 |
|---|---|---|---|
| 结构 | 5 个文件在、题数 500/60/61/30/30、id/exam_id/pick、body.kind、答案形态、跨题引用 | `verify.py`（完整模式） | errors=0 |
| 内容 | 官方答案回放、原始代码/撤销任一修改必须失败、选项与 stdout 逐字命中、空位钉死、tests ≥3（边界用例人工抽检） | `verify.py` + `check_choice.py` + `fill_inject.py` | 全过；域内等价逐条归类 |
| 难度可复算 | 每题整数 `meta.scoring`、S 公式、固定阈值、多选 S 带、大题 3/15、fill/fix 每档 ≥15 | `verify.py` + `score.py` + `qc.py` V4 | 0 error；抽检偏差 ≤1 档 |
| 分布 | 字母均衡、ABCD 循环、骨架族/种类、库内与跨库重复、kp、pick、跨库同任务 | `verify.py` + `qc.py` V3/V5/V6 | 0 error；warning 逐条处置 |
| 排版 | 空代码块、单行 `int main`、长单行、行内程序、缩进、围栏语言 | `verify.py` + `qc.py` V2 + `pretty_c.py` | error 0；warning 记录 |

`qc.py` 会先调 `verify.py` 再补 V1–V8，因此**一次 qc 通过 ≠ 五关全过**：难度语义、多选主观分布、跨库同任务仍要人工判。

## 2. 复核流程（谁签哪一步）

第 0 步（所有轮次）：**冻结输入**——记录 5 个 JSON 的 sha256 / 字节数 / mtime，声明“本轮只读题库”，所有命令都在题库目录**之外**的一级临时目录跑。

| 步 | 谁做 | 做什么 | 出口 |
|---|---|---|---|
| 1 双盲生产 | 3 名出题人，互不可见 | 各自按 §3 题量独立出题；不看他人库与复核意见 | 3 套候选 + 各自 `meta.scoring` |
| 2 第三人复核/定稿 | 未参与生产的人 | 按五关抽检、选库合并、修硬伤；不做“口味”改写 | 定稿库 + 定稿报告 |
| 3 门禁 0/0 | 复核人 | `verify` + `qc` + `score` 全量跑 | verify errors=0 且 warnings=0；qc errors=0；warning 逐条有处置 |
| 4 修复 | 被指派修复人 | 只改题库，不改门禁/阈值；逐条留“修前→修后”证据 | 修复清单 |
| 5 全量重跑 | 修复人 | 五关全部重跑（不得只跑抽样），重算 sha256 | 新指纹 + 新原始输出 |
| 6 独立复审 | **不是修复人**的人 | 复核修复项、抽 15 题重打分、复跑门禁 | PASS 或退回第 4 步 |

- 第 6 步签字人必须与第 4 步不同；同一人改完自己签 PASS，本轮作废重来。
- 抽样：难度 15 题/库独立打分，覆盖三档与 ≥5 个 kp，并含全部“S 距阈值 <0.05”的边界题；多选 50 题逐题确认真实 S∈[2.00,2.20]；`check_choice.py --sample N --seed S` 只用于 choice 代码题盲抽（题号可给评分人）。
- 难度偏差 >1 档即 FAIL（不是“建议修改”）；循环只有一个终止条件：PASS + 独立复审签字。

## 3. 题量与分值（硬门禁）

| 库 | 题数 | 构成 | 单题分 | 总分 | pick |
|---|---|---|---|---|---|
| `choice.json` | 500 | 单选 450 + 多选 50 | 单选 2 / 多选 4 | 1100 | 50 |
| `fill.json` | 60 | 每题 3 空 | 4 | 240 | 10 |
| `fix.json` | 61 | 每题 2 处错 | 4 | 244 | 10 |
| `rutuan-q16-19.json` | 30 | 全部 code + rubric | 15 | 450 | 4 |
| `zhaosheng-b-q16-19.json` | 30 | 全部 code + rubric | 15 | 450 | 4 |

- 5 库合计 **681** 题；题量写在 `verify.py` 顶部 `FILES`，**它是唯一权威**，改题量必须同时改 `FILES` 与本表。
- 大题：30 题/库、**全部难度 3、统一 15 分、450 分/库**，不按 1/2/3/4 分档；`rubric.total` 与评分点合计都必须是 15。
- fill 固定 3 空（`____①____` 等连续编号），fix 固定 2 处（`answer.answer[].line` 必须与 `body.code` 同行号内容逐字一致）。
- fill / fix 的难度 1/2/3 **每档 ≥15 题**，总量不设上限。

## 4. 难度可复算（K/A/C → S → 档位）

### 4.1 三项指标

| 项 | 权重 | 取值 | 判据 |
|---|---|---|---|
| K 知识点 | 35% | 1–3 | 1=单一事实/单表达式；2=1 主 + 1 支撑（数组+下标）；3=≥2 交互（嵌套循环+二维数组） |
| A 算法 | 45% | 1–4 | 1=无算法；2=单层循环/单次判断；3=嵌套/递归/≥2 步；4=设计（贪心/DP/状态机） |
| C 行数 | 20% | 1–4 | 1=0–3 行；2=4–8；3=9–20；4=>20（非空行） |

C 的统计范围：题干围栏块、`body.template`、`body.code`。大题按 `answer.reference` 的**核心算法行**计（去 `#include`/`main`/IO/花括号/`return 0` 样板），由作者标注、复核抽检；纯文字无参考解记 0 行（C=1）。完整可达 S 值表见 `references/difficulty-formula.md` §5。

### 4.2 公式与固定阈值

```text
S = 0.35×K + 0.45×A + 0.20×C          (S ∈ [1, 4])
```

| 题型 | 分档（同 S 必同档） |
|---|---|
| 单选 / fill / fix | S < 1.90 → 1；S < 2.55 → 2；否则 3 |
| 大题 | S < 1.75 → 1；S < 2.60 → 2；S < 3.30 → 3；否则 4（当前一律 3） |
| 多选 | 真实 S ∈ [2.00, 2.20]；JSON `difficulty` 固定 **9** |

### 4.3 四条硬口径

- **多选**：50 题全部 `difficulty=9`；真实 S 必须落在 [2.00, 2.20]（可达值只有 2.00/2.05/2.10/2.15/2.20，现库统一 `{k:3,a:2,c:1}` → S=2.15）。主观重心目标 **10 易 / 35 中 / 5 难**，偏离只记遗留项（现库实测约 13/30–31/6–7），**不得**因此把 9 改成 1/2/3。
- **大题**：30 题/库、全 3、15 分/450 总分；K/A/C 仍要标（统一 `{3,3,2}` → S=2.80，落在 D3 的 [2.60, 3.30)），仅用于质量抽检；**不得**改 `difficulty`。
- **fill / fix**：难度 1/2/3 **每档 ≥15 题**，总量不限；三档都不许为 0，单档占比 >60% 也判 error；不许平移标签凑档，只能改题。
- **只由 S 决定档位**：不得按题号、位置、配额切档；每题必须有整数 `meta.scoring={k,a,c}`，无法标注的题不得入库；`verify.py` 会用该标注复算 S 并与 JSON `difficulty` 比对（多选只强制 `9`，见 §4.4）。

### 4.4 `score.py` 的真实能力边界（别把它当评分门禁）

| 能做 | 不能做 |
|---|---|
| 读 5 个 JSON，按 `meta.scoring` 复算 S，打印每库难度分布 + 前 3 行样例 + `dry-run` | **不报 error、正常输入下退出码恒 0**；不比对 `difficulty`（那是 `verify.py` 的事） |
| `--write` 按重算值覆写 `difficulty`（多选写 9；大题按 S 分档，并把 `score`/`rubric` 改成旧的 10/15/15/25 表） | 不校验多选真实 S（照 `meta.scoring` 打印 S，但档位一律写 9，越界不报错）；不强制大题 3/15 |
| 缺标注时用启发式估 `k/a/c`，`source=heuristic` 标出来 | 不校验 fill/fix 每档 ≥15（`verify.py` 管）；不报缺标注（`qc.py` V4 管） |

> **大题库禁止 `--write`**：实测把某大题 S 改成 1.00 后，`--write` 会把 `difficulty` 改成 1、`score`/`rubric.total` 改成 10，直接破坏“统一 3/15、450/库”。大题只能按 §3 人工核，或先备份再逐题确认。
> `--write` 也会把启发式结果写进 `meta.scoring`，**只有作者确认 `k/a/c` 后才能用**；`source=heuristic` 的行必须人工改掉。`score.py` **没有** `--sample` / `--reveal` / `--suggest` / `--json` 参数。

## 5. 分布关

| 项 | 阈值 | 出处 |
|---|---|---|
| choice 单选档位 | 三档都 >0，单档 ≤60%；实测 180/137/133，**非配额** | `verify.py` |
| 答案字母 | A/B/C/D 各 18%–32%（实测 113/113/112/112） | `verify.py` |
| ABCD 循环 | 与 `"ABCD"[i%4]` 吻合 <35%（实测 125/450 = 27.8%） | `verify.py` |
| 重复 | 库内逐字 0；choice/fill/fix 跨库逐字 0 | `verify.py` |
| 骨架 | 骨架族 ≤4；骨架种类 choice ≥120、fill/fix ≥30 | `verify.py` |
| 句式族 | 掩掉标识符后同壳 ≤4（warning） | `qc.py` V3 |
| kp | 51 点大纲；越纲默认 warning，`--strict-kp` 升 error | `qc.py` V5 |
| 元数据 | 5 个 exam_id 互不相同、无路径字符；pick 在 1..题量 | `qc.py` V6 |

**跨库同任务**（同一能力/同一算法骨架换个题面）：

- 分母是**该库题量**，逐库统计；某能力在任一库占比 **≥10% 必修**（拆题或换场景）。
- **<10% 可接受**，但报告要写明占比与判定依据（如大题 1/30 = 3.3%）。
- 库内同理：同一能力 ≥10% 就是分布失衡，不是“只在跨库时才管”。
- `verify.py` 只查**逐字**题干；任务级重复必须人工统计。

## 6. 排版关

| 规则 | 判定 | 出处 |
|---|---|---|
| 空代码块 / 单行 `int main` / 单行围栏 >40 字符且含 `;` / 程序片段写成行内代码 | error | `verify.py` |
| 围栏语言非 `c` / `{` `}` 未独占行 / main 内语句未缩进 / TAB | warning | `qc.py` V2 |
| 4 空格缩进、`{` `}` 独占行、语句逐行 | 规范 | `pretty_c.py --write` |

`pretty_c.py` 只处理含 `int main` 的单行块；必须先 dry-run，**逐题编译且输出完全一致**才 `--write`，否则手改。

## 7. 命令（全部在临时目录，题库只读）

```bash
export PYTHONIOENCODING=utf-8
SKILL=/path/to/question-bank-qc      # 本仓为 ees-banks/skill-drafts2/C；安装后为 .agents/skills/question-bank-qc
BANK=/path/to/bank                   # 含 5 个 JSON 的目录
work=$(mktemp -d) && cd "$work"      # verify.py 把 eesverify_* 建在 cwd

# 1 结构 + 内容 + 判题回放（完整模式；--no-compile 只验结构，不算门禁）
python "$SKILL/scripts/verify.py" "$BANK"

# 2 难度分布（dry-run 不改题库；分布与 §3/§5 对照）
python "$SKILL/scripts/score.py" "$BANK"

# 3 补充门禁 V1 选项可见性 / V2 排版 / V3 句式族 / V4 难度账本（含多选 S）/
#   V5 kp / V6 pick / V7 判题写文件 / V8 空位钉死（--blank-audit 时实跑）
python "$SKILL/scripts/qc.py" "$BANK" --blank-audit --report "$work/qc.md"
#   注：V8 用的是小候选表（操作符/++/常数），覆盖不到类型名与长表达式；且“仍全过”无法区分
#   域内等价与真漏判，故 V8 一律记 REVIEW，须逐条裁决。全量空位审计以第 5 步 fill_inject.py 为准。

# 4 选择题代码全量实跑（8 并发；no-main / no-code 需人工）
python "$SKILL/scripts/check_choice.py" "$BANK" --all --jobs 8

# 5 fill 空位钉死（--strict 时 UNPINNED 也返回 1）
python "$SKILL/scripts/fill_inject.py" "$BANK" --per-blank 4 --jobs 8 --strict

# 6 排版修复：先 dry-run，输出逐题一致才写盘
python "$SKILL/scripts/pretty_c.py" "$BANK" --file choice.json
python "$SKILL/scripts/pretty_c.py" "$BANK" --file choice.json --write

# 7 管理端导入（必须用 admin 工程环境；PYTHONPATH 指向 admin 根，否则 import app 失败）
PYTHONPATH=/path/to/EncryptedExamSystem/admin \
  uv run --project /path/to/EncryptedExamSystem/admin python \
  "$SKILL/scripts/admin_validate.py" "$BANK"

# 8 选择题盲抽（只给题号；难度抽样仍是人工 15 题）
python "$SKILL/scripts/check_choice.py" "$BANK" --sample 20 --seed 20261118
```

Windows 无 `mktemp` 时：`work=_qcwork/$(date +%s); mkdir -p "$work"; cd "$work"`（`_qcwork/` 必须在题库目录**之外**）。
收尾：`rm -rf eesverify_*`；任何 `eesverify_*` / `_qcwork/` 都不许进交付包。

## 8. 判定

| 结论 | 条件 |
|---|---|
| **PASS** | `verify.py` 完整模式 errors=0 warnings=0 且耗时 >1 分钟；`qc.py` errors=0（REVIEW 全部有裁决）；`score.py` 分布与 §3/§5 一致；`check_choice` 无 bad/compile-error/timeout 且 no-main/no-code 已人工核对；`fill_inject` baseline 0、UNPINNED 逐条归类；五关全过；报告与指纹同批 |
| **FAIL** | 任一 error；A 级未清；只跑抽样没全量重跑；修复人自评；报告数字沿用上一轮 |

- **warning** 必须逐条写处置（修/接受理由），不得静默；**REVIEW** 必须有人签字。
- 循环：FAIL → 修复 → 全量重跑 → 新指纹 → 换人复审 → 直到 PASS。
- `--no-compile` / `--no-gate` 下的 PASS 不是 PASS（`qc.py` 会 warn；`verify.py` 不会，只能看耗时）。

## 9. 报告

模板 `assets/report-template.md`：复制到被检目录之外（如 `review/`）逐节填，必含指纹（5 个 sha256）、门禁原始输出（不删行）、五关逐关结论、A/B 级必改项（每条带复验命令 + 输出片段）、抽样记录（三档 + ≥5 个 kp）、遗留项（为什么不阻断 + 谁裁决）、复现命令。

每个数字后面都要跟一条能复现它的命令；写不出命令的数字删掉。模板统计表里的示例数字是 2026-11-18 最终库的样本，填表一律换成本轮实测值。

## 10. 常见坑（真实事故）

1. 选项只差尾随空格 / NBSP / 全角空格 / 零宽字符 → `qc.py` V1 按 `vis_norm` 判同形（error），换可见干扰项。
2. 整段 `int main` 压成单行 → `pretty_c.py` 美化，禁止手改缩进后不复跑。
3. 选项写 `n%10` 逆序但题干要完整逆序 → `check_choice.py` 用真实 stdout 逐字比对。
4. `&&` 或 `||` 被 `w*h` 之类乘积掩盖 → 补含 0/负值/边界的用例。
5. 死存储空（`c += 0`）填什么都过 → `fill_inject.py` 抓 UNPINNED，改题或删空。
6. 难度按题号位置铺 → 按 S 重标；`score.py --write` 只写回重算值，不替作者判断标注是否真实。
7. 答案 ABCD 循环 → 置换选项并同步重映射答案键。
8. v2 旧格式 `difficulty=0` 表示“多选”而非难度 0 → 转原生后 `type=multiple` + `difficulty=9`。
9. 判题 harness 写当前目录（`fopen`/`fprintf`/`rename`/`system`）→ `qc.py` V7 error（`verify.py` 只是 warning），并发判题会互相踩。
10. kp 越纲 → 归并回 51 点或扩纲；`--strict-kp` 阻断。
11. `pick` 与题量不相称（如 500 题 pick=2）→ 找考务确认，不要默认通过。
12. 只修抽样题 → 必须全量重跑，局部修会引入新的不一致。
13. `verify.py` 的 `eesverify_*` 建在 **cwd** → 不先 `cd` 临时目录就把垃圾留在题库里。
14. `check_choice.py` 的 no-main/no-code 不计入退出码 → 必须人工核对并写进报告。
15. `fill_inject.py` 不加 `--strict` 时 UNPINNED 不返回 1 → 按 A 级项人工判，别当“通过”。
16. 找不到 C 编译器时 `verify.py` 对每道 fill/fix 各记一条 warning（121 条）却仍可能 errors=0 PASS → `qc.py` 会把该情形记 error；完整门禁必须确认耗时 >1 分钟。
17. 大题 rubric 评分点合计 ≠ 15 → `verify.py` 报 error；`score.py` **不报**，别只改 `total`。
18. 多选 S 靠“看起来差不多” → 必须算；[2.00,2.20] 内只有 5 个可达值，`{3,2,1}`=2.15。`verify.py` 与 `score.py` 都不查多选 S，只有 `qc.py` V4 查。
19. 报告里写“已修”却没有修前/修后输出 → 复审直接退回。
20. 沿用旧口径数字（单选 150/150/150、fill/fix 17/17/16、大题 4 题 10/15/15/25=65 分、总分 440）→ 现行是 180/137/133、26/19/15、28/18/15、30 题×15=450。
21. `score.py --write` 会把启发式 `k/a/c`（`source=heuristic`）写进 `meta.scoring`，并可能破坏大题统一口径（实测 S=1.00 的题被改成 difficulty=1、score=10）→ 大题库禁止 `--write`。
22. 跨库“同任务换个题面”不是逐字重复 → `verify.py` 查不出，必须人工按任务族统计 10%。
23. 两个空位审计工具覆盖面不同：`fill_inject.py` 是全量权威（含类型名/表达式候选），`qc.py` V8 只是小候选表子集 → 报告里要写清用的是哪个。

## 11. 目录与复用

```text
question-bank-qc/
├── SKILL.md
├── VERSION
├── scripts/{verify,score,qc,check_choice,fill_inject,pretty_c,admin_validate}.py
├── references/{thresholds,difficulty-formula,gate-blind-spots,v2-format}.md
└── assets/report-template.md
```

- `scripts/verify.py` 与 `scripts/score.py` 是题库仓 `ees-banks/` 的**逐字拷贝**（两文件与题库仓同 sha256，记录在本轮交付报告里），口径只在那里改一处、再同步过来；不要在本 skill 里“顺手优化”判定逻辑。
- 复用：改 `verify.py` 顶部 `FILES`/`KPS`、`qc.py` 的 `STD_FILES`/`KPS51`，再同步 `references/thresholds.md` 与 `assets/report-template.md` 的统计行。
- 权威顺序：**脚本源码 > 本 SKILL.md > references/ 附表**；三者冲突以源码为准，并在当轮同步附表。
- `verify`/`check_choice`/`fill_inject`/`pretty_c` 依赖 C 编译器（clang 优先、gcc 兜底），`admin_validate.py` 依赖 admin 工程环境（`PYTHONPATH=<admin>` + `uv run --project <admin>`，缺一即 `ModuleNotFoundError: app`）。

## 12. 速查

- **681** 题 = 500（单选 450 + 多选 50）+ 60 + 61 + 30 + 30；总分 1100 / 240 / 244 / 450 / 450。
- 分布：choice 单选 **180/137/133** + 多选 **50×9**；fill **26/19/15**；fix **28/18/15**；大题两库 **30×3 档 ×15 分 = 450**。
- 实跑：choice 代码题 **327/327**；fill 官方回填 **60/60**；fix 四段 **61/61**。
- 门禁完整模式 **2–6 分钟**（实测 2m05 无并发、5m39 与其他实跑并发；约 1022 次编译运行 + 681 题结构）；10 秒内 PASS 必是 `--no-compile`。
- 交付包 = 5 个 JSON + 报告 + 指纹 + 复现命令；临时目录、`eesverify_*`、`_qcwork/` 一律不入库。
