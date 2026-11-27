# exam-server（网页考试）

- 适用版本：**v1.10.1**
- 定位：**可选**的网页作答服务端。教师在**隔离局域网 + 现场监考**下启动它，学生用浏览器打开 `https://<教师机 IP>:8443/`，输入学号（可选 PIN）后作答；教师另开一个浏览器看监考页。

> 它不是「在线考试平台」：没有账号体系、没有云端、不做作弊防控。安全边界见[安全边界与威胁模型](../../docs/安全边界与威胁模型.md)，已接受的风险（R-05 / R-06）见[风险接受记录](../../docs/风险接受记录.md)。
> **本文件只在仓库内阅读**：`crates/` 不随发布包分发，所以上面两条 `../../docs/` 链接在发布包里不存在（不是因为链接写错，而是 `crates/server/README.md` 本身不在包里）。

## 1. 它覆盖的链路

```text
已构建考试包（含 K_manifest 密钥区）
  → 解析 payload / header（容器 v3–v8）
  → 学号 + PIN 试解信封（复用 container::try_unlock_session_with_pin）
  → 解密本场题库（答案已在打包时剥离）
  → 按学号确定性抽题（复用 picker）
  → 浏览器作答（600 ms 防抖，每次保存都加密落盘）
  → 交卷 / 监考收卷 / 到点自动收卷时用 answer_sheet::seal 写出 EESANS02
  → 现有 exam-builder verify-answer-sheet / 管理端可直接读
```

服务端**不持有 master、不持有标准答案**：它只需要考试包和 `K_manifest`（`--keyfile`）。

## 2. 运行

```bash
# 1. 先构建考试包（教师端正常流程也可）
EXAM_MASTER=<64 位主密钥> exam-builder build --manifest live.json --banks banks \
  --stub stub-template.exe --out StudentClient.exe \
  --emit-keyfile key.txt

# 2. 启动服务（教师机）
exam-server --package StudentClient.exe --keyfile key.txt \
  --port 8443 --answers-dir answers --time-check warn

# 3. 学生浏览器访问
#    https://<教师机 IP>:8443/   （首次会提示不安全 → 高级 → 继续前往）
```

master 只能通过环境变量 `EXAM_MASTER` 或 `--master-file` 传递（文件权限由教师自管）。
**命令行 `--master` 参数已经移除**：它会把密钥暴露在进程列表（`ps` / 任务管理器）里，
`AGENTS.md` §5 明确禁止；需要自动化时请用文件或环境变量（优先级：`--master-file` > `EXAM_MASTER`）。
本项目的脚本与文档示例一律不提供 `--master` 用法；看到别人这样传，说明使用的是旧版二进制。
学生端 `exam-client` 同样只有 `EXAM_MASTER` / `--master-file`。

- `stub-template.exe` 只在发布包里有；从源码构建时 `--stub` 用 `target/release/exam-client`（Windows 带 `.exe`）；
- 启动顺序是**先校时、再读包、再绑端口**：无外网的机房最坏要等约 6 秒（两轮 × 3 秒超时），这期间端口还不接受连接。请等到打印「考试服务已启动」再通知学生访问；
- 启动横幅会打印访问地址、证书指纹、监考页地址（带令牌）。

命令行参数：

| 参数 | 说明 |
|---|---|
| `--package` | 已构建的考试包（`--gen-cert` 时可省略） |
| `--keyfile` | `--emit-keyfile` 生成的 K_manifest（64 hex） |
| `--bind` | 监听地址，默认 `0.0.0.0`；可只绑局域网网卡 |
| `--port` | HTTPS 端口，默认 `8443` |
| `--tls` | `auto`（默认：没证书就自动生成自签证书）/ `off`（明文，仅排障） |
| `--tls-cert` / `--tls-key` | 用自己的证书；不给就自动生成到 `<answers-dir>/tls/` |
| `--tls-host` | 证书 SAN 里要包含的地址（可多次）；默认 = 本机局域网 IP + 127.0.0.1 + localhost，`--bind` 指定具体地址时该地址也并入 |
| `--tls-ca` | 外部证书的签发 CA（可选；**只用于启动自检的链校验**，不改变发给学生的证书链） |
| `--tls-check` | 外部证书自检严格程度：`warn`（默认，过期/SAN 缺失只告警）/ `strict`（同样拒绝启动） |
| `--http-redirect-port` | 明文 301 跳转端口（默认 `8080`；`0` = 关闭；与 `--port` 相同会自动关闭） |
| `--gen-cert` | 只生成自签证书然后退出（配合 `--tls-host` / `--answers-dir`） |
| `--session` | 指定场次；默认选当前时间窗内的场次。**仍要求当前时间在窗口内**，不是绕过时间 |
| `--answers-dir` | 交卷答题卡输出目录，默认 `answers/` |
| `--state-dir` | 作答状态目录，默认 `<answers-dir>/state` |
| `--time-check` | `off` / `warn`（默认，只告警）/ `require`（不通过拒绝启动） |
| `--time-tolerance` | 允许的本机与网络时间偏差，默认 `120` 秒（夹取到 30–86400） |
| `--login-rate-limit` | 同一 IP 每分钟登录尝试上限，默认 `60`；`0` = 不限制（演练/同 NAT 时用） |
| `--monitor-token` | 监考令牌（默认从 `<answers-dir>/monitor.token` 读或自动生成；**命令行会进进程列表，仅应急**） |
| `--monitor-allow-weak-token` | 允许 `--monitor-token` 不是 64 hex（**默认拒绝启动**；仅完全隔离的排障机） |
| `--monitor-write-mode` | 监考页写操作：`allow`（默认，明文也允许）/ `tls-only`（明文下写操作 403） |
| `--roster` | 学生名单文件（每行一个学号，`#` 开头为注释）；不给就只能看到「已登录的人」 |

管理端「网页考试…」对话框可以设置 `--port` / `--bind` / `--answers-dir` / `--time-check` / `--login-rate-limit` / `--tls auto|off` / 证书四参数（证书 / 私钥 / CA / 严格自检），并在启动时自动生成 `<答题卡目录>/roster.txt` 传 `--roster`。界面只做形式校验（文件存在 / PEM / 成对），真正的强校验是启动自检（见 §7）。

### 2.1 启动横幅长什么样

绑定成功后依次打印（地址随 `--bind` / `--port` / 证书 SAN 变化）：

（以下按实际字符串摘录，括号内为动态值；启动校时的报告在这之前打印）

```text
考试服务已启动：https://172.16.20.133:8443/
已自动生成自签证书（365 天）：…/tls/server.crt（私钥 …/tls/server.key，仅当前用户可读）
证书指纹（SHA-256）：AB:CD:…
学生首次打开会提示「不安全」→ 点「高级 → 继续前往」；指纹应与这里一致
旧书签 http://172.16.20.133:8080 会自动跳到 HTTPS
监考页：https://172.16.20.133:8443/monitor?t=3f9a…
（只读 + 收卷/终止；令牌见 …/monitor.token，泄漏就删掉它重启会重新生成）
场次 #1；答题卡输出到 answers
（阶段 1：作答实时落盘、崩溃后重登可恢复；Ctrl+C 结束）
```

- 自签证书那行只在自动生成时出现；用外部证书时改为「外部证书自检通过」或「N 项警告」；
- 明文（`--tls off`）时没有 `https` 横幅，改为红字警告同网段可嗅探、监考写操作默认允许；
- 没给 `--roster` 时额外打印「名单有 N 人，但没给 --roster：监考页只能列出已登录的人」；
- 弱令牌（`--monitor-allow-weak-token`）会额外打印红字警告；
- 端口绑定失败会直接退出并说明原因，不会留下半启动状态。

## 3. 接口

学生接口（登录 / 作答 / 交卷这类 `POST` / `PUT` 要求 `X-EES-CSRF: 1`；只读的 `/api/paper`、`/api/time` 不要求）：

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/` | 登录页 + 作答页（含 CSP，脚本 / 样式走 `/assets/`） |
| `GET` | `/assets/*` | `app.js` / `app.css` / `monitor.js` / `monitor.css` / `codemirror.*`，内嵌进二进制；页面与监考资源 `no-store` |
| `POST` | `/api/login` | `{student_id, pin?}`，成功返回 HttpOnly cookie；失败 401 / 403 / 429；按 IP / 学号限速 |
| `GET` | `/api/paper` | 本人试卷 + 已保存答案（不含标准答案）；另给 `answer_text`、`review`。已交卷且策略不允许回看时 `paper` / `answers` / `answer_text` 都是 `null` |
| `PUT` | `/api/answer` | `{qid, value}`，单题 ≤1 MiB，整卡 ≤16 MiB |
| `POST` | `/api/submit` | 冻结并写出 `.eea`；默认策略下同时让会话立即失效 |
| `POST` | `/api/logout` | 主动退出：清 cookie 并从内存删 token（旧 cookie 立刻 401） |
| `GET` | `/api/time` | 服务器时间与剩余秒数；附带 `time_mode` / `clock_offset_secs` 便于排障 |

监考接口（要求 `X-Monitor-Token`；写操作另要 `X-EES-CSRF: 1`）：

| 方法 | 路径 | 说明 |
|---|---|---|
| `GET` | `/monitor` | 监考页（`?t=<令牌>` 读一次后从地址栏抹掉） |
| `GET` | `/api/monitor/stats` | 统计快照（计数 + 每行状态；**不含答案与题干**） |
| `POST` | `/api/monitor/collect` | `{student_id}` 强制收卷（幂等） |
| `POST` | `/api/monitor/collect-all` | `{confirm:"收卷"}` 批量收卷；服务端再校验确认串 |
| `POST` | `/api/monitor/block` | `{student_id, reason?}` 终止：**先封存 → 再吊销 token → 再封禁** |
| `POST` | `/api/monitor/unblock` | `{student_id}` 解除封禁（误点救援） |

所有响应都带安全头：`Content-Security-Policy`（`default-src 'none'; script-src 'self'; …`）、`X-Content-Type-Options: nosniff`、`X-Frame-Options: DENY`、`Referrer-Policy: no-referrer`、`Permissions-Policy: geolocation=(), camera=(), microphone=()`。明文端口的 **301 跳转只带 `nosniff`**（它没有 body，CSP / frame-ancestors 对它没有意义）。

### 3.1 请求 / 响应示例

登录（成功返回 HttpOnly cookie；PIN 只在启用了每生 PIN 的包里需要）：

```http
POST /api/login HTTP/1.1
Content-Type: application/json
X-EES-CSRF: 1

{"student_id": 2021001, "pin": "123456"}
```

```json
{"ok": true}
```

成功只回 `{"ok": true}` 并设置 cookie；学号 / 场次从后续 `/api/paper` 取。

保存单题（服务端只做长度与题目归属校验，不判断对错）：

```http
PUT /api/answer HTTP/1.1
Content-Type: application/json
X-EES-CSRF: 1

{"qid": "m001", "value": "B"}
```

取卷（不含标准答案；已交卷且策略不允许回看时 `paper` 为 `null`）：

```json
{"student_id": 2021001, "session_id": 1, "server_time": 1790000000,
 "start_unix": 1789999200, "end_unix": 1790006400,
 "submitted": false, "review": "none",
 "paper": {"student_id": 2021001, "session_id": 1,
           "sections": [{"exam_id": "math", "title": "高等数学",
                         "items": [{"id": "m001", "qtype": "single", "stem": "1+1=?",
                                    "body": {"kind": "options", "options": [...]},
                                    "score": 2}]}]},
 "answers": {"m001": "B"}, "answer_text": {"m001": "B"}}
```

已交卷且策略不允许回看时，`paper` / `answers` / `answer_text` 三个键是 **`null`**（不是缺键）。示例里的 `paper.items[]` 省略了空字段：`samples` 为空数组时该键**不出现**（`skip_serializing_if`），不是 `[]`。错误体是 `{"error": "..."}`，HTTP 状态码区分 400 / 401 / 403 / 404 / 429；`429` 同时是登录限速与监考写冷却复用。

## 4. 作答、状态与恢复

- 每次保存都写加密状态 `<answers-dir>/state/<学号>-<场次>.json`（原子写 + fsync；交卷路径额外 fsync）——教师机崩溃 / 断电 / 误杀进程后，重登即可拿回未交卷作答；
- 服务重启后重新登录会恢复答案与交卷状态；
- 写盘时序：保存与交卷串行化并复查，「已交卷」不会被旧快照覆盖；答题卡先 fsync 再记已交卷，半截文件会被识别并重写；
- 交卷幂等；重复交卷不会产生第二份卡；
- 事件日志 `<answers-dir>/events.jsonl`（并发写有锁 + 整行一次写出）；
- 回看策略按包里的 `student_review` 执行：默认 `none` 时交卷后 `/api/paper` 只返回 `submitted` / `review`，网页端切到「已交卷」页；
- 网页端作答控件：单选/判断用 radio、多选用 checkbox（答案是 `B` / `AC`），填空/改错用 textarea（服务端把纯文本规范化成离线端的 JSON 编码，判分口径一致）；代码题用 CodeMirror 5（行号 + C 系 `clike` 近似高亮 + Python 专属高亮），静态资源内嵌；
- **题目级 `samples`（v1.9.0 起；v1.9.1 补网页端）**：输入 / 输出示例渲染成独立代码框，单个示例框限高 **`max-height: 240px`**（是 px，不是 pt），超长内容在框内滚动；输入与输出都为空时不渲染（服务端已按 `input + output` 合计 ≤4096 字节、最多 20 组做过校验）；
- 断线提示：保存失败时底部红色横幅说明「作答暂存在本页，正在自动重试」，退避重试（2 s→20 s）；剩余时间改为本地倒计时 + 15 s 校准；教师机重启导致会话失效时给「重新登录」按钮（答案在服务端磁盘上，重登即恢复）；
- 会话：`ees_token` cookie `HttpOnly; SameSite=Strict; Max-Age=3600`（TLS 下加 `Secure`），`/api/paper`、`/api/time` 滑动续期；交卷（默认策略）与退出立即失效；同一学号重新登录会顶掉旧 token。

### 4.1 状态文件与损坏恢复

`<answers-dir>/state/<学号>-<场次>.json` 是**加密 JSON**，密钥为 `HKDF(K_manifest, salt="web-state", info="web-state")`；它不是题库明文，也不是最终答题卡。要点：

- 保存路径是「内存改 → 写临时文件 → fsync → rename」，再更新 `last_seen`；写盘串行化并复查 `submitted`，旧快照不会覆盖已交卷状态；
- 服务重启后，登录时按学号 / 场次找到状态文件并解密；解不开或损坏时按「无状态」处理（学生重答），不会把坏文件当成权威副本；
- `*.json.<seq>.tmp` 是写盘中间产物，启动时直接清掉；
- 最终答题卡只在**交卷 / 监考收卷 / 自动收卷**时用 `answer_sheet::seal` 写出；`.eea` 与离线端同格式，可由 `exam-builder verify-answer-sheet` 校验；
- 交卷与保存共用同一把 `(学号, 场次)` 写锁，避免「卡写了一半、状态已记已交卷」。

## 5. 监考页

启动横幅会打印一行 `监考页：<scheme>://<host>:<port>/monitor?t=<token>`，老师直接点开即可。

- **令牌**：首次启动生成 **32 字节 hex（64 hex 字符）** 写进 `<answers-dir>/monitor.token`（0600 / 受保护 DACL），之后重启**复用**（书签一直有效）；`--monitor-token` 可覆盖，但它会进进程列表，只作应急。非 64 hex 的令牌**默认拒绝启动**（`--monitor-allow-weak-token` 才放行，并在横幅红字警告）。泄漏就删掉该文件重启，会重新生成——**旧书签随之失效**，要重新从横幅取地址。页面从 `?t=` 读一次、存进 `sessionStorage` 后立刻 `history.replaceState` 抹掉地址栏——后续请求只带 `X-Monitor-Token` 头，**不设 cookie**。
- **为什么单独一套令牌**：学生 cookie 只能证明「这个浏览器是某个学生」。若监考页认它，任何学生打开 `/monitor` 就能看全班进度；若监考令牌下发成 cookie，又会被浏览器自动带进学生的写请求。
- **只读接口**只给计数与每行状态（学号、状态、已答 / 总题、最后活跃、登录 / 封存时刻），**绝不下发答案与题干**。`state` 取值：`not_logged_in` / `offline` / `answering` / `sealing`（已置位但卡还没落盘）/ `submitted` / `blocked`；优先级 `blocked > sealing > submitted > offline/answering`。`last_seen` 是**距今秒数**（另有 `last_seen_unix` 给绝对值）。
- **名单**：考试包的信封是匿名的（按学号派生入场密钥定位），运行时**拿不到「名单里有哪些学号」**。「谁还没进来」需要 `--roster <文件>`；不给时只能看到「已登录的人」与「应有 N 人」（N 来自信封数），横幅与页面会提示这一点。管理端启动网页考试时会自动生成并传 `--roster`。
- **写操作**（收卷 / 批量收卷 / 终止 / 解除）要求 `POST` + `X-Monitor-Token` + `X-EES-CSRF: 1`：自定义头本身就是 CSRF 防线（跨站页面带它会触发 CORS 预检，而本服务不发任何 CORS 头）。写操作全部写进 `events.jsonl` 审计，**只记令牌指纹（SHA-256 前 8 位）**，不记令牌本身；同一令牌 **1 秒内只允许一次写（429）**，防止脚本把教师机磁盘打成瓶颈。
- **强制收卷复用 `seal_and_export`**（与学生交卷、自动收卷同一条路径），产出的 `.eea` 格式与指纹完全一致；失败会回滚「已交卷」标记。
- **批量收卷** `collect-all` 必须带 `{"confirm":"收卷"}`，**服务端再校验一次**；返回 `collected` / `already`（已封存过）/ `failed`（例如正被自动收卷持有），部分失败不影响其他人。
- **终止**的顺序固定：① 先封存（失败则整件事不生效，避免「人被封了卡没写」）② 吊销 token（学生下一次请求 401）③ 写入 `blocked`（重新登录直接 403「已被监考终止作答」）。封禁写进 `<answers-dir>/blocked.json`（原子写 + 权限收紧），重启后仍有效。**必须能解除**：误点一次就把学生锁在考场外是最不可接受的现场事故；解除后答案仍在，可重新登录（若卡已封存，重登会显示「已交卷」——**封存不可撤销**，要继续作答只能新建场次 / 重发试卷）。
- **明文模式**：默认仍允许写操作（机房可能只跑明文排障），但令牌可被同网段嗅探——启动横幅会红字警告。要求严格的部署用 `--monitor-write-mode tls-only`（明文下写操作 403，只读仍可用）。
- **已知限制**：监考页只有实时表格，没有历史趋势 / 图表 / 导出，也不做「逐人把卡下载下来」；15 s 轮询，不是秒级推送；「在线」= 60 s 内有过请求，学生关掉页面但没交卷会显示 `offline` 而不是消失。

### 5.1 `/api/monitor/stats` 的形状

```json
{"session_now": 1, "server_time": 1790000000, "end_unix": 1790006400,
 "write_mode": "allow", "plaintext_warning": false, "roster_known": true,
 "counts": {"roster": 60, "logged_in": 58, "online": 57, "submitted": 3,
            "exported": 2, "sealing": 1, "not_logged_in": 2, "blocked": 0},
 "students": [
   {"id": 2021001, "session": 1, "state": "answering", "answered": 12, "total": 30,
    "last_seen": 4, "last_seen_unix": 1790000004, "logged_in_at": 1789999300,
    "submitted_at": null, "exported": null, "blocked_reason": null}
 ]}
```

- `roster_known=false` 时 `counts.roster` 是「信封数」而不是「名单人数」，页面显示「名单未提供」；
- `last_seen` 是**距今秒数**，未登录为 `null`；要绝对时间用 `last_seen_unix`；
- `state` 的优先级是 `blocked > sealing > submitted > offline/answering`；
- 这个接口**绝不下发答案与题干**，只有计数与状态。

## 6. 登录限速与会话

- 按**真实对端 IP**（`TcpStream::peer_addr`，不信 `X-Forwarded-For`）与学号各一分钟窗口：默认每 IP 60 次/分钟、每学号 10 次/分钟；
- IP 桶在「请求体解析成功、学号已拿到」之后才计数：同一 NAT 后面全班共用一个桶，坏请求 / 扫描器不该把真实学生挡在门外；挡 PIN 爆破靠每学号 10 次；
- 被封禁的学生直接 403，连解锁都不做，也不消耗限速额度；
- 单会话：同一学号重新登录顶掉旧 token；多标签页没有强制关闭，只靠单会话顶掉旧页面。

## 7. TLS

- 默认 HTTPS：启动时若 `<answers-dir>/tls/` 里没有证书，会自动生成一张 **ECDSA P-256 自签证书**（SAN 含本机局域网 IP、`127.0.0.1`、`localhost`；EKU 为 `serverAuth`——缺它浏览器会报**不可绕过**的 `ERR_SSL_KEY_USAGE_INCOMPATIBLE`）；有效期 365 天，**到期前 30 天自动重签**；
- **会自动重签的三种情况**（启动时判断，日志会打印「证书已重签：<原因>」）：证书已过期或将在 30 天内到期；本机地址变了、证书 SAN 已不含当前地址；旧版本遗留目录缺少元数据（`server.json`）。证书/私钥文件损坏时也会**备份后重签**，不需要人工删目录；
- ALPN 只声明 `http/1.1`（服务端不实现 h2，声明清楚避免浏览器猜）；
- 启动横幅打印 `https://<IP>:8443/` 与**证书指纹（SHA-256）**。核对路径：先点「继续前往」进入页面，再点地址栏左侧「不安全」→「证书无效」→「查看证书」→「详细信息」看 SHA-256 指纹（**拦截页本身看不到证书详情**）。这是自签场景下识别中间人的唯一办法；
- **外部证书启动自检**（`--tls-cert/--tls-key`）：启动时在**内存里**跑一次真实 TLS 握手（不开端口、不走网络），因此走的是浏览器同一条校验路径，覆盖 **私钥是否配套 / 是否过期 / EKU 是否含 `serverAuth` / SAN 是否覆盖每个访问地址 / 链是否能接到 `--tls-ca`**。结论分两级：
  - **致命**（拒绝启动）：私钥与证书不匹配、EKU 缺 `serverAuth`、链不可用 / 握手失败——这些情况下学生根本连不上，早失败好过开考后炸；
  - **警告**（继续启动）：已过期、**尚未生效**（教师机时钟慢了）、SAN 不覆盖某个地址——浏览器仍可点「继续」，把服务停掉反而更糟；`--tls-check strict` 可让警告也拒绝启动。
  证书文件请按「**叶子 → 中间 CA**」的顺序把中间证书一并放进去：只放叶子、且没给 `--tls-ca` 时会被判成「链不可信」（那其实是缺中间证书，不是 CA 配错）。自检**不打印精确到期日**（`rustls` 不暴露 `notAfter`），只回答「是否过期」；
- 明文端口只做 301：`--http-redirect-port` 起一个**只回跳转**的监听器，不返回页面、不碰状态，旧书签 `http://<IP>:8080/...` 会自动跳到 HTTPS；`--tls off` 时该参数被忽略；
- 已知限制：**iOS 上的 Chrome 用的是 WKWebView，无法绕过证书警告**——iPad/iPhone 请用 Safari；受管 Chromebook 的策略也可能禁止「继续」。这类设备只能改用其他设备，或由老师用 `--tls off` 临时排障。

## 8. 已知限制

- 自签证书不是公共 CA：能挡住同网段的**被动嗅探**，但不挡主动中间人（除非学生核对指纹）；
- 外部证书自检只回答「能不能用」：它走的是 webpki 的 TLS 服务端校验（EKU / 有效期 / 名称 / 链 / 签名 / `basicConstraints`），**不做逐字段审计**——叶子证书 `keyUsage` 的位组合、`policyConstraints` 这类字段不会单独报；需要逐字段核对请用 `openssl x509 -text`；
- `--tls off` 时仍是明文 HTTP（仅建议排障）；此模式下监考写操作默认仍可执行（R-06）；
- 状态是加密 JSON（不是 SQLite）；服务端每连接一线程；
- 隔离局域网没有外网时取时必然失败：默认档只告警，此时场次判定退回教师机系统时钟，需要现场确认时钟（`--time-check require` 可让它在取时失败时拒绝启动）；
- 换一个 Windows 账户启动服务会读不到旧状态文件（受保护 DACL 只给原账户），断线恢复随之失效；
- 交卷不受场次时间窗限制：到点后仍可补交（便于自动收卷失败时补救），但过期后不能再改答案；
- **没有浏览器本地草稿（IndexedDB）**：服务端是唯一权威副本，断网期间未同步的题会丢；
- 静态资源缓存分级：第三方 vendored 资源（CodeMirror 及语言模式）带 `Cache-Control: public, max-age=3600`；自己的 `app.js` / `app.css` / `monitor.*` 是 `no-store`；
- **每个静态资源仍是单独建连**（`Connection: close`，未做 keep-alive）：一次页面加载约 6–7 次 TLS 握手。局域网下可以接受；50 人同时开考时握手数会 ×7；
- 服务端没有实现 h2 / WebSocket，也没有秒级推送。

## 9. 测试与验收

- Rust 单测在 `crates/server/src/server_tests.rs` 与各模块内（含 TLS 自检、限速、收卷、监考鉴权、301 只带 `nosniff`、安全响应头清单）；
- `scripts/smoke-web.sh`：打包 → 起 HTTPS 服务 → 登录 / 取卷 / 作答 / 交卷 → 校验 `.eea`；
- `scripts/smoke-monitor.sh`：**10 条断言**——页面 200 + `no-store`、无令牌 401、令牌 200、收卷出卡、收卷后学生再写被拒、`tls-only` 明文写 403、弱令牌拒绝启动等；它是发布预检 **18 项中的 1 项**（`smoke-tls-cert.sh` 是另一项，两者各占 1 项，不是「两项共 18」）；
- `scripts/smoke-tls-cert.sh`：外部证书自检的四种启动结果（私钥不匹配 / 配套 / SAN 缺失 + strict / IPv6）；
- `scripts/drill-web.sh`：并发与崩溃恢复演练（CI 跑 12 人；真机 30–50 台由使用方组织）；
- CI 的 `web-smoke` job 跑上述网页冒烟；发布预检共 18 项（非 `--full`），完整清单以脚本输出为准。

## 10. 下一步（阶段 1 剩余）

- axum/tokio + SQLite（WAL）：当前单文件、线程一连接、状态是 JSON 文件；
- 监考页的历史趋势 / 导出（当前只有实时表格）；
- 静态资源连接复用（当前每资源一次 TLS 握手；仅在实测首屏变慢时立项）；
- 浏览器本地草稿（IndexedDB）——**未做**：服务端是唯一权威副本，断网期间未同步的题会丢。
