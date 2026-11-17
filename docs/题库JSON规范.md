# 题库 JSON 规范（v1.3.1）

本文是**题库文件的唯一格式说明**。手写题库、或让其它系统导出题库时照抄即可。
`schemas/exam-bank.schema.json` 是本规范的机器可读版本（pydantic 生成，`test_schemas.py` 防漂移）。

> 配套样例：`banks/demo/demo-showcase.json`（四种题型，可直接导入）、
> `banks/demo/math.json` / `english.json`（发布包里的「示例题库」由它们生成）、
> `examples/image-question.json`（带图题）。

---

## 0. 两种格式，选一种

| 格式 | 长什么样 | 什么时候用 |
|---|---|---|
| **原生格式** | 一个 `.json` 同时含题目与答案（`exam_id` + `questions`） | 自己手写、其它系统一次导出 |
| **题目/答案分离**（`format_version: 2`） | `X.questions.json` + `X.answers.json` 两个文件 | 已有题库，或"出题人/审题人"分工 |

两种都能直接导入，导入后行为一致（都变成同一个 `ExamBank` 模型）。
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
      "score": 2
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

`exam_id` 限制（导入时逐条校验）：不能为空且 ≤64 字符；不能含 `/ \ : * ? " < > |`、
空白或控制字符；不能以 `.` 开头/结尾；不能是 Windows 保留名（`CON`/`NUL`/`COM1-9` 等）。中文允许。

### 1.2 题目字段

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `id` | 字符串 | 是 | 题库内唯一。**试卷指纹只哈希 id**，改 id 等于换了一份卷 |
| `type` | 字符串 | 是 | `single` / `multiple` / `judge` / `fill` / `fix` / `essay` / `code` |
| `stem` | 字符串 | 是 | 题干，支持 `\n` 换行与一小部分 Markdown（见 1.6） |
| `body` | 对象 | 否 | 正文形态，见 1.4；**省略时按旧格式用 `options` 渲染** |
| `options` | 字符串数组 | 否 | 旧格式选项（纯文本），最多 26 个 |
| `answer` | 任意 | 否 | 标准答案。**打包时剥离**，永不进学生端。形状见 1.3 |
| `judge` | 对象 | 否 | 代码题判分材料 `{entry, harness, tests}`，**打包时剥离**。注意与 `type:"judge"`（判断题）区分 |
| `kp` | 字符串 | 否 | 知识点编号，如 `"1.1"` |
| `difficulty` | 整数 | 否 | `1–255`（越大越难）；`0` 与缺省一律视为**未标注** |
| `score` | 整数 | 否 | 分值，默认 `2`，范围 `0–1000` |

旧的题型别名会在导入与 builder 加载时自动迁移（`multi`→`multiple`、`blank`/`blanks`→`fill`、
`fix_code`→`fix`），新写题库请直接用标准值。

> `difficulty` 只用于「平均难度区间」抽题：约束的是抽出的 N 题的**平均值**，
> **不会**对单题设上下限（允许个别极难/极易题）。这是刻意设计，请勿改成逐题过滤。
> 设置了区间的题库，**所有题都必须标 `difficulty`**，否则保存/打包被拦下。

### 1.3 `answer` 的形状（最容易踩的坑）

`answer` 必须是**对象**，`kind` 说明答案类型，`answer` 装具体内容：

| 题型 | 正确写法 | 判分比较方式 |
|---|---|---|
| `single` | `{"kind": "single", "answer": "B"}` | 字母相等（可忽略大小写/空白） |
| `judge` | `{"kind": "single", "answer": "A"}` | 同单选；选项通常是「对 / 错」 |
| `multiple` | `{"kind": "multi", "answer": ["A", "C"]}` | **集合相等**，顺序无关（`CA` = `AC`） |
| `fill` | `{"kind": "blanks", "answer": [{"no": "①", "answer": "a + b"}]}` | 逐空比对；`no` 只用于展示 |
| `fix` | `{"kind": "fixes", "answer": [{"line": 3, "wrong": "原文", "answer": "改后"}]}` | 按 `line` 对应到学生改的那一行 |
| `essay` | 可省略，或 `null` | 人工给分，不需要标准答案 |
| `code` | 可省略，或 `null` | 人工给分，不需要标准答案 |

> ⚠ **不要只写一个字母**。`"answer": "B"`（或 `"AC"`）这种旧写法：导入不报错、
> 打包不报错、预检也不会报「没有标准答案」，但**自动判分时标准答案取到的是空串**
> ——每份卷子都被判成错，成绩表「标准答案」列也是空的。
> v1.3.1 实测：`"answer": "B"` + 学生答 `B` → 0 分/「错」/标准答案空；对象写法 → 满分/「对」。多选同理。

字母必须落在该题的选项键里。写成 `"E"` 而选项只有 A/B 会被拒绝：
`答案 ['E'] 不在选项 ['A','B'] 中——学生永远选不到正确项`。

### 1.4 `body` 的四种形态

#### `text` —— 只有题干

```json
{ "kind": "text" }
```

缺省就是它。老题库没有 `body` 字段时也落到这里，渲染时回退到顶层 `options`。

#### `options` —— 选择题（推荐写法）

```json
{
  "kind": "options",
  "options": [
    { "key": "A", "text": "1" },
    { "key": "B", "text": "2" }
  ]
}
```

- `key` 必须是 **A、B、C… 连续**，顺序即显示顺序，**最多 26 个**；
- 答题卡用字母拼接表示多选答案，超过 26 个会出现 `"27"` 这种多字符键，与 `"2"+"7"` 无法区分；
- 选项文本可以多行、可放代码块（整块可点选），**不支持图片**。

#### `template` —— 填空题（带占位符的代码模板，旧称补全）

```json
{
  "kind": "template",
  "template": "int add(int a, int b)\n{\n    return ____①____;\n}",
  "blank_count": 1
}
```

- 占位符写成 `____①____`、`____②____`…（① 到 ⑩）；
- **占位符个数必须等于 `blank_count`**，导入时会数一遍；
- `blank_count ≥ 1`。

#### `code` —— 改错题（逐行代码）

```json
{
  "kind": "code",
  "code": ["int sq(int x)", "{", "    return x * 2;", "}"],
  "blank_count": 1
}
```

- 行号从 **1** 开始，空行也占一个元素；
- `answer` 里的 `line` 指向这里第几行，`wrong` 必须与该行原文**逐字相同**；
- 改正项条数必须等于 `blank_count`。

> **注意**：`body.kind = "code"` 是"改错题的逐行代码"，**不是**代码简答题。
> 代码简答题是独立题型 `type: "code"`，`body` 仍用 `text`（见 1.5）。

### 1.5 代码简答题（`type: "code"`）

````json
{
  "id": "code-1",
  "type": "code",
  "code_languages": ["c", "cpp", "python"],
  "stem": "请实现 `reverse(s)`：\n\n```python\ndef reverse(s):\n    ...\n```",
  "body": { "kind": "text" },
  "score": 10
}
````

语言声明规则：

| 目标 | 写法 |
|---|---|
| 只允许 1 种 | `"code_language": "python"` |
| 允许 2–8 种 | `"code_languages": ["c", "cpp", "python"]` |
| 不限制 | 两个都不写（或 `code_languages: []`，等价于没写） |
| ❌ 两个都写 | 导入/打包直接报错 |

- 支持 11 种语言（大小写不敏感，落库前转小写）：`python` / `c` / `cpp` / `java` / `rust` /
  `javascript` / `typescript` / `go` / `sql` / `bash` / `text`；**最多 8 个**（schema 对应 `maxItems: 8` + `uniqueItems`）；
- `body.kind` 必须是 `text`，写成 `code` 会被拒绝：
  `代码简答题必须使用 body.kind=text，不能用改错题的 code body`；
- `answer` 可省略；代码题**不自动判分**，由教师人工给分；
- 含代码题的包是容器 v8，要求学生端 **v1.1.1+**；语言下拉框与「自动识别语言」按钮要求 **v1.3.0+**；
- 答案原样保存在答题卡 `answers` 字符串里：不 trim、不换缩进、不截断；
  **单题 ≤ 1 MiB，整卡答案合计 ≤ 16 MiB**；
- 学生自选语言写进答题卡 `chosen_code_languages`（题号 → 语言），只作阅卷参考。

解密后取回代码：

```bash
EXAM_MASTER=<master> exam-builder verify-answer-sheet answers-2021001-1-91d70557.eea \
  --package 学生端.exe --json > result.json
```

```python
import json

data = json.load(open("result.json", encoding="utf-8"))
sheet = data["sheet"]
qids, answers = sheet["qids"], sheet["answers"]
lang = (sheet.get("chosen_code_languages") or {}).get(qids[0])
open("answer.py", "w", encoding="utf-8", newline="").write(answers[0])
```

### 1.6 题干支持的 Markdown 子集

只解析下面这些；其余语法按纯文本显示（不报错、不 panic）。

| 语法 | 写法 | 说明 |
|---|---|---|
| 围栏代码块 | ` ```c … ``` ` | 语言标记可省略；**未闭合**按普通文本 |
| 行内代码 | `` `int x` `` | 等宽 + 浅底 |
| 加粗 | `**重点**` | 优先用系统粗体字体，缺失退化为强调色 |
| 标题 | `# ` / `## ` / `### ` | `#` 后必须有空格 |
| 无序列表 | `- 项`（也认 `*` `+`） | 一级 |
| 有序列表 | `1. 项`（也认 `1)`） | 一级 |
| 引用 | `> 文字` | 灰底块 |
| 表格 | 竖线表格 + 分隔行 | 见下 |
| 图片 | `![说明](data:image/png;base64,…)` | 见 1.7 |

**不支持**：链接、斜体、删除线、嵌套列表、HTML。

表格必须有表头行与分隔行，否则按普通文本：

```text
| 变量 | 类型 | 说明 |
|:-----|:----:|-----:|
| x    | int  | 计数 |
```

- `:` 决定对齐（左/中/右）；
- 列多、表宽时学生端**底部出现横向滚动条**，可拖动；
- 单元格里的 `\|` 表示一个竖线字符。

### 1.7 题干内嵌图片

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

生成方式：管理端 **[题库]** →【插入图片】→ 选图 → 检查通过后片段进剪贴板。
批处理可用 `tools/embed-image.py`（只支持 PNG/JPEG，>1.5 MB 直接报错）。

导入时的提示语（带题号前缀，例如 `第 3 题：`）：

- `图片「示意图」1840 KB，超过建议上限 1500 KB，请压缩后再打包`
- `图片「示意图」尺寸 2400×1600，超过建议上限 1200px，请缩小后再打包`
- `图片「示意图」的 base64 已损坏，学生端会显示不出来`
- `图片「示意图」不是有效的 PNG/JPEG，学生端会显示不出来`

### 1.8 完整示例

```json
{
  "exam_id": "demo-full",
  "title": "五题型示例",
  "pick": 5,
  "questions": [
    {
      "id": "q1", "type": "single", "stem": "1 + 1 = ?",
      "body": {"kind": "options", "options": [{"key":"A","text":"1"},{"key":"B","text":"2"}]},
      "answer": {"kind": "single", "answer": "B"}, "score": 2, "difficulty": 1
    },
    {
      "id": "q2", "type": "multiple", "stem": "选出所有偶数",
      "body": {"kind": "options", "options": [{"key":"A","text":"2"},{"key":"B","text":"3"},{"key":"C","text":"4"}]},
      "answer": {"kind": "multi", "answer": ["A","C"]}, "score": 4, "difficulty": 2
    },
    {
      "id": "q3", "type": "fill", "stem": "补全下面的函数",
      "body": {"kind": "template", "template": "int add(int a,int b){ return ____①____; }", "blank_count": 1},
      "answer": {"kind": "blanks", "answer": [{"no":"①","answer":"a + b"}]}, "score": 3, "difficulty": 2
    },
    {
      "id": "q4", "type": "fix", "stem": "找出并改正下面代码的错误",
      "body": {"kind": "code", "code": ["int sq(int x)","{","    return x * 2;","}"], "blank_count": 1},
      "answer": {"kind": "fixes", "answer": [{"line":3,"wrong":"    return x * 2;","answer":"    return x * x;"}]},
      "score": 5, "difficulty": 3
    },
    {"id": "q5", "type": "essay", "stem": "简述快速排序的平均复杂度。", "body": {"kind": "text"}, "score": 5, "difficulty": 3}
  ]
}
```

导入它应看到「五题型示例 · 5 题」。

---

## 2. 题目/答案分离格式（`format_version: 2`）

### 2.1 文件配对

```text
数学/choice.questions.json   ← 题目
数学/choice.answers.json     ← 答案
```

- 同名、后缀不同即配对（下载重复带来的 `(1)` 后缀会被忽略）；只选题目文件时程序会在**同目录**找 `X.answers.json`；
- 两个文件也可一次全选；同一批里两个目录的重名题库**不会**被合并；
- **该格式不支持 `code` 代码简答题**，代码题请用原生 JSON。

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
| `fixes` | 改错 | `[{"line": 3, "wrong": "原文", "answer": "改后"}]` |

`judge`（可空）：判分材料 `{ "entry": "add", "harness": "int main(void){…}", "tests": [{"input":"1 2","output":"3"}] }`。

> 外部格式里 **`difficulty == 0` 表示多选题**（这是题型信息），必须与 `verdict.kind == "multi"` 印证，
> 否则报 `difficulty=1（单选）与答案 kind='multi' 矛盾`。
> 导入后 `difficulty` 原样保留，`0` 仍按"未标注"参与平均难度计算。

---

## 3. 强制执行的校验

两种格式都跑同一套规则：外部格式在导入时逐条检查；原生格式在「导入」与「打包前检查」
两处都跑（源文件可能在导入后被改过）。

| 规则 | 不满足时的提示（真实文案） |
|---|---|
| 顶层必须含 `exam_id` + `questions` | `xxx.json: 无法识别的格式。` |
| JSON 必须是 UTF-8 | `xxx.json: 不是合法的 UTF-8 JSON。` |
| `exam_id` 字符/长度/保留名 | `exam_id 不能包含路径分隔符或 Windows 禁用字符（当前 'a/b'）` |
| `pick ≤ 题目数` | `题库 math 抽题数 20 超过题量 10` |
| 题目 id 唯一 | `题库 math 存在重复题目 ID` |
| `type` 在白名单内 | `题库 math 的题 m001 使用了未知题型：singel；支持 single/multiple/judge/fill/fix/essay/code…` |
| `code` 题的 `body.kind` 必须是 `text` | `题库 math 的题 c1 必须使用 body.kind=text` |
| 代码语言在白名单内 | `题库 math 的题 c1 使用了不支持的语言：brainfuck` |
| 代码语言 ≤ 8 | `题库 math 的题 c1 允许语言过多：9（上限 8）` |
| 语言字段不共存 | `题库 math 的题 c1 同时写了 code_language 与 code_languages；二者不共存` |
| 选项 ≤ 26 | `题库 math 的题 m001 有 27 个选项，超过 26 个（A–Z）` |
| 答案字母在选项内 | `题库 math 的题 m001：答案 ['E'] 不在选项 ['A', 'B'] 中` |
| 答案无重复字母 | `题库 math 的题 m002：答案里有重复字母 ['A', 'A']` |
| 模板占位符数 == `blank_count` | `第 3 条（id=f1）: 模板里有 2 个 ____①____ 占位符，但 blank_count=3` |
| 改错条数 == `blank_count` | `第 4 条（id=x1）: 有 1 条改正项，但 blank_count=2` |
| 改错行号在范围内 | `第 4 条（id=x1）: 第 1 条改正项的 line=99 超出 1..8` |
| 改错 `wrong` 与原文一致 | `第 4 条（id=x1）: 第 1 条改正项的 wrong 与第 3 行原文不一致` |
| 题干非空 | `第 7 条（id=e1）: 题干为空` |
| 外部格式 `count` == 实际条数 | `choice.questions.json: count=500，实际 questions 有 499 条` |
| 外部格式 id 一一对应 | `choice.answers.json: 缺少答案：CH-042` / `多出答案：CH-999` |
| 外部格式 `verdict.kind` 匹配题型 | `第 1 条（id=CH-001）: 题型 choice 的答案 kind 应为 ['multi', 'single']，实际 'blanks'` |
| 外部格式选项键连续 | `第 1 条（id=CH-001）: 选项键应为 ['A', 'B', 'C']，实际 ['A', 'C']` |

> 校验**不**检查"答案是否为对象"——这就是 1.3 那个坑能溜进来的原因。
> 请在源头就写成 `{"kind": …, "answer": …}`。

---

## 4. 常见错误

| 现象 | 原因 | 处理 |
|---|---|---|
| `不是合法的 UTF-8 JSON` | 存成 GBK/ANSI | 记事本「另存为 → UTF-8」；VS Code 编码 → Save with Encoding → UTF-8 |
| `无法识别的格式` | 缺 `exam_id`/`questions`，或 `format_version` 不是 2 | 对照第 1、2 节补齐 |
| `缺少对应的答案文件（X.answers.json）` | 只选了题目文件 | 把答案文件放同目录，或两个一起选 |
| `题库名相同` | 两个目录下有同名题库 | 在「确认导入」里改题库 ID |
| 选择题全被判错、标准答案列为空 | `answer` 写成了纯字母 | 改成对象，见 1.3 |
| 填空/改错判分总是 0 | `blank_count` 与占位符/改正项数量不一致 | 数一遍，两边对齐 |
| 代码题没有语言下拉框 | 学生端低于 v1.3.0 | 用 v1.3.x 的 `stub-template.exe` 重新打包 |
| 旧学生端打开代码题包提示版本不支持 | 含 `code` 题是容器 v8 | 换 v1.1.1+ 学生端 |
| 图片显示 `[图片无法显示]` | 超 4 MB / 4096 px，或 base64 被截断 | 用【插入图片】重新生成 |
| 题干里的 `![...](...)` 原样显示 | 图片没独占一行，或写在了选项里 | 挪到 `stem` 单独一行 |

---

## 5. 自检清单

- [ ] 文件是 UTF-8（无 BOM 也能读，但别用 UTF-16）；`exam_id` 不含路径字符、不是保留名、≤64 字符且全工程唯一；
- [ ] `pick ≤ 题目数`；
- [ ] 每道题的 `type` 是七个标准值之一；
- [ ] **每道要自动判分的题，`answer` 是对象**（`{"kind":…,"answer":…}`）；
- [ ] 选择题答案字母 ⊆ 选项键；多选无重复字母；
- [ ] `body.kind` 与题型匹配（`code` 题型只能配 `text`）；选项 ≤26 时键从 A 连续；
- [ ] 填空题占位符数 = `blank_count`；改错题改正项数 = `blank_count`，`wrong` 逐字一致；
- [ ] 代码题只用 `code_language` **或** `code_languages`（≤8），语言名拼写正确；
- [ ] 设了平均难度区间的题库，所有题都有 `difficulty`（1–255）；
- [ ] 导入后双击题库行（「题库预览」），确认题干、选项、模板/代码都正常展开；
- [ ] 打包前用一张测试答题卡跑一遍判分，确认"标准答案"列不是空的。

---

## 6. 版本与兼容

| 变更 | 影响 |
|---|---|
| 无 `body` 的老题库 | 继续按 `options` 渲染，**不用改写** |
| `judge` / `answer` | 只在题库与工程里存在，打包时剥离 |
| 外部格式 | 目前只支持 `format_version: 2`，其它版本明确报错 |
| `code_language`（v1.2.0+）/ `code_languages`（v1.3.0+） | 旧学生端忽略该字段，仍可作答、无高亮 |
| 答题卡 | 自 v1.3.0 起为 `EESANS02`/v2（新增 `chosen_code_languages`），可读 v1 旧卡 |

格式若有变更，会同时更新本文件、`schemas/`、[发布说明](release-notes.md) 与
[代码审阅报告](代码审阅报告.md) 对应轮次。
