# exam-server（网页考试，v1.4.x）

网页版学生端：教师在隔离局域网里启动它（管理端「网页考试…」入口），
学生用浏览器打开 `http://<教师机 IP>:<端口>`，输入学号（可选 PIN）后作答。

> 仅用于**隔离局域网 + 现场监考**；HTTP 明文，不要接入互联网。

## 它证明的链路

```text
已构建考试包（含 K_manifest 密钥区）
  → 解析 payload / header
  → 学号 + PIN 试解信封（复用 container::try_unlock_session_with_pin）
  → 解密本场题库
  → 按学号确定性抽题（复用 picker）
  → 浏览器作答（每 600 ms 防抖，保存一次即加密落盘）
  → 交卷时用 answer_sheet::seal 写出 EESANS02
  → 现有 exam-builder verify-answer-sheet / 管理端可直接读
```

服务端**不持有 master、不持有标准答案**：它只需要考试包和 K_manifest。

## 运行

```bash
# 1. 先构建考试包（教师端正常流程也可）
#    master 只走环境变量：`--master` 会出现在进程列表里（仓库安全红线）
EXAM_MASTER=<64 位主密钥> exam-builder build --manifest live.json --banks banks \
  --stub stub-template.exe --out StudentClient.exe \
  --emit-keyfile key.txt
# 注：`stub-template.exe` 只在发布包里有；从源码构建时 --stub 用
#     `target/release/exam-client`（Windows 带 .exe）

# 2. 启动服务（教师机）
exam-server --package StudentClient.exe --keyfile key.txt \
  --port 8080 --answers-dir answers --time-check warn

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
| `--state-dir` | 作答状态目录，默认 `<answers-dir>/state` |
| `--time-check` | `off` / `warn`（默认，只告警）/ `require`（不通过拒绝启动） |
| `--time-tolerance` | 允许的本机与网络时间偏差，默认 `120` 秒（夹取到 30–86400） |
| `--login-rate-limit` | 同一 IP 每分钟登录尝试上限，默认 `60`；`0` = 不限制（演练/同 NAT 时用） |

启动时会先做一次三源网络时间校验并打印结论（本地时间 / 网络时间 / 偏差 / 生效偏移）。
管理端「网页考试…」对话框可以直接设置 `--bind` / `--time-check` / `--login-rate-limit`，
并在服务打印出「考试服务已启动」之后才显示访问地址。
启动横幅里的地址跟随 `--bind`：绑 `0.0.0.0` 时显示本机局域网 IP，绑具体地址时原样显示
（绑 `127.0.0.1` 调试时不会再打印一个学生连不上的地址）。
默认 `warn`：没有外网的隔离局域网会打印「取时失败，仅告警」后继续启动；
`require` 适合确实有外网、又要求时间强一致的考场。

注意这一步发生在**绑定端口之前**：无外网的机房最坏要等约 6 秒（两轮 × 3 秒超时），
这期间端口还不接受连接。请等到打印出「考试服务已启动」再通知学生访问。

## 接口

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/` | 登录页 + 作答页 |
| `POST` | `/api/login` | `{student_id, pin?}`，成功返回 HttpOnly cookie |
| `GET` | `/api/paper` | 本人试卷 + 已保存答案（不含标准答案） |
| `PUT` | `/api/answer` | `{qid, value}`，单题 ≤1 MiB，整卡 ≤16 MiB |
| `POST` | `/api/submit` | 冻结并写出 `.eea` |
| `GET` | `/api/time` | 服务器时间与剩余秒数；附带 `time_mode` / `clock_offset_secs` 便于排障 |

## 已具备（阶段 1）

- 答案落盘：`<answers-dir>/state/<学号>-<场次>.json`，原子写 + 交卷幂等；**每次保存都落盘**
  （v1.4.5 起，前端 600 ms 防抖；交卷路径额外 fsync）——教师机崩溃 / 断电 / 误杀进程后
  重登即可拿回未交卷的作答；
- 服务重启后重新登录会恢复答案与交卷状态；
- CSRF：**登录 / 作答 / 交卷都要求 `X-EES-CSRF: 1`**（防登录 CSRF：否则任意网页都能用别人的学号把受害者登进攻击者的会话）；
- 单会话：同一学号重新登录会顶掉旧 token；
- 登录限速：按**真实对端 IP**（`TcpStream::peer_addr`）与学号各一分钟窗口；
  默认每 IP 60 次/分钟、每学号 10 次/分钟；
- 到点收卷：学生页面在剩余 0 秒时自动 `POST /api/submit`；页面已关闭的由后台线程每 15 秒
  收一轮（最多延迟 15 秒）。加密与写盘在全局状态锁**之外**进行（v1.4.4），不会堵住其他学生的请求；
- 事件日志：`<answers-dir>/events.jsonl`（并发写有锁 + 整行一次写出，v1.4.5）；
- 代码题：CodeMirror 5（行号 + C 系语言用 `clike` 近似高亮 + Python 专属高亮），静态资源内嵌；
- **写盘时序（v1.4.8）**：保存与交卷的状态写盘串行化并复查，「已交卷」不会被旧快照覆盖；
  答题卡先 fsync 再记已交卷，半截文件会被识别并重写；
- **启动校时（v1.4.3）**：三源中位数校验网络时间，偏差存成时钟偏移供场次判定使用；
- **状态文件权限收紧（v1.4.3）**：Windows 受保护 DACL（仅当前用户）/ Unix `0600`、`0700`，
  旧文件在启动时补收紧。

## 已知限制

- HTTP 明文（按约定只用于隔离局域网）；
- 状态文件是加密 JSON（不是 SQLite）；没有管理端监控页；
- 多标签页只靠单会话 token 顶掉旧页面，没有强制关闭；
- 隔离局域网没有外网时取时必然失败：默认档只告警，此时场次判定退回教师机系统时钟，
  需要现场确认时钟（`--time-check require` 可让它在取时失败时拒绝启动）；
- 换一个 Windows 账户启动服务会读不到旧状态文件（受保护 DACL 只给原账户），断线恢复随之失效；
- 交卷不受场次时间窗限制：到点后仍可补交（便于自动收卷失败时补救），但过期后不能再改答案。

## 下一步（阶段 1 剩余）

axum/tokio + SQLite（WAL）+ 管理端监控页（在线人数 / 交卷进度 / 逐人导出）。
