# 题库 JSON 规范

> **代码片段约定（S-01）**：题面中的 C 代码片段默认已包含 `<stdio.h>`、`<string.h>`、`<stdlib.h>`；判题/编译时按此约定注入头文件。若某题选项含“编译错误”，题干必须写明该约定。

> 适用版本：**v1.4.11**。格式或校验规则变化必须同步更新 `schemas/` 与 `tools/bank_qc`。

本文是**题库文件的唯一格式说明**。手写题库、用脚本生成题库、或让其它系统导出题库时照抄即可。
`schemas/exam-bank.schema.json` 是本规范的机器可读版本（由 pydantic 模型生成，
`admin/tests/test_schemas.py` 防漂移）。

配套文档：

- [出题质检与难度口径](出题质检与难度口径.md) —— 题量、难度公式、阈值、返工判定；
- [教师使用手册](教师使用手册.md) —— 导入、组场次、打包、判分的操作步骤；
- [bank_qc 工具说明](../tools/bank_qc/README.md) —— 导入前自检怎么跑、能查什么、查不了什么。

> **先分清两层校验**（最容易混淆的一点）：
> **产品层**（管理端导入 / 打包前）只认格式与自洽性，过了就能导入；
> **项目口径层**（`tools/bank_qc/`）在此之上加题量、难度分布、重复、排版等交付要求。
> 一份题库能导入 ≠ 可以交卷。两层的完整清单见 §3。

> 仓库里的样例：`banks/demo/demo-showcase.json`（四种题型，可直接导入）、
> `banks/demo/math.json` / `english.json`（发布包里的「示例题库」由它们生成）、
> `examples/image-question.json`（带图题）。

---

## 0. 两种格式，选一种

| 格式 | 长什么样 | 什么时候用 |
|---|---|---|
| **原生格式** | 一个 `.json` 同时含题目与答案（`exam_id` + `questions`） | 手写、脚本生成、其它系统一次导出；**代码简答题只能用这种** |
| **题目/答案分离**（`format_version: 2`） | `X.questions.json` + `X.answers.json` 两个文件 | 已有题库；出题人与审题人分工、答案单独保管 |

两种都能直接导入，导入后都变成同一个 `ExamBank` 模型，行为一致。
**同一门课不要两种格式混用**，否则会出现两个题库。

---

## 1. 原生格式

```json
{
  "exam_id": "demo-one",
  "title": "单题示例",
  "pick": 1,
  "questions": [
    {
      "id": "s001",
      "type": "single",
      "stem": "1 + 1 = ?",
      "body": {"kind": "options", "options": [{"key": "A", "text": "1"}, {"key": "B", "text": "2"}]},
      "answer": {"kind": "single", "answer": "B"},
      "score": 2,
      "difficulty": 1,
      "kp": "1.1"
    }
  ]
}
```

### 1.1 顶层字段

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `exam_id` | 字符串 | 是 | 题库标识，**会用作文件名**。规则见下 |
| `title` | 字符串 | 是 | 界面与学生端显示的科目名，可任意中文 |
| `pick` | 整数 | 是 | 从本题库抽多少题，必须 `0 ≤ pick ≤ 题目数`；`0` ＝ 空卷（需教师确认） |
| `questions` | 数组 | 是 | 题目列表，`id` 在题库内唯一 |

`exam_id` 限制（导入时逐条校验，中文允许）：不能为空且 ≤ 64 字符；不能含
`/ \ : * ? " < > |`；不能含空白或控制字符；不能以 `.` 开头或结尾；不能是
Windows 保留名（`CON` / `NUL` / `COM1-9` / `LPT1-9` 等）。
它会被拼进临时文件路径，所以规则是**按危险特征拒绝**，不是 ASCII 白名单。

### 1.2 题目字段

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `id` | 字符串 | 是 | 题库内唯一。**试卷指纹只哈希 id**，改 id 等于换了一份卷子 |
| `type` | 字符串 | 是 | `single` / `multiple` / `judge` / `fill` / `fix` / `essay` / `code` |
| `stem` | 字符串 | 是 | 题干，支持 `\n` 换行与一小部分 Markdown（见 1.10） |
| `body` | 对象 | 否 | 正文形态，见 1.6；省略时按旧格式用顶层 `options` 渲染 |
| `options` | 字符串数组 | 否 | 旧格式选项（纯文本）。新题请用 `body.kind=options` |
| `answer` | 对象 | 否 | 标准答案，形状见 1.4。**打包时剥离**，永不进学生端 |
| `judge` | 对象 | 否 | fill/fix 的判题材料，见 1.5。**打包时剥离**。注意与 `type: "judge"`（判断题）区分 |
| `kp` | 字符串 | 否 | 知识点编号，形如 `"1.1"`（本项目范围 1.1–12.9） |
| `difficulty` | 整数 | 否 | 难度；格式层允许 `0–255`，本项目口径见 1.8；`0` 与缺省一律视为**未标注** |
| `score` | 整数 | 否 | 分值，默认 `2`，范围 `0–1000` |
| `code_language` / `code_languages` | 字符串 / 字符串数组 | 否 | 仅 `type=code` 用，见 1.7；**二者不共存** |
| `meta` | 对象 | 否 | **出题/质检专用**，教师端导入时被忽略，见 1.3 |

题型别名会在导入与 builder 加载时自动迁移（`multi`→`multiple`、`blank`/`blanks`→`fill`、
`fix_code`→`fix`），新写题库请直接用标准值。

### 1.3 `meta`：只给写题和质检用，系统不读

`meta` 里放 **K/A/C 评分标注**，供 `tools/bank_qc/score_bank.py` 计算难度：

```json
"meta": {"scoring": {"k": 2, "a": 2, "c": 2}}
```

必须清楚三件事：

1. **教师端导入时会忽略它。** `admin/app/core/models.py` 的 `Question` 模型**没有**
   `meta` 字段，pydantic v2 默认 `extra="ignore"`，所以多出来的键被**静默丢弃**：
   不报错、不保存、不参与任何计算。实测：把一个带 `meta.scoring` 的题喂给
   `ExamBank.model_validate()`，解析成功，但 `model_dump()` 里已经没有 `meta`。
2. **它不作为判分或抽题依据。** 判分读 `answer`，抽题读 `difficulty` 与场次配置；
   把难度"写在 meta 里"而不写 `difficulty`，等于没标。
3. **写坏了不会当场报错。** 目前没有导入层校验 `meta` 的形状，只有
   `verify_bank.py` / `score_bank.py` 会检查 `k/a/c` 是否为整数且在规定区间内。

> 结论：`meta` 可以留、建议留（复核与追溯都要用），但**真正生效的永远是 `difficulty`**。

### 1.4 `answer` 的形状

`answer` 必须是**对象**，`kind` 说明答案类型，`answer` 装具体内容：

| 题型 | `answer.kind` | 正确写法 |
|---|---|---|
| `single` | `single` | `{"kind": "single", "answer": "B"}` |
| `judge` | `single` | `{"kind": "single", "answer": "A"}`（选项通常是「对 / 错」） |
| `multiple` | `multi` | `{"kind": "multi", "answer": ["A", "C"]}`（**集合相等**，顺序无关） |
| `fill` | `blanks` | `{"kind": "blanks", "answer": [{"no": "①", "answer": "a + b"}, …]}` |
| `fix` | `fixes` | `{"kind": "fixes", "answer": [{"line": 3, "wrong": "原文整行", "answer": "改后整行"}, …]}` |
| `code` | `rubric` | `{"kind": "rubric", "total": 15, "points": […], "reference": "…", "samples": […], "note": "…"}` |
| `essay` | 可省略 / `null` | 人工给分，不需要标准答案 |

**判分比较方式**（`admin/app/core/grading.py`）：

| 题型 | 比较规则 |
|---|---|
| `single` / `judge` | 字母相等（默认忽略大小写与空白，可在界面改） |
| `multiple` | 学生答案的**字母集合**与标准答案集合相等 |
| `fill` | 逐空比对；默认整题全对才得分，可切「按项给分」 |
| `fix` | 按 `line` 对应到学生改的那一行，比 `answer` 整行文本 |
| `essay` / `code` | **只能人工给分**，`answer` 不参与判分 |

#### 大题（`type: "code"`）的 `rubric`

```json
"answer": {
  "kind": "rubric",
  "total": 15,
  "note": "按 3 个得分点给分；只输出多余提示语扣分。",
  "points": [
    {"text": "用 scanf 正确读入 n 与 n 个整数，数组开得足够大。", "score": 5},
    {"text": "以第一个元素为初值扫描求最大值，全负数时也正确。", "score": 5},
    {"text": "再次扫描统计出现次数，输出格式“最大值 次数”。", "score": 5}
  ],
  "reference": "int main(void)\n{\n    …\n}\n",
  "samples": [{"input": "6\n3 8 8 1 8 4\n", "output": "8 3"}]
}
```

| 键 | 必填 | 说明 |
|---|---|---|
| `total` | 是 | 本题总分，**必须等于 `score`** |
| `points` | 是 | 评分点数组，每项 `{text, score}`；各项 `score` 之和必须等于 `total` |
| `reference` | 是 | 参考答案（可编译的 C 程序），**只给教师** |
| `samples` | 是 | 输入/输出样例，每项 `{input, output}`；**只给教师** |
| `note` | 否 | 给阅卷人的口径说明 |

> `reference` 与 `samples` 属于**教师端材料**：它们装在 `answer` 里，所以打包时
> 随 `answer` 一起被剥离（`banks_io.write_temp_banks` 先把 `answer`/`judge` 置空，
> builder 再剥一遍），学生端拿不到。系统**不会**执行 `reference`，代码题始终人工评分。

> ⚠ **不要把答案写成纯字母**。`"answer": "B"`（或 `"AC"`）这种旧写法：导入不报错、
> 打包不报错、预检也不会报"没有标准答案"，但**判分时标准答案取到的是空串**——
> 每份卷子都被判错，成绩表「标准答案」列也是空的。原因：判分读的是
> `answer["answer"]`，而顶层 `answer` 是字符串时取不到键。
> **导入层目前仍不拦截**（`answer_letters()` 能识别旧写法），`verify_bank.py` 会报
> `answer 必须是对象`——请在源头就按上表写。

答案字母必须落在该题的选项键里。写成 `"E"` 而选项只有 A/B 会被拒绝（原生导入走
`validate_bank_answers`）：
`题库 demo 的题 m001：答案 ['E'] 不在选项 ['A', 'B'] 中`。

### 1.5 `judge`：判题材料（只用于出题回放）

`judge` 描述"这段代码怎么跑、跑什么用例"，供出题人/复核人**在题库侧回放**：

```json
"judge": {
  "entry": "fun",
  "harness": "int main(void){ int a, b; if(scanf(\"%d %d\", &a, &b) != 2) return 0; printf(\"%d\", fun(a, b)); return 0; }",
  "tests": [
    {"input": "17 5", "expected": "302", "note": "普通值"},
    {"input": "7 7", "expected": "100", "note": "边界：a 整除 b"},
    {"input": "0 3", "expected": "3", "note": "边界：a 为 0"}
  ]
}
```

| 键 | 必填 | 说明 |
|---|---|---|
| `entry` | 是 | 被调用的函数名，供回放脚本定位 |
| `harness` | 是 | 带 `main` 的驱动程序：读输入、调 `entry`、打印结果 |
| `tests` | 是 | 用例数组，**≥ 3 条，且至少 1 条边界**（0 / 负数 / 空 / 单元素 / 重复 / 极值） |

每条用例：`input`（喂给 stdin 的字符串，可为空串）、`expected`（期望 stdout）。
旧文档里的 `output` 写法也接受（工具两者都认，`expected` 优先）；**建议统一写 `expected`**。

必须明白的两点：

1. **系统不执行 `judge`。** `admin` 侧只把它当不透明 JSON 存放并在打包时剥离
   （`Question.judge` 类型是 `serde_json::Value` / `Any`），判分只看 `answer`。
   `judge` 的价值在于：`verify_bank.py` 能用 clang 把官方答案回放一遍，证明
   "题目与用例自洽"。
2. **fill/fix 必须可编译回放**：官方答案回填后 **全部通过**；fix 的原始代码
   **至少一条失败**（推荐全部失败）；撤销任一修改点后 **至少一条失败**（否则那处错"改没改都得分"）。

### 1.6 `body` 的四种形态

#### `text` —— 只有题干

```json
{ "kind": "text" }
```

缺省就是它。老题库没有 `body` 字段时也落到这里，渲染时回退到顶层 `options`。
**代码简答题只能用这个形态。**

#### `options` —— 选择题

```json
{
  "kind": "options",
  "options": [
    { "key": "A", "text": "1" },
    { "key": "B", "text": "2" }
  ]
}
```

- `key` 顺序即显示顺序，**最多 26 个**；建议从 A 起连续（答题卡用字母拼接表示多选答案，
  超过 26 个会出现 `"27"` 这种多字符键，与 `"2"+"7"` 无法区分）；
- 选项文本可多行、可放代码块（整块可点选），**不支持图片**；
- 本项目选择题统一 4 个选项、文本互不相同（`verify_bank.py` 会把不统一的报出来）。

#### `template` —— 填空题（带占位符的代码模板）

```json
{
  "kind": "template",
  "template": "int add(int a, int b)\n{\n    return ____①____;\n}",
  "blank_count": 1
}
```

- 占位符写成 `____①____`、`____②____`…（① 到 ⑩）；
- **占位符个数 = `blank_count` = `answer.answer` 的条数**，导入时逐项核对；
- 本项目填空统一 3 空。

#### `code` —— 改错题（逐行代码，**不是**代码简答题）

```json
{
  "kind": "code",
  "code": ["int sq(int x)", "{", "    return x * 2;", "}"],
  "blank_count": 1
}
```

- 行号从 **1** 开始，空行也占一个元素；
- `answer` 里的 `line` 指向这里第几行，`wrong` 必须与该行原文**逐字相同**，
  `answer` 是**改动后的整行**；
- 改正项条数 = `blank_count`；多条改正项不得指向同一行；
- 本项目改错统一 2 处错。

> **注意**：`body.kind = "code"` 是"改错题的逐行代码"；代码简答题是独立题型
> `type: "code"`，`body` 仍用 `text`（见 1.7）。混用会被拒：
> `代码简答题必须使用 body.kind=text，不能用改错题的 code body`。

### 1.7 代码简答题（`type: "code"`）

````json
{
  "id": "q16",
  "type": "code",
  "code_languages": ["c"],
  "stem": "给定 n 个整数，求最大值及其出现次数。\n\n样例输入：\n6\n3 8 8 1 8 4\n\n样例输出：\n8 3",
  "body": { "kind": "text" },
  "difficulty": 3,
  "score": 15,
  "kp": "6.2",
  "answer": { "kind": "rubric", "total": 15, "points": [ … ], "reference": "…", "samples": [ … ] }
}
````

语言声明规则：

| 目标 | 写法 |
|---|---|
| 只允许 1 种 | `"code_language": "python"` |
| 允许 2–8 种 | `"code_languages": ["c", "cpp", "python"]` |
| 不限制 | 两个都不写（`"code_languages": []` 等价于没写，打包时该键会被删掉） |
| ❌ 两个都写 | 导入/打包直接报错：`code_language 与 code_languages 不能同时填写，二者只写一个` |

- 支持 11 种（大小写不敏感，落库前转小写）：`python` / `c` / `cpp` / `java` / `rust` /
  `javascript` / `typescript` / `go` / `sql` / `bash` / `text`；**最多 8 个**；
- **大题必须写 `code_languages: ["c"]`**（本项目口径，`verify_bank.py` 默认按此检查）。
  写 `code_language: "c"` 或 `code_languages: ["cpp"]` 都会被判 error；
- 学生端行为：写 1 种时语言框灰色只读；写多种时只能从中选；都不写时列出全部 11 种并提示
  「以考场老师通知为准」；学生自选语言写进答题卡 `chosen_code_languages`，**只是阅卷参考**；
- 代码题**不自动判分**，由教师按 `rubric` 人工给分；
- 含代码题的包是容器 v8，要求学生端 **v1.1.1+**；语言下拉框与「自动识别语言」按钮需 **v1.3.0+**；
- 答案原样保存在答题卡 `answers` 字符串里：不 trim、不换缩进、不截断；
  **单题 ≤ 1 MiB，整卡答案合计 ≤ 16 MiB**。

### 1.8 `difficulty`：难度怎么填

格式层允许 `0–255` 的整数，**本项目口径**（详见[出题质检与难度口径](出题质检与难度口径.md)）是：

| 题型 | 允许值 | 说明 |
|---|---|---|
| `single` / `judge` / `fill` / `fix` / `essay` | `1` / `2` / `3` | 由 `S = 0.35K+0.45A+0.20C` 与 1.90 / 2.55 两个阈值决定 |
| `multiple` | **`9`** | 哨兵值：多选不参与 1–3 统计，真实难度由结构统一保证（S ∈ [2.00, 2.20]） |
| `code`（大题） | **`3`** | 本项目大题统一难度 3、统一 15 分 |

三件必须知道的事：

1. **`0` 与缺省 = 未标注**。设置「平均难度区间」的题库，所有题都必须标 1–255，
   否则保存/打包会被拦下。
2. **`9` 只是本项目的约定，系统并不特殊对待它。** 抽题时 `9` 就是一个普通难度值，
   会和其他题一起参与**平均难度**的计算。所以：**含多选题的库，不建议设平均难度区间**——
   区间设得低时，可行组合会明显变少，极端情况下直接不可行（报「区间不可行」）。
   确需设区间时，要么把多选单独成库，要么把上限放宽到能容纳 `9` 的范围。
   本库实测：500 题均值 2.61（450 道 1–3 + 50 道 9）。
3. **难度不参与单题过滤**。「平均难度区间」约束的是抽出的 N 题的**平均值**，
   不会对单题设上下限——这是刻意设计，请勿要求改成逐题过滤。

### 1.9 `score`：分值

- 默认 `2`，范围 `0–1000`；**本项目要求每题显式写**（不写时产品按 2 分导入，
  `verify_bank.py` 只给 WARN，因为"总分多少"不该靠数一遍才知道）；
- 同一题库内同一题型建议统一（本项目：选择 2 分、多选 4 分、填空 4 分、改错 4 分、大题 15 分）；
- 大题 `score` 必须与 `answer.total` 一致，且 `answer.points` 各项之和等于它；
- 总分 = 各题 `score` 之和。`title` 里的「30 题／450 分」只是给人看的，**系统不读**；
  写错不会影响判分，但会误导建场次的老师——`verify_bank.py` 会对不上的数字给 WARN
  （本库 2026-11-18 就出现过标题写 440、实际 450 的一处，已改）。

### 1.10 题干支持的 Markdown 子集

只解析下面这些；其余语法按纯文本显示（不报错、不 panic）。

| 语法 | 写法 | 说明 |
|---|---|---|
| 围栏代码块 | ` ```c … ``` ` | 语言标记可省略；**未闭合**按普通文本 |
| 行内代码 | `` `int x` `` | 等宽 + 浅底 |
| 加粗 | `**重点**` | 缺粗体字体时退化为强调色 |
| 标题 | `# ` / `## ` / `### ` | `#` 后必须有空格 |
| 无序列表 | `- 项`（也认 `*` `+`） | 一级 |
| 有序列表 | `1. 项`（也认 `1)`） | 一级 |
| 引用 | `> 文字` | 灰底块 |
| 表格 | 竖线表格 + 分隔行 | 见下 |
| 图片 | `![说明](data:image/png;base64,…)` | 见 1.11 |

**不支持**：链接、斜体、删除线、嵌套列表、HTML。

表格必须有表头行与分隔行，否则按普通文本：

```text
| 变量 | 类型 | 说明 |
|:-----|:----:|-----:|
| x    | int  | 计数 |
```

- `:` 决定对齐（左/中/右）；列多、表宽时学生端底部出现横向滚动条；
- 单元格里的 `\|` 表示一个竖线字符。

**代码排版（硬性）**：题干里的程序必须用多行围栏块，禁止压成一行：

- `int main` 程序必须换行、4 空格缩进、`{` / `}` 独占一行；
- 单行围栏块只允许短表达式（≤ 40 字符）；
- 程序片段禁止用行内代码（反引号）书写。

### 1.11 题干内嵌图片

只接受 **data URI**（离线自包含，不联网、不支持外部 URL）：

```text
![示意图](data:image/png;base64,iVBORw0KGgo…)
```

| 项 | 要求 |
|---|---|
| 位置 | 只能在 `stem` 里，且**独占一行**（不做图文混排）；选项里写 `![](...)` 按普通文字显示 |
| 格式 | `image/png` / `image/jpeg`（`image/jpg` 也认，推荐 jpeg） |
| 建议值 | ≤ 1.5 MB、≤ 1200 px——超了导入时**只提示、不阻止** |
| 学生端硬上限 | 4 MB、4096 px——超了直接显示 `[图片无法显示]` |

生成方式：管理端 **[题库]** →【插入图片】→ 选图 → 检查通过后片段进剪贴板；
批处理可用 `tools/embed-image.py`（只支持 PNG/JPEG，>1.5 MB 直接报错）。

导入时的提示语（带题号前缀，例如 `第 3 题：`）：

- `图片「示意图」1840 KB，超过建议上限 1500 KB，请压缩后再打包`
- `图片「示意图」尺寸 2400×1600，超过建议上限 1200px，请缩小后再打包`
- `图片「示意图」的 base64 已损坏，学生端会显示不出来`
- `图片「示意图」不是有效的 PNG/JPEG，学生端会显示不出来`

### 1.12 完整示例（格式演示：五种自动判分题型 + 一道大题）

> 下面这份是**格式**演示（每题该有哪些键、形状怎么写），不是按 2027 数量口径出的成品卷：
> 它只有 5 道题、填空 1 空、单选 1 道。直接拿它跑 `verify_bank.py` 会在
> 「每档 ≥15 题」「填空统一 3 空」等**项目口径**上报错——这是预期的，别把它当成
> 格式错误。要照抄结构，请配合[出题质检与难度口径](出题质检与难度口径.md) §1 的题量表。

```json
{
  "exam_id": "demo-full",
  "title": "题型示例",
  "pick": 5,
  "questions": [
    {
      "id": "q1", "type": "single", "stem": "1 + 1 = ?", "kp": "1.1", "difficulty": 1, "score": 2,
      "body": {"kind": "options", "options": [
        {"key": "A", "text": "1"}, {"key": "B", "text": "2"},
        {"key": "C", "text": "3"}, {"key": "D", "text": "4"}]},
      "answer": {"kind": "single", "answer": "B"},
      "meta": {"scoring": {"k": 1, "a": 1, "c": 1}}
    },
    {
      "id": "q2", "type": "multiple", "stem": "选出所有偶数", "kp": "1.1", "difficulty": 9, "score": 4,
      "body": {"kind": "options", "options": [
        {"key": "A", "text": "2"}, {"key": "B", "text": "3"},
        {"key": "C", "text": "4"}, {"key": "D", "text": "5"}]},
      "answer": {"kind": "multi", "answer": ["A", "C"]},
      "meta": {"scoring": {"k": 3, "a": 2, "c": 1}}
    },
    {
      "id": "q3", "type": "fill", "stem": "补全下面的函数", "kp": "1.2", "difficulty": 1, "score": 4,
      "body": {"kind": "template",
               "template": "int add(int a, int b)\n{\n    return ____①____;\n}\n",
               "blank_count": 1},
      "answer": {"kind": "blanks", "answer": [{"no": "①", "answer": "a + b"}]},
      "judge": {"entry": "add",
                "harness": "int main(void){int a,b;if(scanf(\"%d %d\",&a,&b)!=2)return 0;printf(\"%d\",add(a,b));return 0;}",
                "tests": [{"input": "1 2", "expected": "3", "note": "普通值"},
                          {"input": "0 0", "expected": "0", "note": "边界：0"},
                          {"input": "-3 5", "expected": "2", "note": "边界：负数"}]},
      "meta": {"scoring": {"k": 1, "a": 1, "c": 2}}
    },
    {
      "id": "q4", "type": "fix", "stem": "找出并改正下面代码的错误（2 处）", "kp": "1.2", "difficulty": 2, "score": 4,
      "body": {"kind": "code",
               "code": ["int sq(int x)", "{", "    int y = x * 2;", "    return y - 1;", "}"],
               "blank_count": 2},
      "answer": {"kind": "fixes", "answer": [
        {"line": 3, "wrong": "    int y = x * 2;", "answer": "    int y = x * x;"},
        {"line": 4, "wrong": "    return y - 1;", "answer": "    return y;"}]},
      "judge": {"entry": "sq",
                "harness": "int main(void){int n;if(scanf(\"%d\",&n)!=1)return 0;printf(\"%d\",sq(n));return 0;}",
                "tests": [{"input": "3", "expected": "9", "note": "普通值"},
                          {"input": "0", "expected": "0", "note": "边界：0"},
                          {"input": "-4", "expected": "16", "note": "边界：负数"}]},
      "meta": {"scoring": {"k": 1, "a": 3, "c": 2}}
    },
    {
      "id": "q5", "type": "code", "code_languages": ["c"],
      "stem": "输入 n 与 n 个整数，输出最大值及其出现次数。\n\n样例输入：\n6\n3 8 8 1 8 4\n\n样例输出：\n8 3",
      "body": {"kind": "text"}, "kp": "6.2", "difficulty": 3, "score": 15,
      "answer": {"kind": "rubric", "total": 15,
                 "points": [{"text": "正确读入", "score": 5},
                            {"text": "求最大值（含全负数）", "score": 5},
                            {"text": "统计次数并按格式输出", "score": 5}],
                 "reference": "int main(void)\n{\n    …\n}\n",
                 "samples": [{"input": "6\n3 8 8 1 8 4\n", "output": "8 3"}]},
      "meta": {"scoring": {"k": 3, "a": 3, "c": 2}}
    }
  ]
}
```

导入它应看到「题型示例 · 5 题」。

> 这份示例每题的 `meta.scoring` 与 `difficulty` 是**对得上**的（S = 1.00 / 2.15(多选) /
> 1.20 / 2.10 / 2.80），可以用 `score_bank.py` 复算验证：
> `python score_bank.py <把示例另存到的目录>` 应报「需改 0 题」。

---

## 2. 题目/答案分离格式（`format_version: 2`）

### 2.1 文件配对

```text
数学/choice.questions.json   ← 题目
数学/choice.answers.json     ← 答案
```

- 同名、后缀不同即配对（下载重复带来的 `(1)` 后缀会被忽略）；只选题目文件时程序会在
  **同目录**找 `X.answers.json`；
- 两个文件也可一次全选；同一批里两个目录的重名题库**不会**被合并；
- **该格式不支持 `code` 代码简答题**，代码题请用原生 JSON；
- 该格式**没有 `score`**（按默认 2 分导入）、**没有 `meta`**、`kp` 可省略。

### 2.2 题目文件

```json
{
  "format_version": 2,
  "type": "choice",
  "role": "questions",
  "count": 2,
  "questions": [
    {
      "id": "CH-001",
      "kp": "1.1",
      "difficulty": 1,
      "stem": "在常见的 32 位平台上，int 类型占（ ）个字节。",
      "content": { "kind": "options", "options": { "A": "4", "B": "2", "C": "8", "D": "1" } }
    }
  ]
}
```

| 字段 | 说明 |
|---|---|
| `format_version` | 固定 `2` |
| `type` | `choice` / `fill_blank` / `fix_code` |
| `role` | 固定 `questions` |
| `count` | 必须等于 `questions` 的实际条数 |
| `content.kind` | `options` / `template` / `code`，与 `type` 一一对应 |
| `content.blank_count` | `template` / `code` 必填，且与占位符 / 改正项条数一致 |

外部格式里选项写成**对象** `{"A": "4", …}`（导入时转成有序列表），键必须从 A 开始连续。

### 2.3 答案文件

```json
{
  "format_version": 2,
  "type": "choice",
  "role": "answers",
  "count": 1,
  "answers": [
    { "id": "CH-001", "verdict": { "kind": "single", "answer": "A" }, "judge": null }
  ]
}
```

| `verdict.kind` | 对应题型 | `answer` 的形状 |
|---|---|---|
| `single` | 单选 | `"A"` |
| `multi` | 多选 | `["A", "C"]` |
| `blanks` | 填空 | `[{"no": "①", "answer": "a + b"}]` |
| `fixes` | 改错 | `[{"line": 3, "wrong": "原文整行", "answer": "改后整行"}]` |

`judge`（可空）：形状同 1.5；旧示例里的用例键写成 `output` 也能导入（系统不解析，只存）。

> **`difficulty == 0` 表示多选题**——这是旧格式的题型标记（原生格式里多选写 `9`）。
> 它必须与 `verdict.kind == "multi"` 互相印证，否则报
> `difficulty=1（单选）与答案 kind='multi' 矛盾`。
> 导入后 `difficulty` 原样保留，`0` 仍按"未标注"参与平均难度计算。
> 迁移到原生格式时，请把多选显式改成 `difficulty: 9`。

---

## 3. 强制执行的校验（产品层）

两种格式跑同一套规则：外部格式在导入时逐条检查；原生格式在**导入**时完整解析一遍
（`ExamBank.model_validate_json`）。

**改完源文件之后要重新导入或重开工程**，否则打包用的是导入时载入内存的那一份：
- **重开工程**：按 `bank_paths` 记的路径重新读源文件并重新校验（`load_bank_from_path`）；
- **打包前**：只对内存里的题库再查一遍答案自洽（`ProjectState.validate()` →
  `validate_bank_answers`），**不会**重读磁盘。

下面引用的都是**真实文案**（pydantic 的 `ValidationError` 会把它包在更长的多行信息里）。

| 规则 | 不满足时的提示 |
|---|---|
| 顶层必须含 `exam_id` + `questions` | `xxx.json: 无法识别的格式。` |
| JSON 必须是 UTF-8 | `xxx.json: 不是合法的 UTF-8 JSON。` |
| `exam_id` 字符 / 长度 / 保留名 | `exam_id 不能包含路径分隔符或 Windows 禁用字符（当前 'a/b'）` |
| `pick ≤ 题量` | `题库 math 抽题数 20 超过题量 10` |
| 题目 id 唯一 | `题库 math 存在重复题目 ID` |
| `type` 在白名单内 | `不支持的题型：'singel'；支持 ['code', 'essay', 'fill', 'fix', 'judge', 'multiple', 'single']；旧别名 multi→multiple、blank→fill、fix_code→fix` |
| `body.kind` 与题型匹配 | `代码简答题必须使用 body.kind=text，不能用改错题的 code body`；`kind=options 时 options 不能为空`；`kind=template 时 blank_count 必须 ≥ 1`；`kind=code 时 code 不能为空` |
| 选项 ≤ 26 | `选项不能超过 26 个（A–Z）` |
| 选项键不重复 | `选项键重复` |
| 答案字母在选项内 | `题库 math 的题 m001：答案 ['E'] 不在选项 ['A', 'B'] 中` |
| 答案无重复字母 | `题库 math 的题 m002：答案里有重复字母 ['A', 'A']` |
| 语言字段不共存 | `code_language 与 code_languages 不能同时填写，二者只写一个` |
| 语言在白名单内 | `不支持的代码语言：'brainfuck'` |
| 语言 ≤ 8 个 | `code_languages 最多 8 个` |
| 模板占位符 = `blank_count` | `第 3 条（id=f1）: 模板里有 2 个 ____①____ 占位符，但 blank_count=3` |
| 填空答案数 = `blank_count` | `第 3 条（id=f1）: 有 2 个空答案，但 blank_count=3` |
| 改错条数 = `blank_count` | `第 4 条（id=x1）: 有 1 条改正项，但 blank_count=2` |
| 改错行号在范围内 | `第 4 条（id=x1）: 第 1 条改正项的 line=99 超出 1..8` |
| 改错 `wrong` 与原文一致 | `第 4 条（id=x1）: 第 1 条改正项的 wrong 与第 3 行原文不一致` |
| 题干非空 | `第 7 条（id=e1）: 题干为空` |
| 外部 `count` = 实际条数 | `choice.questions.json: count=500，实际 questions 有 499 条` |
| 外部 id 一一对应 | `choice.answers.json: 缺少答案：CH-042` / `多出答案：CH-999` |
| 外部 `verdict.kind` 匹配题型 | `第 1 条（id=CH-001）: 题型 choice 的答案 kind 应为 ['multi', 'single']，实际 'blanks'` |
| 外部选项键连续 | `第 1 条（id=CH-001）: 选项键应为 ['A', 'B', 'C']，实际 ['A', 'C']` |
| 外部 `difficulty=0` 与 `multi` 一致 | `第 1 条（id=CH-001）: difficulty=1（单选）与答案 kind='multi' 矛盾` |
| 同批 `exam_id` 不重名 | `choice.questions.json: 与 旧/choice.questions.json 的题库名相同（'choice'）` |

### 3.1 产品层没拦、但交付必须过的（项目口径层）

下面这些**导入不会报错**，属于"能不能交卷"的问题，由 `tools/bank_qc/verify_bank.py` 查：

| 检查 | 严重度 | 说明 |
|---|---|---|
| 题数（`--expect`） | ERROR | 2027 口径 500 / 60 / 61 / 30 / 30 |
| `kp` 是否存在且形如 `N.M`（1.1–12.9） | ERROR | 产品层允许省略 `kp`；项目要求每题都有 |
| `score` 是否显式写、在 0–1000 | WARN / ERROR | 产品层允许省略（默认 2 分） |
| `difficulty` 是否在本项目口径内（1–3、多选 9、大题 3） | ERROR | 格式层只要求 `0–255`，缺省 = 未标注 |
| `meta.scoring` 形状与 `S` 是否对得上 `difficulty` | ERROR / WARN | 小库校验；大题只查整数/范围（统一 3/15，S 仅诊断）；缺失只 WARN |
| 大题 `score=15`、`rubric` 合计、`reference`/`samples` | ERROR / WARN | 样例 <2 组只 WARN |
| `judge` 是否存在、`tests ≥ 3`、官方回放/原始必败/逐处可测 | ERROR | 需要本地 C 编译器 |
| 重复：逐字题干、骨架族 ≤4、骨架种类下限、跨库同题干 | ERROR | 文本归一，查不出语义重复 |
| 原子化：题干引用别的题 | ERROR | 上题 / 第 N 题 / 见图 … |
| 代码排版：整段压单行、行内代码写程序、围栏未闭合 | ERROR | TAB 缩进只 WARN |
| 答案位置分布、某一档 >60%、选项不是 4 个 | WARN / ERROR | 单选字母失衡是 ERROR（18%–32% 之外） |
| 多选答案 = 全部选项、改错"改前改后相同"、填空占位符乱序 | ERROR / WARN | 前两个 ERROR，乱序 WARN |

**仍然谁都不拦**（只能人看）：答案内容对不对、选项表述是否有歧义、难度标得准不准、
边界用例算不算边界、语义级重复。`verify_bank.py` 的 README 有一张"不能查"的表。

### 3.2 导入与打包：哪些字段会被剥掉

| 字段 | 导入时 | 打包时 | 说明 |
|---|---|---|---|
| `answer` | 保留 | **剥离** | 判分只在管理端用；学生端永远拿不到 |
| `judge` | 保留 | **剥离** | `Question.judge` 是 `Any` / `serde_json::Value`，产品从不解析 |
| `meta` | **丢弃** | —（已不存在） | 模型里没有这个字段，pydantic 默认 `extra="ignore"` 静默丢 |
| `difficulty` / `score` / `kp` | 保留 | **进包** | 抽题、组卷、成绩统计都靠它们 |
| `code_languages` | 保留（转小写、去重） | 进包；空数组时连键一起删 | 学生端据此决定语言下拉框 |
| 大题 `rubric` 的 `reference` / `samples` | 保留 | **随 `answer` 一起剥离** | 只在教师端可见 |

打包时会先把题库写到临时目录，**在那里就剥掉 `answer` 与 `judge`**（`banks_io.write_temp_banks`
的纵深防御），builder 再剥一遍；临时目录在打包结束（成功/失败/取消）后删除。

---

## 4. 常见错误

| 现象 | 原因 | 处理 |
|---|---|---|
| `不是合法的 UTF-8 JSON` | 存成 GBK / ANSI | 记事本「另存为 → UTF-8」；VS Code 编码 → Save with Encoding → UTF-8 |
| `无法识别的格式` | 缺 `exam_id`/`questions`，或 `format_version` 不是 2 | 对照第 1、2 节补齐 |
| `缺少对应的答案文件（X.answers.json）` | 只选了题目文件 | 把答案文件放同目录，或两个一起选 |
| `题库名相同` | 两个目录下有同名题库 | 在「确认导入」里改题库 ID |
| 选择题全被判错、标准答案列为空 | `answer` 写成了纯字母 | 改成对象，见 1.4 |
| 填空/改错判分总是 0 | `blank_count` 与占位符/改正项数量不一致 | 数一遍，两边对齐 |
| 代码题没有语言下拉框 | 学生端低于 v1.3.0 | 用 v1.3.x 的 `stub-template.exe` 重新打包 |
| 旧学生端打开代码题包提示版本不支持 | 含 `code` 题是容器 v8 | 换 v1.1.1+ 学生端 |
| 图片显示 `[图片无法显示]` | 超 4 MB / 4096 px，或 base64 被截断 | 用【插入图片】重新生成 |
| 题干里的 `![...](...)` 原样显示 | 图片没独占一行，或写在了选项里 | 挪到 `stem` 单独一行 |
| 设了平均难度区间却抽不出题 | 含多选的库把 `difficulty=9` 也算进均值 | 见 1.8 第 2 条：含多选的库别设区间 |

---

## 5. 自检清单

**格式层（导入前一定过一遍）**

- [ ] 文件是 UTF-8（无 BOM 也能读，但别用 UTF-16）；
- [ ] `exam_id` 不含路径字符、不是保留名、≤64 字符且全工程唯一；
- [ ] `pick ≤ 题量`；
- [ ] 每道题的 `type` 是七个标准值之一，`body.kind` 与题型匹配；
- [ ] **每道要自动判分的题，`answer` 是对象**（`{"kind":…,"answer":…}`）；
- [ ] 选择题答案字母 ⊆ 选项键；多选字母升序且无重复；
- [ ] 填空题占位符数 = `blank_count` = 答案条数；改错题改正项数 = `blank_count`，`wrong` 逐字一致；
- [ ] 代码题只用 `code_language` **或** `code_languages`（≤8），语言名拼写正确；大题写 `["c"]`；
- [ ] 大题 `answer.total == score`，评分点合计等于 `total`，`reference`/`samples` 齐备；
- [ ] 设了平均难度区间的题库，所有题都有 `difficulty`（1–255），且该库不含多选。

**质量层（跑工具：结构几秒，含判题回放约半分钟）**

```bash
python tools/bank_qc/score_bank.py  <题库目录> --strict
python tools/bank_qc/verify_bank.py <题库目录> --expect choice=500,fill=60,fix=61,rutuan=30,zhaosheng=30
```

（`--expect` 的名字可用文件名主干、`exam_id` 或主干前缀；上面 `rutuan` 会命中
`rutuan-q16-19.json`。没有 C 编译器时工具只报警告并跳过判题回放，CI 里想卡死
就加 `--require-compile`。）

- [ ] `score_bank.py` 退出码 0（没有难度偏差）；
- [ ] `verify_bank.py` 输出 `errors=0`、末行 `PASS`（`PASS` 只看 error；warning 要逐条解释）；
- [ ] `verify_bank.py` 的 warning 都看过了，每条都能解释（不是"忽略"）；
- [ ] 导入后双击题库行（「题库预览」），确认题干、选项、模板/代码都正常展开；
- [ ] 打包前用一张测试答题卡跑一遍判分，确认"标准答案"列不是空的。

---

## 6. 版本与兼容

| 变更 | 影响 |
|---|---|
| 无 `body` 的老题库 | 继续按 `options` 渲染，**不用改写** |
| `answer` / `judge` / `meta` | 只在题库与工程里存在；`answer` 与 `judge` 打包时剥离，`meta` 导入时即被忽略 |
| 外部格式 | 目前只支持 `format_version: 2`，其它版本明确报错 |
| `code_language`（v1.2.0+）/ `code_languages`（v1.3.0+） | 旧学生端忽略该字段，仍可作答、无高亮 |
| 答题卡 | 自 v1.3.0 起为 `EESANS02`/v2（新增 `chosen_code_languages`），可读 v1 旧卡 |

格式若有变更，会同时更新本文件、`schemas/`、[发布说明](release-notes.md) 与
[代码审阅报告](代码审阅报告.md) 对应轮次。
