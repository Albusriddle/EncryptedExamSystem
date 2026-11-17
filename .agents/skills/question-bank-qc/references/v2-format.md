# 旧版分离格式（`format_version: 2`）质检要点

> 历史题库（如 `choice.questions.json` + `choice.answers.json`）使用分离格式。原生 JSON 是新格式；两者都能被管理端导入，但质检前必须先分清。

## 1. 文件配对

| 文件 | 顶层字段 | 内容 |
|---|---|---|
| `X.questions.json` | `format_version:2`、`type`、`role:"questions"`、`count`、`questions[]` | `id/kp/difficulty/stem/content{kind:options\|template\|code}` |
| `X.answers.json` | 同上、`role:"answers"`、`count`、`answers[]` | `id/verdict{kind,answer}/judge` |

- 两文件按 `id` 一一对应，`count` 必须等于实际条数；
- 只选题目文件时，管理端会在**同目录**找 `X.answers.json`；
- 同名不同后缀（`(1)` 等下载后缀）会被忽略。

## 2. `difficulty` 的双重语义（旧格式坑）

旧格式里 `difficulty` **同时表示题型**：

| difficulty | 含义 |
|---|---|
| `0` | **多选题**（题型信息，不是难度） |
| `1/2/3` | 单选难度 |
| `4` | 大题（原生格式） |

因此：

- 旧格式的“多选”没有 1/2/3 难度；
- 转成原生 JSON 时，`type=multiple` 与 `difficulty` 解耦，可以设统一 `9`；
- 质检时若看到 `difficulty=0`，先判断是旧格式多选，而不是“难度 0”。

## 3. 内容形态

| `content.kind` | 用于 | 关键字段 |
|---|---|---|
| `options` | 单选/多选 | `options{A..D}` 或 `[{key,text}]` |
| `template` | 填空 | `template`、`blank_count` |
| `code` | 改错 | `code[]`、`blank_count` |

答案侧：

| `verdict.kind` | 用于 | 关键字段 |
|---|---|---|
| `single` | 单选 | `answer:"B"` |
| `multi` | 多选 | `answer:["A","C"]` |
| `blanks` | 填空 | `answer:[{no,answer}]` |
| `fixes` | 改错 | `answer:[{line,wrong,answer}]` |

## 4. 导入前检查清单

1. `format_version == 2`，`role` 正确；
2. `count` 与实际条数一致；
3. questions 与 answers 的 `id` 集合相等；
4. `type` 与 `content.kind` 匹配（choice→options、fill_blank→template、fix_code→code）；
5. 旧格式 `difficulty=0` 只出现在多选；
6. 导入后转原生并跑一遍 `verify.py`。

## 5. 参考实现

- 转换/导入逻辑：`admin/app/core/bank_import.py`（`difficulty==0` 判定多选在 `:305-321`）；
- 原生模型：`admin/app/core/models.py`（`QuestionBody.kind`、`Question.qtype` 白名单）；
- 旧格式规范：`docs/题库JSON规范.md`。
