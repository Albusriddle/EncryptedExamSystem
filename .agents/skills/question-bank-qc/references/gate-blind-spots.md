# 门禁能力与盲区（以 `scripts/verify.py` 源码为准）

> 常见错误：把“门禁没查”当成“门禁查不出”。本文件按源码逐项列出，改门禁后必须同步更新。
> 依据：`scripts/verify.py`（304 行，sha256 见报告）与 `scripts/qc.py`（V1–V8）。

## 1. `verify.py` 已经检查（不要重复实现，也不要声称它没查）

| 能力 | 源码位置（约） | 判定 |
|---|---|---|
| 5 个文件存在、题数精确 500/60/61/30/30 | `FILES`、`main()` | error |
| 顶层 `exam_id` 非空、`pick` 在 1..题量 | `check_bank` | error |
| id 库内唯一；kp 编号形如 `1.1`–`12.9`（越 51 点仅 warning） | `check_question` | error / warning |
| `difficulty` 合法（选择 1/2/3，多选必须 9，大题 1–4）；`score` 为整数 | `check_question` | error |
| `meta.scoring` 存在时：k/a/c 必须整数且在 1–3/1–4/1–4；非多选复算 S↔档（大题用 1.75/2.60/3.30） | `check_question` | error |
| 选择题 4 选项、键/文本不重复、答案在选项内；多选 `kind=multi`、2–3 项且升序 | `check_question` | error |
| fill：`body.kind=template`、`blank_count=3`、恰好 3 个 `____①____` 占位符、答案 3 项 | `check_question` | error |
| fix：`body.kind=code`、`blank_count=2`、2 处修改、`line` 越界、`wrong` 与原文逐字一致 | `check_question` | error |
| 大题：`body.kind=text`、`answer.kind=rubric`、`rubric.total==score`、评分点合计 `==score` | `check_question` | error |
| judge：`tests ≥3`、`harness` 非空；fill 官方答案回放；fix 官方通过 / 原始失败 / 撤销任一修改失败 | `check_judge` | error |
| 难度配额：choice/fill/fix 三档都 >0；fill/fix 每档 ≥15；单档 ≤60% | `check_bank` | error |
| 题干逐字重复（库内） | `check_bank` | error |
| 骨架族 ≤4；骨架种类 choice ≥120、fill/fix ≥30 | `check_bank` | error |
| 答案字母均衡（18%–32%）、ABCD 循环 <35% | `check_bank` | error |
| **跨库逐字同题干**（choice/fill/fix 两两） | `cross_bank_check` | error |
| 跨题引用（`上题/上一题/上述/第 N 题/见图/见表/沿用/同上`，只看题干） | `check_question` | error |
| 代码排版：空代码块、单行 `int main`、>40 字符且含 `;` 的单行、行内程序片段 | `check_question` | error |
| 大题统一难度 3、统一分值 15 | `main()` | error |
| harness 含 `fopen/fprintf/freopen`（写文件） | `cross_bank_check` | **warning** |

> `--no-compile` 会整段跳过 `check_judge`，但**不打印任何 warning**：输出仍是 `errors=0 warnings=0 PASS`，与完整门禁无法区分。这是最容易自欺的一处。

## 2. `verify.py` 不检查（必须 `qc.py` / 其他脚本 / 人工补）

| 盲区 | 为什么门禁看不见 | 补法 |
|---|---|---|
| **缺 `meta.scoring`** | `if sc:` 才校验；未标注的题静默通过 | `qc.py` V4 报 REVIEW；`score.py` 会用启发式补算（`source=heuristic`） |
| **多选真实 S 越界** | 多选跳过 S↔档；`score.py` 对多选一律按 `{3,2,1}` 输出 2.15 | `qc.py` V4 直接读 `meta.scoring` 复算，越界 error |
| 选项“屏幕同形”：仅差尾随空格、NBSP、零宽字符 | 只看选项文本是否 `!=`，不做可见性归一 | `qc.py` V1（去空白/不可见字符后比较） |
| 空位是否被测试钉死 | 只验“官方答案通过”，不验“错误值失败” | `fill_inject.py` 注入错误值（全量权威）；`qc.py` V8 是小候选表子集且一律记 REVIEW，须逐条裁决 |
| 句式族（同壳换标识符） | 骨架算法只看数字/代码块，随机函数名会逃逸 | `qc.py` V3：掩掉标识符后再聚类 |
| harness 写文件 | `verify.py` 只 warning（且只认 `fopen/fprintf/freopen`） | `qc.py` V7：`remove/rename/system/fwrite/fputs/tmpnam` 也拦，记 error |
| 难度标注语义 | 只验“标注 ↔ 档位自洽”，不验标注是否真实 | 抽 15 题/库按 K/A/C 独立打分（偏差 >1 档即 FAIL） |
| 大题 C 是否按“核心算法行”标 | 公式只算，不核对行数范围 | 复核抽检 `answer.reference` |
| kp 语义是否同源 | 只验编号合法，不验编号含义 | 对照 51 点大纲人工抽查；`qc.py --strict-kp` 把越纲升 error |
| 概念题答案事实正确性 | 编译只能验代码题 | 人工核对 |
| pick 是否符合考务 | 只验 1..题量 | 与考务确认；异常值（500 题 pick=2）应质疑 |
| 领域等价答案（`&&`↔`&`） | 只看输出，域内等价会通过 | 逐条归类“等价/补用例”，不能静默 |
| **跨库同任务**（换个题面的同一任务） | 只查逐字题干 | 人工按任务族统计，任一库 ≥20% 必修 |
| 多选主观分布 10/35/5 | 不可机检 | 复核人逐题判，偏差记遗留项 |
| 难度抽样与盲评 | 无内置抽样器 | 复核人手工抽 15 题，题号写进报告（`check_choice.py` 有 `--sample/--seed`，仅限 choice 代码题） |

## 3. 使用顺序

1. `verify.py` **完整模式** —— 先拿到 0 error / 0 warning（确认耗时 >1 分钟，且非“未找到编译器”）；
2. `qc.py --blank-audit` —— 补选项可见性/句式族/难度账本（含多选 S）/kp/pick/harness/空位钉死；
3. `check_choice.py --all` / `fill_inject.py --strict` —— 答案与空位全量实跑；
4. 人工抽样（难度语义、概念题事实、多选答案键、跨库同任务）；
5. 全绿后再交付；任何修复都从第 1 步重跑。

## 4. 历史事故对照

| 事故 | 当时门禁状态 | 真实原因 |
|---|---|---|
| 选项只差尾随空格 | PASS | 门禁不做可见性归一 |
| 44 道单行 `int main` | PASS（旧版） | 旧版无排版规则；新版已 error |
| `n%10` 逆序题错答 | PASS | 用例只覆盖个位数，门禁只验“官方通过” |
| `&&` 或 `\|\|` 被乘积掩盖 | PASS | 缺含 0 的用例；注入检查才发现 |
| kp 越纲 112 条 | PASS（仅 warning） | 门禁只警告，不阻断 |
| 多选 `difficulty=9` 但 `meta.scoring` 旧值（S=2.60） | PASS | 多选跳过 S 校验；`qc.py` V4 才报 |
| `--no-compile` 当完整门禁 | PASS | 输出与完整门禁同形；必须看耗时与 `qc.py` 的 warning |
