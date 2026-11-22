# exam-server（阶段 0 原型）

网页版学生端的最小可运行原型：教师在隔离局域网里启动它，学生用浏览器打开
`http://<教师机 IP>:<端口>`，输入学号（可选 PIN）后作答。

> 这是**原型**，不是生产实现：只有内存存储、没有管理端 UI、没有 HTTPS、
> 没有 SQLite、限速很粗糙。不要直接用于正式考试。

## 它证明的链路

```text
已构建考试包（含 K_manifest 密钥区）
  → 解析 payload / header
  → 学号 + PIN 试解信封（复用 container::try_unlock_session_with_pin）
  → 解密本场题库
  → 按学号确定性抽题（复用 picker）
  → 浏览器作答（自动保存到内存）
  → 交卷时用 answer_sheet::seal 写出 EESANS02
  → 现有 exam-builder verify-answer-sheet / 管理端可直接读
```

服务端**不持有 master、不持有标准答案**：它只需要考试包和 K_manifest。

## 运行

```bash
# 1. 先构建考试包（教师端正常流程也可）
exam-builder build --manifest live.json --banks banks \
  --stub exam-client.exe --out StudentClient.exe \
  --emit-keyfile key.txt

# 2. 启动服务（教师机）
exam-server --package StudentClient.exe --keyfile key.txt \
  --port 8080 --answers-dir answers

# 3. 学生浏览器访问
#    http://<教师机 IP>:8080/
```

命令行参数：

| 参数 | 说明 |
|---|---|
| `--package` | 已构建的考试包 |
| `--keyfile` | `--emit-keyfile` 生成的 K_manifest（64 hex） |
| `--bind` | 监听地址，默认 `0.0.0.0` |
| `--port` | 端口，默认 `8080` |
| `--session` | 指定场次；默认选当前时间窗内的场次 |
| `--answers-dir` | 交卷答题卡输出目录，默认 `answers/` |

## 接口（原型）

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/` | 登录页 + 作答页 |
| `POST` | `/api/login` | `{student_id, pin?}`，成功返回 HttpOnly cookie |
| `GET` | `/api/paper` | 本人试卷 + 已保存答案（不含标准答案） |
| `PUT` | `/api/answer` | `{qid, value}`，单题 ≤1 MiB，整卡 ≤16 MiB |
| `POST` | `/api/submit` | 冻结并写出 `.eea` |
| `GET` | `/api/time` | 服务器时间与剩余秒数 |

## 已知限制（阶段 0）

- 答案只在内存：服务重启即丢；
- 没有管理端界面（命令行启动）；
- 没有 HTTPS / CSRF / 正式限速（仅按 IP 简单计数）；
- 代码题是纯 textarea，没有 CodeMirror 行号与高亮；
- 没有断线重连/多标签页限制；
- 只按 `--session` 或“当前时间窗内第一个场次”工作。

## 下一步（阶段 1）

axum/tokio + SQLite（WAL）+ 管理端「网页考试」入口 + CodeMirror 5 + 登录限速/CSRF +
断线重连 + 导出/核对 + 文档。
