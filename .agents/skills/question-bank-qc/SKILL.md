---
name: question-bank-qc
description: Use when a C-language question bank must be produced, rebuilt, reviewed, or accepted before import — 题库质检/出题验收/门禁复核/判题回放/修复复审/多生产者盲审。Runs static checks → compile and judge replay → stratified sampling → gate verdict → fix-and-rereview loop, with explicit thresholds, scripts and report template.
---

# 题库出题质检（question-bank-qc）

一套题库要能交付，必须同时过四关：**结构合法**（可导入）、**内容正确**（答案与判题经得起实跑）、**分布合规**（难度/答案位置/知识点）、**排版可读**（代码多行缩进）。

> 铁律：**门禁绿 ≠ 内容对。** `verify.py` 只证明结构、配额、判题材料可跑；选项在学生端不可分辨、错答能蒙对、代码压成单行等，仍可能 PASS。门禁是下限，不是结论。

## 0. 适用与不适用

| 适用 | 不适用 |
|---|---|
| 原生 JSON 题库（选择/填空/改错/大题）交付前质检 | 单题润色、单个字段修改 |
| 双盲复核、门禁复核、整改后复审 | 非 C 语言题库（编译与判题模型需重写） |
| 多生产者合并、难度重标、kp 归并 | 教学法评价（题出得好不好） |
| `format_version: 2` 的 questions+answers 分离格式 | 纯 UI/管理端功能缺陷 |

## 1. 固定流程（七阶段）

| 阶段 | 动作 | 出口 |
|---|---|---|
| 0 冻结 | 记录 5 个 JSON 的 sha256、行数、mtime；只在临时目录工作 | 快照清单 |
| 1 静态 | `verify.py`（完整模式）+ `qc.py` | 结构/配额/重复/排版无 error |
| 2 编译判题 | `verify.py` 全量 fill/fix 回放；`check_choice.py --all` | 327/327 命中；fix 四段全过 |
| 3 抽样 | 难度 9 题/库、fill/fix 各 10 题、多选 10 题 | 抽样记录 |
| 4 判定 | 汇总 error/warning/必改项 | PASS 或 FAIL |
| 5 修复 | 只改题库，不改门禁；逐条修 | 修复清单 |
| 6 复审 | 重跑全量门禁 + 独立复核（修复人不得自评） | 新指纹 + PASS |
| 7 交付 | 报告 + 指纹 + 命令 | 可复现交付包 |

## 2. 质检项（按严重度）

### 2.1 A 级（阻断，必须修）

| # | 检查 | 方法 | 阈值 |
|---|---|---|---|
| A1 | 题数与题型 | 脚本统计 | choice 500（单选 450/多选 50）、fill 50、fix 50、大题 4+4 |
| A2 | id 唯一 / exam_id 唯一 | 全库集合 | 608 个 id 全局唯一；5 个 exam_id 不撞车 |
| A3 | 原子化 | 正则 `上题\|上述\|第\s*\d+\s*题\|见图\|见表\|沿用` | 0 次 |
| A4 | 难度口径 | 统计 | 单选 1/2/3 各 150（±2）；**多选统一 9（50 题）**；fill/fix 17/17/16；大题 1/2/3/4 |
| A5 | 答案键 | `check_choice.py --all` | 含代码选择题编译运行逐字命中 100%，且正确项唯一 |
| A6 | 判题材料 | `verify.py` 全量 | fill 官方回填 50/50；fix 官方通过/原始失败/撤销任一错处失败 50/50 |
| A7 | 空位钉死 | `fill_inject.py` | 注入错误值必须失败；域内等价须逐条归类说明 |
| A8 | 重复 | `verify.py` + `qc.py` | 库内/跨库逐字 0；骨架族 ≤4；骨架种类 choice ≥120、fill/fix ≥30 |
| A9 | 答案位置 | 统计 | 单选项 A/B/C/D 各 18%–32%；与 `"ABCD"[i%4]` 吻合 <35% |
| A10 | 代码排版 | `verify.py` 排版规则 | 禁止单行 `int main`；单行块 ≤40 字符；程序片段禁止行内代码；4 空格缩进 |
| A11 | 选项可见性 | `qc.py` V1 | 选项去首尾空白后两两不同；禁止仅差空格的选项 |
| A12 | 判题写文件 | `verify.py` | harness 含 `fopen/fprintf/freopen` 即 error |

### 2.2 B 级（必修，可批量）

| # | 检查 | 方法 | 阈值 |
|---|---|---|---|
| B1 | 难度语义 | 抽 9 题/库人工判 | 1=≤3 行/单点，2=4–8 行/1 陷阱，3=≥9 行/2 陷阱 |
| B2 | kp 覆盖 | 统计 | 原 51 点全覆盖；越纲 0 |
| B3 | 选项质量 | 抽样 | 4 选项、文本互异、无荒谬凑数 |
| B4 | 多选答案 | 统计 | 2–3 项、升序、不得全选/全不选 |
| B5 | 测试条数 | 统计 | 每题 ≥3 条，含至少 1 条边界 |
| B6 | pick | 顶层字段 | 1..题量；choice 50、fill/fix 10、大题 4（可按考务调整） |
| B7 | 句式族 | `qc.py` V3 | 掩标识符后同句式 ≤3 题 |

### 2.3 C 级（记录）

- 题干错别字、标点、风格统一；
- 难度边界题的主观分歧；
- 等价答案（域内语义等价）清单。

## 3. 命令（全部在临时目录跑，避免污染题库）

```bash
SKILL=.agents/skills/question-bank-qc
BANK=/path/to/bank            # 含 5 个 JSON 的目录
work=$(mktemp -d) && cd "$work"
export PYTHONIOENCODING=utf-8

# 1) 完整门禁（必须完整模式；--no-compile 只验结构）
python "$OLDPWD/$SKILL/scripts/verify.py" "$BANK"

# 2) 补充检查（选项可见性/句式族/难度账本/kp/pick/空位钉死）
python "$OLDPWD/$SKILL/scripts/qc.py" "$BANK" --no-compile --report "$work/qc.md"

# 3) 选择题代码题全量编译 + 抽样
python "$OLDPWD/$SKILL/scripts/check_choice.py" "$BANK" --all --jobs 8

# 4) fill 空位钉死
python "$OLDPWD/$SKILL/scripts/fill_inject.py" "$BANK" --per-blank 4 --jobs 8

# 5) 单行代码美化（先 dry-run；输出一致才写盘）
python "$OLDPWD/$SKILL/scripts/pretty_c.py" "$BANK"
python "$OLDPWD/$SKILL/scripts/pretty_c.py" "$BANK" --write

# 6) 管理端导入
PYTHONPATH=/path/to/EncryptedExamSystem/admin \
  uv run --project /path/to/EncryptedExamSystem/admin python \
  "$OLDPWD/$SKILL/scripts/admin_validate.py" "$BANK"
```

Windows 下 `mktemp` 不可用时，用 `work=_qcwork/$(date +%s); mkdir -p "$work"; cd "$work"`，并把 `$OLDPWD` 换成题库绝对路径。

## 4. 判定与循环

- **PASS**：`verify.py` errors=0；`qc.py` errors=0；A 级 12 项全过；B 级问题已修或书面记录；抽样无错。
- **FAIL**：任一 A 级不过，或 B 级 >3 项未处理。
- **循环**：FAIL → 修复 → 全量重跑（不得只跑抽样）→ 记录新 sha256 → **独立复核人**复审（修复人不得自评）→ 直到 PASS。
- **warning**：必须逐条写处置（修/接受理由），不得静默。

## 5. 报告模板

见 `assets/report-template.md`：结论、统计表、门禁原文、实跑证据、抽样记录、必改项、遗留项、复现命令、指纹。

## 6. 常见坑（全部来自真实事故）

1. 选项只差尾随空格（UI 不可分辨）→ 换可见干扰项；
2. 整段 `int main` 压成单行 → 多行缩进；
3. 答案 `n%10` 但题干要完整逆序（n≥10 错）→ 补边界用例；
4. `&&`/`||` 被 `w*h` 之类掩盖 → 补含 0/负值用例；
5. 死存储空（`c += 0`）填什么都过 → 删除或改题；
6. fill/fix 一题一 kp，“每 kp ≥2 档”结构性不可满足 → 规范需裁决；
7. 难度按题号位置铺（d2/d3 无客观差异）→ 按行数/陷阱重标；
8. 答案 ABCD 循环 → 选项随机置换并重映射；
9. 多选旧格式用 `difficulty=0` 表示题型 → 原生 JSON 用 `type=multiple`；
10. 判题 harness 写当前目录 → 并发/只读环境下失败；
11. kp 越纲（112 条 warning）→ 归并回大纲或扩展大纲；
12. pick=2 却 500 题 → 考务抽题异常；
13. 修复只修抽样题 → 必须全量重跑；
14. `--no-compile` 的 PASS 不是 PASS；
15. 临时目录落在题库里 → 污染目录、误入交付。

## 7. 门禁能力与盲区（以 `scripts/verify.py` 源码为准）

**已覆盖**：结构/题数/难度配额、pick、跨题引用、库内与**跨库**逐字重复（choice/fill/fix 两两）、骨架族、答案字母与循环、代码排版（单行 main/长单行/行内程序）、fill/fix 判题回放与突变、跨库同题干、判题写文件（error）。

**未覆盖（必须 `qc.py`/人工补）**：
- 选项“屏幕同形”（仅差空格/NBSP/零宽字符）；
- 空位是否被测试钉死（注入错误值才能发现）；
- 句式族（掩掉标识符后同壳）；
- 难度标签的语义正确性（行数与陷阱人工判断）；
- kp 语义是否与原大纲一致（只校验编号）；
- 概念题答案是否事实正确（编译只能验代码题）；
- pick 是否符合考务（只校验范围）。

详见 `references/gate-blind-spots.md`。

## 8. 目录与安装

```text
.agents/skills/question-bank-qc/
├── SKILL.md
├── VERSION
├── scripts/{verify.py,qc.py,check_choice.py,fill_inject.py,pretty_c.py,admin_validate.py}
├── references/{thresholds.md,gate-blind-spots.md,v2-format.md}
└── assets/report-template.md
```

其他项目复用时：复制整个目录，按本项目的题量/难度口径改 `verify.py` 顶部 `FILES`、`KPS` 与 `qc.py` 阈值；`check_choice`/`fill_inject` 依赖 C 编译器（clang 优先，gcc 兜底）。

## 9. 速查

- 门禁完整模式 ≈2–3 分钟；10 秒内 PASS 必是 `--no-compile`。
- 交付前最后一条命令永远是完整 `verify.py`，最后一份证据永远是原始输出。
- 交付包 = 5 个 JSON + 报告 + 指纹 + 复现命令；临时目录、`eesverify_*`、`_qcwork/` 一律不入库。
