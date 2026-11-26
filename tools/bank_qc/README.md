# bank_qc —— 题库门禁与难度评分

> 口径来源固定为[出题质检与难度口径](../../docs/出题质检与难度口径.md)，脚本参数随该文档的变更同步。

两个**互相独立**的命令行工具，用来在把题库交给管理端导入之前，先把结构和质量问题挡在门外：

| 脚本 | 作用 | 会不会改文件 |
|---|---|---|
| `verify_bank.py` | 只读校验：结构 / 答案 / 判题材料回放 / 重复 / 原子化 / 代码排版 / 难度一致性 | **不会**（编译产物只写临时目录，退出即删） |
| `score_bank.py` | 按 `S = 0.35K + 0.45A + 0.20C` 算难度目标；`--write` 时回写 `difficulty` | 默认**不会**；只有 `--write` 才写，且默认只写 `difficulty` / `meta.scoring` |

口径来源：[出题质检与难度口径](../../docs/出题质检与难度口径.md)、[题库 JSON 规范](../../docs/题库JSON规范.md)。两份文档写的是「应该是什么样」，这两个脚本负责「实际是不是这样」。

> **它和管理端导入不是一回事。** 导入是产品层校验（格式 + 自洽），过了就能进工程；这两个脚本是**项目口径层**（题量、难度分布、重复、排版、判题材料）。两个都过才算能交卷。导入永远是唯一权威——工具是「提前发现」，不是「替代导入」。

---

## 1. 环境

- **Python 3.10+**（本机 3.13 实测）；只用标准库，**不需要** `pip install`，也不依赖 pydantic / PySide6；管理端装不装、装哪一版都不影响；
- **C 编译器**（只有 `verify_bank.py` 的 fill / fix 判题回放需要）：clang 或 gcc 都行。查找顺序 `--clang` → `$CLANG` → `$CC` → `PATH` 里的 `clang` / `gcc` / `cc`；
  - 找不到时**只给一条 WARN 并跳过回放**（题库没问题，是本机缺工具链），加 `--require-compile` 才当 error；
  - 想指定某一个：`--clang /path/to/clang`（Windows 上 `where clang` 查位置）；
  - 回放前会**先编译一个最小程序做冒烟**：编译器能启动但装坏了（比如 cc1 静默退出）同样只给一条 WARN 并跳过——否则 121 道题会报 121 条「编译失败」，看着像题库全错。

两个脚本都**不写死任何盘符或目录**：题库路径、编译器、临时目录全由参数 / 环境决定，可以在任何目录下运行。

---

## 2. 快速开始

Windows（cmd / PowerShell 均可，`python` 不行就换 `py -3`）：

```bat
:: 只看结构，秒级返回（不编译）
python tools\bank_qc\verify_bank.py <题库目录> --expect choice=500,fill=60,fix=61,rutuan=30,zhaosheng=30 --no-compile

:: 全量门禁：含 fill/fix 编译回放（编译器不在 PATH 时用 --clang 指定）
python tools\bank_qc\verify_bank.py <题库目录> --expect choice=500,fill=60,fix=61,rutuan=30,zhaosheng=30 --clang <clang 路径> --jobs 8

:: 难度核算：先看差异，再决定写不写
python tools\bank_qc\score_bank.py <题库目录>
python tools\bank_qc\score_bank.py <题库目录> --write --backup
```

Linux / macOS：

```bash
python3 tools/bank_qc/verify_bank.py <题库目录> --expect choice=500,fill=60,fix=61,rutuan=30,zhaosheng=30
python3 tools/bank_qc/score_bank.py <题库目录> --strict        # CI 里当门禁用
```

输出末尾固定是 `errors=N warnings=M` 加一行 `PASS` / `FAIL`，可以直接被 CI 抓。

---

## 3. `verify_bank.py`

```text
python verify_bank.py [PATH ...] [选项]
```

`PATH` 可以是文件或目录（目录默认只看一层，加 `--recursive` 递归）；不给就是当前目录。

| 选项 | 默认 | 说明 |
|---|---|---|
| `--expect SPEC` | 空 | 期望题数，如 `choice=500,fill=60,fix=61`；名字按**文件名主干**、`exam_id` 或**主干前缀**匹配（`rutuan=30` 命中 `rutuan-q16-19.json`） |
| `--no-compile` | 关 | 跳过 fill / fix 的编译回放，只做结构检查 |
| `--clang [PATH]` | 关 | 给路径 = 用它；只写 `--clang` = 优先用 PATH 里的 clang；不给 = `$CLANG`→`$CC`→`clang`→`gcc`→`cc` |
| `--require-compile` | 关 | 找不到编译器时算 error（CI 用） |
| `--jobs N` | `min(8, CPU)` | 回放并发数（每题独立编译，互不干扰） |
| `--min-per-level N` | `15` | fill / fix 每个难度档的最少题数；`0` 关闭 |
| `--max-skeleton-family N` | `4` | 同一骨架最多几题；`0` 关闭 |
| `--big-difficulty N` | `3` | 大题统一难度（取值必须 1–4）；`0` 关闭该检查 |
| `--big-score N` | `15` | 大题统一分值；`0` 关闭该检查 |
| `--big-lang L` | `c` | 大题必须写的语言（要求正好是 `code_languages: ["c"]`）；空串关闭 |
| `--limit N` | `0` | 每库只校验前 N 题（**冒烟专用**：题数、`pick`、分档下限自动不判） |
| `--max-report N` | `80` | 人读模式最多打印多少条结论 |
| `--json` | 关 | 输出 JSON（含每条 finding 的 severity / scope / where / rule / message，以及回放通过数） |

退出码：`0` 无 error（warning 不影响）；`1` 有 error；`2` 参数或 IO 问题；**`130` 用户中断**（`Ctrl-C`，两个脚本一致）。

### 3.1 能查什么

| 类别 | 具体检查 |
|---|---|
| 顶层结构 | `exam_id` / `title` / `pick` / `questions` 是否存在且类型正确；`exam_id` 的字符、长度、保留名、首尾点规则（与 `models.validate_exam_id` 一致）；`pick` 是否 `0 ≤ pick ≤ 题量`；题目 `id` 是否唯一；`title` 里的「N 题 / N 分」是否与库内实际一致（WARN） |
| 逐题字段 | `type` 是否在白名单（旧别名 `multi/blank/blanks/fix_code` 会提示）；**`kp` 在原生题库里必填**（`N.M`，1.1–12.9，缺失即 error；只有 `format_version: 2` 的旧格式才降为 WARN）；`difficulty` 是否按题型取值（**多选固定 `9`、大题 1–4、其余 1/2/3**）；`score` 是否 0–1000 的整数 |
| `body` | **必须是对象**（缺 `body`、或在顶层沿用旧的 `options: [String]`，都会报 `body 必须是对象` → error）；与题型是否匹配（`single/multiple/judge→options`、`fill→template`、`fix→code`、`code→text`）；选项 ≥2、≤26、键 / 文本不重复、键从 A 连续（不连续只告警） |
| 答案 | `single/multi/blanks/fixes/rubric` 五种形状；答案字母必须落在选项里；多选答案升序、不重复、2–3 项、**不能等于全部选项**；fill 占位符数 = `blank_count` = 答案条数、`no` 与占位符对位、**出现顺序是否 ①②③**（WARN）；fix 行号在范围内、`wrong` 与原文逐字一致、多条改正项不指向同一行、**改前改后不能相同** |
| 大题 rubric | `answer.kind=rubric`、`total == score`、`points` 非空且合计等于总分、`reference` 与 `samples` 存在、每个 sample 有 `input`/`output`、样例 <2 组给 WARN |
| 代码语言 | `code_language` 与 `code_languages` 不能共存；11 种白名单；≤8 个；重复项提示；大题必须是 `code_languages: ["c"]`（可用 `--big-lang` 改） |
| 题目级 `samples` | 必须是对象数组；最多 20 个；**单个 `input` + `output` 合计 ≤4096 UTF-8 字节**；两者不能同时为空 |
| 判题材料 | fill / fix 必须有 `judge.entry/harness/tests`；`tests ≥ 3`；每条有 `input` 与 `expected`（**兼容旧写法 `output`**）；`entry` 是否真的出现在 harness 里（提示）；harness 含 `fopen/fprintf` 时提示并发风险 |
| 编译回放 | fill：官方答案回放**全过**；fix：官方修正**全过**、原始代码**必败**、撤销任意一处修改后**必败**（每项都实跑编译器）；输出里显示 `judge 回放 60/60` |
| 重复 | 库内逐字题干（仅去空白）；骨架族 ≤4；**骨架种类下限**：`single` 题占一半以上的库要求 **120** 种、其它库 **30** 种，再按题量取小（`need = min(need, 题量)`）；跨库逐字同题干 |
| 原子化 | 题干出现 `上题/上一题/上述/前述/同上/沿用/见图/见表/此题/第 N 题` 即 error |
| 代码排版 | 空代码块；**围栏未闭合**；整段程序压在单行；`int main` 与 `{` 同行；单行围栏块 >40 字符；用行内代码写整段程序；**TAB 缩进**（WARN） |
| 难度一致性 | 有 `meta.scoring` 时按 `S×100 = 35K+45A+20C` 重算并与 `difficulty` 比对；多选额外要求真实 S ∈ [2.00, 2.20]；**大题 `code` 只查整数 / 范围，S 仅作诊断**（统一 3/15）。**整数比较**，2.55 / 3.30 这类边界不会因浮点误差跳档 |
| 分布 | fill / fix 每档 ≥ `--min-per-level`；某一档 >60% 提示；单选答案字母占比 **18%–32% 之外即 error**（≥40 道单选才判）、ABCD 循环吻合 >35% 提示 |
| 旧格式 | `format_version: 2` 的 `X.questions.json` + `X.answers.json` 成对时在内存里转换后跑同一套检查；`difficulty=0` 识别为「旧约定 = 多选」并提示迁移到 `9` |

### 3.2 不能查什么

| 查不了 | 原因 / 替代办法 |
|---|---|
| **题目内容对不对** | 回放只能证明「代码与用例自洽」，证明不了「这道题考的知识点、选项表述、陷阱设计是否正确」。大题的 `answer.reference` 不会被编译执行，需人工或另写脚本核对 |
| **边界用例是否真的覆盖边界** | 只能查 `tests ≥ 3` 与 `note` 是否非空；「这条用例算不算边界」要人看 |
| **语义级重复** | 骨架只做文本归一（代码块→`<CODE>`、数字→`<N>`），不理解「同一能力换个说法」。跨库「同任务」（如两边都有求和）只能按逐字重复查 |
| **排版是否好看** | 只查「程序被压成一行 / 行内代码写程序 / 围栏未闭合 / TAB」这几类硬伤，不评判缩进宽度与空行 |
| **图片能否显示** | 不解析 `stem` 里的 data URI，也不查大小与尺寸（用管理端的【插入图片】或 `tools/embed-image.py`） |
| **`exam_id` 全工程唯一** | 只看单个文件内的规则；跨文件重名要等管理端导入时才报 |
| **知识点覆盖是否均衡** | 只查 `kp` 格式，不统计每个 kp 的题量分布 |
| **等价的 pydantic 校验** | 本工具是独立复刻的规则，不是调用管理端模型；最终仍应**在管理端真导入一次**，那是唯一权威 |
| **库内难度是否「标得对」** | 只校验 `meta.scoring` 与 `difficulty` 是否自洽；K/A/C 标得准不准要复核人抽检（见口径文档 §4.2） |

---

## 4. `score_bank.py`

```text
python score_bank.py [PATH ...] [选项]
```

| 选项 | 默认 | 说明 |
|---|---|---|
| `--write` | 关 | 把目标 `difficulty` 写回文件（多选强制 9）；**默认只写 `difficulty` 与 `meta.scoring`** |
| `--backup` | 关 | 写入前留一份 `<文件名>.bak-<YYYYmmdd-HHMMSS>` |
| `--strict` | 关 | 有偏差就退出码 1，**不写文件**（CI 用；与 `--write` 互斥） |
| `--allow-heuristic` | 关 | 允许把启发式估算写进文件；不给则缺 `meta.scoring` 时**拒绝写盘**（退出码 3） |
| `--fix-score` | 关 | `--write` 时顺带把大题 `score` 与 `rubric` 合计改成统一口径 |
| `--big-mode uniform` | `uniform` | 大题统一难度 / 分值（默认 3 / 15 分），S 只作抽检参考 |
| `--big-mode formula` | — | 大题按 1.75 / 2.60 / 3.30 分档，分值 10/15/15/25 |
| `--big-difficulty N` | `3` | uniform 模式下大题的固定难度（必须 1–4） |
| `--big-score N` | `15` | uniform 模式下大题的固定分值 |
| `--no-meta` | 关 | `--write` 时只改 `difficulty`，不写 `meta.scoring` |
| `--max-report N` | `20` | 每库最多列多少条改动 |
| `--json` | 关 | 输出 JSON（含每题 from/to/k/a/c/S/source） |

退出码：`0` 正常；`1` `--strict` 下存在偏差（档位或大题分值）；`2` 参数或 IO 问题；`3` `--write` 被拒绝（有题缺 `meta.scoring`）；**`130` 用户中断**。

### 4.1 它能给出的结论

- 每题 `S`、目标档位、与当前 `difficulty` 的差异清单；
- 多选一律 `9`，并报告真实 `S` 落在 `[2.00, 2.20]` 的题数（`50/50` 这种）；
- 大题的 `uniform` / `formula` 两种口径；分值偏离统一口径会点名（`--fix-score` 才改）；
- `meta.scoring` 里 `source=heuristic` 的题会**一直**被标出来，不会因为写过一次就被当成「作者标注」。

### 4.2 它不能做什么

- **不能替你判断 K/A/C 标得对不对**：优先读 `meta.scoring`；没有标注时给的是**启发式估算**（关键词命中数 / 循环分支结构 / 非空代码行数），只用于占位——启发式**不会**给出 A=4（设计类），也不理解「核心算法行数」；
- 默认不改题干、选项、答案正文，也不改分值（只动 `difficulty`、`meta.scoring`；加 `--fix-score` 才动大题 `score`/`rubric` 合计）；
- 不做题数与判题材料校验（那是 `verify_bank.py` 的活）；
- 不处理旧格式（`format_version: 2` 没有 `meta`/`score`，会提示先转原生 JSON）；
- `--write` 会把文件重排成 `indent=2`、UTF-8（无 BOM）、LF 换行，**键顺序保持**；仍然建议 `--backup` 或先提交 git。

---

## 5. `samples` 的限额：schema 比实现更松

这条最容易踩，因为它会让「schema 校验通过」给人一种「格式已经没问题」的错觉。

`samples` 的**结构上界**（JSON Schema / 生成器）是：最多 20 组，`input`、`output` 各 ≤ 4096 **字符**。但**实际门禁更严**：Rust 校验、管理端导入、`verify_bank.py` 三处一致要求**每组 `input + output` 合计 ≤ 4096 UTF-8 字节**，且两者不能同时为空。schema 只是宽松上界——**以合计字节为准**，否则会出现「schema 过了、导入被拒」的排查成本。

具体差在哪：

| 层 | 规则 |
|---|---|
| JSON Schema | `maxItems: 20`；`input` ≤ 4096 **字符**、`output` ≤ 4096 **字符**（**没有**合计字节上限） |
| Rust `model.rs` | `len ≤ 20`；`bytes(input) + bytes(output) ≤ 4096` |
| 管理端 pydantic | 各字段 `max_length=4096`；另有 `MAX_SAMPLE_BYTES = 4096`（合计字节） |
| `verify_bank.py` | `len ≤ 20`；`len(input.utf8) + len(output.utf8) > 4096` → error |

中文题干下 4096 **字符** 的 UTF-8 长度可达 12 KB，比 4096 **字节** 宽松 3 倍；schema 也没有「两组字段合计」这个概念（只逐字段限长）。所以**写题库时按字节算**，别按字符算。

---

## 6. 典型工作流

```text
1. 出题人写题 → 每题标 meta.scoring = {k,a,c}
2. python score_bank.py <库>            # 看 S 与目标档位，改题或改标注
3. python score_bank.py <库> --write     # 落盘 difficulty（多选写 9）
4. python verify_bank.py <库> --expect …  # 结构 + 判题回放 + 重复 / 原子化
5. 复核人抽 15 题独立打分（口径文档 §4.2），偏差 >1 档即该题 FAIL
6. 管理端真导入一次（唯一权威的格式校验）
```

第 4 步在 CI 里建议固定成：

```bash
python tools/bank_qc/score_bank.py banks/final --strict
python tools/bank_qc/verify_bank.py banks/final --expect choice=500,fill=60,fix=61,rutuan=30,zhaosheng=30 --require-compile
```

本仓库自己的 CI（`tools-scripts` job）跑的是**反例守护**：`verify_bank.py banks/demo --no-compile` 必须**失败**；如果哪天它突然 PASS，说明门禁被放水了，CI 会直接红。这是「门禁本身也要被门禁」那条经验的具体实现——`banks/demo` 的三个文件是演示数据，本来就该 FAIL（原因见 §9）。

---

## 7. 常见问题

**`找不到 C 编译器…（本次按 warning 放行）` / `连一个最小程序都编译不了`**
本机 `PATH` 里没有 clang / gcc，或装的那个根本跑不起来（MSYS2 的 gcc 缺 `cc1` 是常见情况）。用 `--clang <路径>` 换一个，或 `--no-compile` 明确只查结构；CI 里加 `--require-compile` 让这类环境问题变成失败。**看到这两条 WARN 就说明判题材料没验过**，别当成通过。

**`--expect` 报「没找到对应题库」**
`--expect` 的名字按**文件名主干**（`choice.json` → `choice`）、`exam_id` 或**主干前缀**（`rutuan` → `rutuan-q16-19.json`）匹配。文件叫 `choice-2027.json` 就写 `--expect choice-2027=500`；一个名字命中多个文件会报「有歧义」，写全主干即可。

**回放很慢**
单次编译约 0.2–0.5 秒；121 道 fill/fix 全量回放（含撤销变体）在 8 并发下约 25–35 秒（既有实测 27.9 秒）。只想快速冒烟时用 `--limit 5`（题数 / 分档检查会自动跳过）。

**`--limit` 之后一堆「题数不对 / 某档不足」**
那是旧行为；现在 `--limit` 会自动跳过题数、`pick`、分档下限三类检查。

**自己的题库被「每档 ≥15 题」判成 FAIL**
那是**2027 交付口径**，不是格式错误。自己的小库自检时加 `--min-per-level 0`（骨架种类下限另用 `--max-skeleton-family 0` 关）。

**Windows 控制台是乱码**
输出重定向到文件时脚本会强制 UTF-8；控制台直接看时用的是系统代码页，若仍乱码可先 `chcp 65001`。

**`--write` 之后 git diff 很大**
`--write` 会统一成 `indent=2` + LF。第一次对老文件跑会出现纯格式 diff，建议单独一个提交（`chore(banks): 统一 JSON 缩进`），别和题目改动混在一起。

---

## 8. 实测样例（2027 五库，2026-11-18 记录；**历史记录，本版未复跑**）

> 下面是历史实测输出，**不是**对当前仓库的承诺；题量 / 数字以实际运行为准。

```text
$ python verify_bank.py <题库目录> --expect choice=500,fill=60,fix=61,rutuan=30,zhaosheng=30 --clang <clang> --jobs 8
期望题数：choice=500, fill=60, fix=61, rutuan=30, zhaosheng=30
[choice] 500 题  题型 single:450 multiple:50  难度 1:180 2:137 3:133 9:50
    · 原生格式：choice.json
[fill] 60 题  题型 fill:60  难度 1:26 2:19 3:15  判题回放 60/60
    · 原生格式：fill.json
[fix] 61 题  题型 fix:61  难度 1:28 2:18 3:15  判题回放 61/61
    · 原生格式：fix.json
[rutuan-q16-19] 30 题  题型 code:30  难度 3:30
    · 原生格式：rutuan-q16-19.json
[zhaosheng-b-q16-19] 30 题  题型 code:30  难度 3:30
    · 原生格式：zhaosheng-b-q16-19.json

题库 5 个，题 681 道
errors=0 warnings=0
PASS
```

```text
$ python score_bank.py <题库目录>
== choice  (…/choice.json)
   题数 500（multiple:50 single:450）
   当前难度 1:180 2:137 3:133 9:50
   应为难度 1:180 2:137 3:133 9:50
   来源 meta:500
   多选 50 题：真实 S ['2.15']，落在 [2.00, 2.20] 的 50/50
   结论：与 K/A/C 口径一致，无需改动
…
题库 5 个，题 681 道；需改 0 题；启发式估算 0 题；大题分值偏离统一口径 0 题
dry-run（未写入；加 --write 落盘）
```

`score_bank.py --strict` 在同一份题库上退出码 `0`。

---

## 9. 为什么发布包里的示例题库不 PASS

发布包自带的 `示例题库/`（`数学.json`、`英语.json`、`图片题示例.json`）只用于演示管理端流程（导入 → 场次 → 打包 → 收卷）。它由 `banks/demo/math.json`、`banks/demo/english.json` 与 `examples/image-question.json` 改名生成，题量、难度与 K/A/C 都是随手填的，**不满足出题口径**。

对它们运行：

```bash
python tools/bank_qc/verify_bank.py 示例题库 --no-compile
```

会得到 `FAIL`，这是**预期结果**（`scripts/make-release-zip.sh` 生成的 `示例题库/README.md` 也这么写），不代表工具或题库文件损坏。**失败的直接原因是结构错误，不是题量**。本机实测（v1.9.1，`--no-compile`）：

```text
题库 3 个，题 11 道
errors=24 warnings=11
FAIL
```

这 24 条 error 来自三处，逐条可核对：

| 文件 | 可核对的具体问题 |
|---|---|
| `数学.json` | 6 题**全部缺 `kp`**（原生题库必填）+ **全部缺 `body`**（报 `body 必须是对象`）→ 12 条；判断题 `m006` 的 `difficulty=4`，而判断题只允许 1/2/3 → 1 条 |
| `英语.json` | 4 题**全部缺 `kp`** + **全部缺 `body`** → 8 条。难度值都在 1–3，**没有**越界项——它是被 `kp` / `body` 判死的，不是「题量不够」 |
| `图片题示例.json` | 1 题**缺 `kp`** + **缺 `difficulty`**（报 `difficulty 必须是整数，实际 None`）+ **缺 `body`** → 3 条；选项只有 3 个（工具允许 ≥2，但不是 2027 口径的 4 选） |
| 三个文件 | 11 条 `warnings` 都是「没有 `meta.scoring`（K/A/C）」——所以 `score_bank.py --write` 会被拒绝（退出码 3），先补标注或用 `--allow-heuristic` |

**为什么 `body` 是 error**：这三个文件用的是最早的写法（顶层 `options: ["A……","B……"]`，没有 `body`）。模型层为了向后兼容仍能读它，但**门禁要求 `body.kind` 形态**——新题库请一律写 `body: {"kind": "options", "options": [{"key": "A", "text": "……"}]}`（规范见[题库 JSON 规范](../../docs/题库JSON规范.md)）。

关于**题量**：题量只在给了 `--expect` 时才判，**不加 `--expect` 时题量不产生任何错误**。所以上面这些结构错误已经足够让门禁 FAIL；「题量远低于 2027 交付口径（500/60/61/30/30）」是另一层结论，别把它当成 FAIL 的直接原因。

真正出题请按[题库 JSON 规范](../../docs/题库JSON规范.md)与[出题质检与难度口径](../../docs/出题质检与难度口径.md)编写。

---

## 10. 与 `.agents/skills/question-bank-qc` 的关系

仓库里有两套题库相关资产，职责不同，**不要理解成二选一，也不要当成同一套实现**：

| 资产 | 角色 | 会不会改题库 | 依据 |
|---|---|---|---|
| `docs/出题质检与难度口径.md` | **口径的唯一来源** | 不会 | 人读的规则 |
| `tools/bank_qc/`（本目录） | **通用执行器**：结构 / 判题 / 难度 / 分值；题量由 `--expect` 传入 | `verify_bank.py` 只读；`score_bank.py` 仅 `--write` 时写 | 上面那份文档 |
| `.agents/skills/question-bank-qc/` | **生产与复核流程**（双盲、抽样、返工、门禁复核）+ 一套**针对 2027 五库**的专用脚本 | **会**：`pretty_c.py --write` / `score.py --write` 真写盘（`io.open(..., "w")`）；流程上要求**先 dry-run、由修复人执行** | 流程文档 + 该 skill 自己的阈值表 |

关键区别：

- 本目录的脚本是**参数化**的：同一套代码可以对任意题库跑，题量由 `--expect` 给；
- skill 里的 `scripts/verify.py` / `scripts/score.py` 是题库仓的**逐字拷贝**：`verify.py` 把题量写死在文件顶部的 `FILES`（`choice=500, fill=60, fix=61, rutuan=30, zhaosheng=30`），只适用于那 5 个库；
- skill 另有 `qc.py`（V1–V8 补充门禁）、`check_choice.py`、`fill_inject.py`、`pretty_c.py`、`admin_validate.py`，这些**本目录没有**；反过来，本目录的 `--expect` / `--limit` / `--json` 这类通用参数，skill 脚本也没有；
- 阈值、题量分布、K/A/C 权重有分歧时，以 `docs/出题质检与难度口径.md` 为准；需要「跑门禁、给结论」时用本目录的脚本；需要「组织两个人独立出题 / 复核、怎么抽样」时看 skill。

> 不要把 skill 的 `verify.py` 与本目录的 `verify_bank.py` 当成同一个文件：两者字节数、参数、题量假设都不同（本目录脚本约 90 KB / 28 KB，skill 的约 15 KB / 5 KB）。
>
> **`.agents/` 不随发布包分发**，所以本文件不提供指向它的相对链接（发布包构建会判死链）。上面出现的这些路径只能作为行内代码阅读，不能在发布包里点开。

---

## 11. 能力边界一页纸

| 能 | 不能 |
|---|---|
| 挡结构 / 字段 / 答案形状错误 | 判断题目内容、选项表述、陷阱设计是否正确 |
| 实跑 fill / fix 判题材料（官方答案必过、原始 / 撤销必败） | 判断「边界用例是否真的覆盖边界」 |
| 查逐字重复、骨架族、跨库逐字重复 | 查语义级重复 / 同任务换题面 |
| 按 K/A/C 重算 S 并比对 `difficulty` | 判断 K/A/C 标注本身是否准确 |
| 查排版硬伤（单行程序、行内程序、围栏、TAB） | 评判排版是否好看 |
| 按 `--expect` 校验题量与分布 | 替代管理端导入（导入才是唯一权威） |
| 按 4096 字节合计上限校验 `samples` | 保证 `samples` 的输入输出内容正确 |
