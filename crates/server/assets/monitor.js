// 监考页：15 s 轮询 + 收卷 / 批量收卷 / 终止 / 解除。
//
// 设计要点（为什么这么写）：
// 1. **令牌只走请求头**。URL 里的 `?t=` 会被浏览器历史、截图、Referer 带走；
//    首屏读到后立刻 `history.replaceState` 抹掉（只删 `t`，别的参数与 hash 原样保留），
//    之后只用 `X-Monitor-Token`。
// 2. **不用 window.confirm**：原生弹窗在长时间监考里容易被学生看到、也无法显示
//    「已答 5/8 题」这类关键信息；统一走页面里的 `<dialog>`。
// 3. **不拼 HTML 字符串**：学号、终止原因来自服务端，一律 `textContent`（CSP 只挡脚本，
//    拼字符串本身也不会被执行，但没有理由给自己留 XSS 面）。
// 4. **写操作幂等且可恢复**：终止 = 服务端封存 → 吊销 → 封禁，界面提供「解除」。
// 5. **明文告警常驻**：`plaintext_warning` 为真时顶部红条；`write_mode=tls-only` 时
//    写按钮直接禁用并给 tooltip，而不是点了才报错。
// 6. **增量更新表格**（v1.8.1）：每 15 s 全量重建 `<tbody>` 会把老师正停在某行按钮上的
//    键盘焦点与 hover tooltip 打断；改成按 `data-id` 复用行对象、只改变化的单元格，
//    并在更新前后保存/恢复焦点。
// 7. **轮询有并发闸**：慢响应 + `visibilitychange` 同时触发时，没有闸会堆出多个并发请求。

(function () {
  'use strict';

  const TOKEN_KEY = 'ees_monitor_token';
  const POLL_MS = 15000;
  const FETCH_TIMEOUT_MS = 5000;
  const CONFIRM_WORD = '收卷';

  const $ = (id) => document.getElementById(id);

  let token = '';
  let lastStats = null;
  let busy = false;
  let stopped = false;
  let refreshing = false;
  let pollTimer = null;
  let clockTimer = null;
  let clockOffsetMs = 0;
  let toastTimer = null;
  // 上一次写进断线横幅的文本：`role="alert"` 每次赋值都会被读屏播报，同样的话不能每 15 s 说一遍。
  let netShown = '';

  // 行对象缓存：学号 → { tr, badge, answered, seen, login, opsCell, opsKey }
  const rowsById = new Map();

  // ---------------------------------------------------------------- 纯函数（可单测）
  // 这一段的函数不碰 DOM，便于用 node 直接跑（见交接说明里的自测命令）。
  const STATE_TEXT = {
    not_logged_in: '未登录',
    offline: '离线',
    answering: '作答中',
    sealing: '封存中',
    submitted: '已交卷',
    blocked: '已终止',
  };

  function stateText(state) {
    return STATE_TEXT[state] || state || '未知';
  }

  function fmtAgo(sec) {
    if (sec === null || sec === undefined || Number.isNaN(sec)) return '—';
    const s = Math.max(0, Math.floor(sec));
    if (s < 60) return s + ' 秒前';
    if (s < 3600) {
      const m = Math.floor(s / 60);
      const r = s % 60;
      return r ? m + ' 分 ' + r + ' 秒前' : m + ' 分前';
    }
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    return m ? h + ' 时 ' + m + ' 分前' : h + ' 时前';
  }

  function fmtClock(unix) {
    if (!unix) return '—';
    const d = new Date(unix * 1000);
    const p = (n) => String(n).padStart(2, '0');
    return p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds());
  }

  // 状态徽章的 hover 说明。
  // 原来「终止原因」先写进 title、紧接着又被「交卷时间」覆盖 → 被终止的学生永远看不到原因；
  // 现在拼成多行，且**终止原因优先**（监考最想知道的就是「为什么这个人被终止了」）。
  function badgeTitle(st) {
    const lines = [];
    if (st.blocked_reason) {
      const when = st.submitted_at ? '（' + fmtClock(st.submitted_at) + '）' : '';
      lines.push('终止原因：' + st.blocked_reason + when);
    }
    if (st.submitted_at) lines.push('已交卷：' + fmtClock(st.submitted_at));
    return lines.join('\n');
  }

  // 「未交卷人数」= 已登录 − 已封存（服务端契约就是这么算，界面与按钮文案保持一致）。
  function pendingCount(counts) {
    if (!counts) return 0;
    return Math.max(0, (counts.logged_in || 0) - (counts.exported || 0));
  }

  function batchBtnText(counts) {
    return '批量收卷（未交卷 ' + pendingCount(counts) + ' 人）';
  }

  function remainText(endUnix, nowMs) {
    if (!endUnix) return '';
    const remain = Math.max(0, Math.floor(endUnix - nowMs / 1000));
    if (remain <= 0) return '已到结束时间';
    const h = Math.floor(remain / 3600);
    const m = Math.floor((remain % 3600) / 60);
    const s = remain % 60;
    const p = (n) => String(n).padStart(2, '0');
    return h > 0 ? '剩余 ' + h + ':' + p(m) + ':' + p(s) : '剩余 ' + m + ':' + p(s);
  }

  // 计数区的七格。没给 `--roster` 时服务端只知道「包里有几个人」，不知道「是谁」，
  // 表格只能列出已登录的人——这时把「名单 / 未登录」两格标成「未提供」，别让监考
  // 把「名单 30 / 未登录 27」当成「27 人没来」。
  function countItems(counts) {
    const c = counts || {};
    if (c.roster_known === false) {
      return [
        ['名单未提供', '—', ''],
        ['已登录', c.logged_in, 'emphasis'],
        ['在线', c.online, 'good'],
        ['已交卷', c.submitted, 'good'],
        ['封存中', c.sealing, 'warn'],
        ['只列已登录', '—', 'bad'],
        ['已终止', c.blocked, 'bad'],
      ];
    }
    return [
      ['名单', c.roster, ''],
      ['已登录', c.logged_in, 'emphasis'],
      ['在线', c.online, 'good'],
      ['已交卷', c.submitted, 'good'],
      ['封存中', c.sealing, 'warn'],
      ['未登录', c.not_logged_in, 'bad'],
      ['已终止', c.blocked, 'bad'],
    ];
  }

  // 把底层网络错误翻成老师能看懂的一句话（浏览器的 `Failed to fetch` 对现场毫无帮助）。
  function netErrText(err) {
    const msg = String((err && err.message) || '');
    if (err && (err.name === 'TimeoutError' || err.name === 'AbortError')) return '请求超时';
    if (!navigator.onLine) return '网络不可用';
    if (/Failed to fetch|NetworkError|Load failed|network/i.test(msg)) return '网络不可用';
    return msg || '未知错误';
  }

  // ---------------------------------------------------------------- 令牌
  function readToken() {
    const params = new URLSearchParams(location.search);
    const fromUrl = (params.get('t') || '').trim();
    if (fromUrl) {
      try {
        sessionStorage.setItem(TOKEN_KEY, fromUrl);
      } catch (_) {
        /* 隐私模式下 sessionStorage 可能不可用；那就只用这一次 */
      }
      // 立刻把令牌从地址栏抹掉：别让它进历史、截图或 Referer。
      // 只删 `t`，其余 query 与 hash 原样保留（否则以后加一个参数会被静默吃掉）。
      const url = new URL(location.href);
      url.searchParams.delete('t');
      history.replaceState({}, '', url.pathname + url.search + url.hash);
      return fromUrl;
    }
    try {
      return (sessionStorage.getItem(TOKEN_KEY) || '').trim();
    } catch (_) {
      return '';
    }
  }

  // 5 秒超时：断网时 fetch 可能挂几十秒，界面会一直显示「上一次的数据」而没有任何反馈。
  // 老浏览器没有 `AbortSignal.timeout` 就不带 signal（退回原来的行为，不报错）。
  function timeoutSignal(ms) {
    try {
      if (typeof AbortSignal !== 'undefined' && typeof AbortSignal.timeout === 'function') {
        return AbortSignal.timeout(ms);
      }
    } catch (_) {
      /* 忽略：不支持就不带超时 */
    }
    return undefined;
  }

  // ---------------------------------------------------------------- 请求
  async function api(path, opts) {
    const init = {
      method: (opts && opts.method) || 'GET',
      cache: 'no-store',
      headers: {
        'X-Monitor-Token': token,
        'X-EES-CSRF': '1',
        'Content-Type': 'application/json',
      },
    };
    if (opts && opts.body !== undefined) init.body = JSON.stringify(opts.body);
    if (opts && opts.signal) init.signal = opts.signal;
    const res = await fetch(path, init);
    let data = {};
    try {
      data = await res.json();
    } catch (_) {
      data = {};
    }
    if (!res.ok) {
      const err = new Error(data.error || ('HTTP ' + res.status));
      err.status = res.status;
      throw err;
    }
    return data;
  }

  // ---------------------------------------------------------------- 反馈
  function toast(text, kind) {
    const el = $('toast');
    el.textContent = text;
    el.className = 'toast show' + (kind ? ' ' + kind : '');
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => {
      el.className = 'toast';
    }, 4000);
  }

  function setNet(text) {
    const next = text || '';
    // 同样的话不重复写：`role="alert"` 每次赋值都会被读屏播报一遍。
    if (next === netShown) return;
    netShown = next;
    $('netText').textContent = next;
    $('net').classList.toggle('hidden', !next);
  }

  function setActionMsg(text) {
    $('actionMsg').textContent = text || '';
  }

  // ---------------------------------------------------------------- 确认对话框
  // 用 `<dialog>` 而不是 window.confirm：确认框里要显示「已答 5/8 题」、批量收卷要
  // 求输入确认词，原生弹窗做不到。
  function confirmAction(opts) {
    const dlg = $('confirmDlg');
    // 已经开着就别再 `showModal()`：规范规定对 open 的 dialog 再调会抛 InvalidStateError，
    // 表现是「快速点两行的按钮，第二次没反应」。
    if (dlg.open) return Promise.resolve(false);
    return new Promise((resolve) => {
      $('confirmTitle').textContent = opts.title;
      $('confirmBody').textContent = opts.body;
      const extra = $('confirmExtra');
      while (extra.firstChild) extra.removeChild(extra.firstChild);

      const ok = $('confirmOk');
      ok.textContent = opts.okText || '确定';
      ok.className = opts.danger ? 'danger' : 'primary';
      ok.disabled = false;
      if (opts.buildExtra) opts.buildExtra(extra, ok);

      const onClose = () => {
        dlg.removeEventListener('close', onClose);
        resolve(dlg.returnValue === 'ok');
      };
      dlg.returnValue = 'cancel';
      dlg.addEventListener('close', onClose);
      dlg.showModal();
    });
  }

  // 批量收卷的确认词：必须**恰好**输入「收卷」，「确认」按钮才可用。
  // 这是防止「手滑点一下就把全班封了」的最后一道闸（服务端也会再校验一次）。
  function buildBatchExtra(extra, ok) {
    const hint = document.createElement('p');
    hint.className = 'hint';
    hint.textContent = '请在下方输入「' + CONFIRM_WORD + '」二字确认（此操作不可撤销）';

    const input = document.createElement('input');
    input.type = 'text';
    input.id = 'confirmWord';
    input.autocomplete = 'off';
    input.spellcheck = false;
    input.setAttribute('aria-label', '确认词');
    input.addEventListener('input', () => {
      ok.disabled = input.value.trim() !== CONFIRM_WORD;
    });
    // 按钮禁用时按回车不能提交（否则「输入没对也能 Enter 掉」）。
    input.addEventListener('keydown', (ev) => {
      if (ev.key === 'Enter' && ok.disabled) ev.preventDefault();
    });

    ok.disabled = true;
    extra.appendChild(hint);
    extra.appendChild(input);
  }

  // ---------------------------------------------------------------- 渲染
  function renderCounts(counts) {
    const box = $('counts');
    while (box.firstChild) box.removeChild(box.firstChild);
    for (const [label, num, cls] of countItems(counts)) {
      const cell = document.createElement('div');
      cell.className = 'cell' + (cls ? ' ' + cls : '');
      const n = document.createElement('div');
      n.className = 'num';
      n.textContent = String(num === undefined || num === null ? 0 : num);
      const l = document.createElement('div');
      l.className = 'label';
      l.textContent = label;
      cell.appendChild(n);
      cell.appendChild(l);
      box.appendChild(cell);
    }
  }

  // 按钮工厂。
  // `soft` 的意思是「看起来禁用、但点击要给原因」：`disabled` 的按钮在部分浏览器上不显示
  // `title`，老师看不到「为什么点不了」；这类情况改用 `aria-disabled` + 点击 toast。
  function opButton(label, cls, title, disabled, onClick, soft) {
    const b = document.createElement('button');
    b.textContent = label;
    b.className = cls;
    b.title = title;
    if (disabled && soft) {
      b.setAttribute('aria-disabled', 'true');
      b.classList.add('is-disabled');
      b.addEventListener('click', () => toast(title, 'err'));
    } else {
      b.disabled = Boolean(disabled);
      if (!disabled) b.addEventListener('click', onClick);
    }
    return b;
  }

  function rebuildOps(cell, st, tlsOnly) {
    while (cell.firstChild) cell.removeChild(cell.firstChild);
    const writeBlocked = tlsOnly
      ? '当前为明文，写操作已被服务端禁用（--monitor-write-mode）'
      : '';

    if (st.state === 'blocked') {
      cell.appendChild(
        opButton('解除', 'unblock', '恢复该生的登录资格（已封存的答题卡不变）', tlsOnly || busy, () => {
          unblockOne(st);
        }),
      );
    } else if (st.state === 'not_logged_in') {
      // 未登录的人没有卡可封：按钮保持「不可点」，但点击时告诉老师原因（而不是静默）。
      cell.appendChild(
        opButton('收卷', 'secondary', '该生还没登录，没有可封存的内容', true, () => {}, true),
      );
      cell.appendChild(
        opButton('终止', 'danger', '该生还没登录，无需终止', true, () => {}, true),
      );
    } else {
      const canCollect = st.state !== 'submitted';
      cell.appendChild(
        opButton(
          '收卷',
          'secondary',
          tlsOnly
            ? writeBlocked
            : canCollect
              ? '立即封存该生的答题卡（只含已保存的作答）'
              : '该生已交卷，无需重复收卷',
          tlsOnly || busy || !canCollect,
          () => {
            collectOne(st);
          },
        ),
      );
      cell.appendChild(
        opButton(
          '终止',
          'danger',
          tlsOnly ? writeBlocked : '封存并禁止该生重新登录（可在之后解除；封存不可撤销）',
          tlsOnly || busy,
          () => {
            blockOne(st);
          },
        ),
      );
    }
  }

  function buildRow(st, tlsOnly) {
    const tr = document.createElement('tr');
    tr.dataset.id = String(st.id);

    const idCell = document.createElement('th');
    idCell.scope = 'row';
    idCell.textContent = String(st.id);
    tr.appendChild(idCell);

    const stateCell = document.createElement('td');
    const badge = document.createElement('span');
    stateCell.appendChild(badge);
    tr.appendChild(stateCell);

    const answered = document.createElement('td');
    tr.appendChild(answered);
    const seen = document.createElement('td');
    tr.appendChild(seen);
    const login = document.createElement('td');
    tr.appendChild(login);

    const opsCell = document.createElement('td');
    tr.appendChild(opsCell);

    const row = { tr, badge, stateCell, answered, seen, login, opsCell, opsKey: null };
    updateRow(row, st, tlsOnly);
    return row;
  }

  function updateRow(row, st, tlsOnly) {
    row.tr.className = 'row-' + st.state;
    row.badge.className = 'state state-' + st.state;
    row.badge.textContent = stateText(st.state);
    row.badge.title = badgeTitle(st);
    const notLogged = st.state === 'not_logged_in';
    row.answered.textContent = notLogged ? '—' : st.answered + '/' + st.total;
    row.seen.textContent = notLogged ? '—' : fmtAgo(st.last_seen);
    row.login.textContent = notLogged ? '—' : fmtClock(st.logged_in_at);
    // 操作列只在「会影响按钮可用性」的东西变化时重建：否则每 15 s 重建一批按钮，
    // 会把正在按 Tab 切换焦点的老师打断。
    const opsKey = st.state + '|' + (tlsOnly ? 1 : 0) + '|' + (busy ? 1 : 0);
    if (row.opsKey !== opsKey) {
      rebuildOps(row.opsCell, st, tlsOnly);
      row.opsKey = opsKey;
    }
  }

  // 记下当前焦点落在哪一行的哪个按钮上，更新完再还回去。
  // 不还原的话，老师按 Tab 走到「终止」按钮、刚好赶上 15 s 刷新，焦点就会跳回页面顶部。
  function captureFocus() {
    const el = document.activeElement;
    if (!el || el === document.body || !el.closest) return null;
    if (el.closest('dialog')) return null; // 对话框里的焦点别抢
    const tr = el.closest('tr');
    if (!tr || !tr.dataset || !tr.dataset.id) return null;
    return { id: tr.dataset.id, cls: el.className, text: el.textContent };
  }

  function restoreFocus(info) {
    if (!info) return;
    const tr = $('rows').querySelector('tr[data-id="' + info.id + '"]');
    if (!tr) return;
    for (const btn of tr.querySelectorAll('button')) {
      if (btn.className === info.cls && btn.textContent === info.text) {
        try {
          btn.focus();
        } catch (_) {
          /* 元素可能刚被禁用，忽略 */
        }
        return;
      }
    }
  }

  function renderRows(stats) {
    const tbody = $('rows');
    const list = (stats.students || []).slice().sort((a, b) => a.id - b.id);
    $('empty').classList.toggle('hidden', list.length > 0);
    const tlsOnly = stats.write_mode === 'tls-only';
    const focus = captureFocus();

    const alive = new Set();
    // 按顺序推进游标：行已存在就原地更新并保证顺序，不存在就插到游标位置。
    let cursor = tbody.firstChild;
    for (const st of list) {
      alive.add(st.id);
      let row = rowsById.get(st.id);
      if (!row) {
        row = buildRow(st, tlsOnly);
        rowsById.set(st.id, row);
      } else {
        updateRow(row, st, tlsOnly);
      }
      if (cursor === row.tr) {
        cursor = cursor.nextSibling;
      } else {
        tbody.insertBefore(row.tr, cursor);
      }
    }
    // 名单里已经不存在的人（换了场次、被删了）要清掉，否则会留下幽灵行。
    for (const [id, row] of rowsById) {
      if (!alive.has(id)) {
        row.tr.remove();
        rowsById.delete(id);
      }
    }
    restoreFocus(focus);
  }

  function updateClock() {
    if (!lastStats) {
      $('clock').textContent = '';
      return;
    }
    $('clock').textContent = remainText(lastStats.end_unix, Date.now() + clockOffsetMs);
  }

  function render(stats) {
    $('session').textContent = stats.session_now ? '场次 #' + stats.session_now : '';
    const counts = stats.counts || {};
    renderCounts(counts);
    // 没给 --roster：表格只能列出已登录的人，顶上给一句解释（否则「名单 30 / 未登录 27」
    // 与表格 3 行对不上，监考会以为 27 人没来）。
    $('rosterHint').classList.toggle('hidden', counts.roster_known !== false);
    renderRows(stats);

    const tlsOnly = stats.write_mode === 'tls-only';
    const pending = pendingCount(counts);
    const btn = $('collectAllBtn');
    btn.textContent = batchBtnText(counts);
    btn.disabled = busy || pending === 0 || tlsOnly;
    btn.title = tlsOnly
      ? '当前为明文，写操作已被服务端禁用（--monitor-write-mode）'
      : pending === 0
        ? '没有未交卷的学生'
        : '把尚未交卷的学生立刻封存（只含已保存的作答）';

    $('plaintextWarn').classList.toggle('hidden', !stats.plaintext_warning);
    updateClock();
  }

  // ---------------------------------------------------------------- 轮询
  function stopTimers() {
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
    if (clockTimer) {
      clearInterval(clockTimer);
      clockTimer = null;
    }
  }

  function startTimers() {
    stopTimers();
    pollTimer = setInterval(() => {
      // 后台标签页不轮询：50 人考场里老师可能开着好几个页面。
      if (document.visibilityState !== 'hidden') refresh(false);
    }, POLL_MS);
    clockTimer = setInterval(updateClock, 1000);
  }

  async function refresh(manual) {
    if (stopped) {
      // 令牌失效后轮询已停，但「立即刷新」按钮还留在页面上：点了必须给反馈，
      // 否则老师会以为页面卡死（原来是静默 return）。
      if (manual) toast('监考令牌无效或已过期：请从启动横幅重新复制完整地址（含 ?t=…）', 'err');
      return;
    }
    // 并发闸：慢响应 + 切回前台都会触发 refresh，没有闸会堆出多个并发请求。
    if (refreshing) {
      if (manual) toast('正在刷新，请稍候', '');
      return;
    }
    refreshing = true;
    try {
      const stats = await api('/api/monitor/stats', { signal: timeoutSignal(FETCH_TIMEOUT_MS) });
      lastStats = stats;
      clockOffsetMs = (stats.server_time || 0) * 1000 - Date.now();
      render(stats);
      setNet('');
    } catch (err) {
      if (err && (err.name === 'TimeoutError' || err.name === 'AbortError')) {
        setNet('请求超时（5 秒）：正在重试（显示的是最后一次数据）');
        return;
      }
      if (err.status === 401) {
        // 令牌错/被换掉了：继续轮询只会刷屏，停下来并告诉老师从哪拿新地址。
        stopped = true;
        stopTimers();
        setNet('监考令牌无效或已过期：请重新打开启动横幅里的完整地址（?t=…）。');
        return;
      }
      // 其余错误保留最后一次表格：断线时「最后一次数据」比空白有用。
      setNet('已断开：' + netErrText(err) + '，正在重试（显示的是最后一次数据）');
    } finally {
      refreshing = false;
    }
  }

  // ---------------------------------------------------------------- 写操作
  function setBusy(on) {
    busy = on;
    if (lastStats) render(lastStats);
  }

  async function doWrite(path, body) {
    try {
      return await api(path, { method: 'POST', body });
    } catch (err) {
      // 429 是「操作太频繁」（服务端有 1 s 冷却），不是失败：服务端文案本身已经是
      // 完整的一句话，不要再套「操作失败：」或重复前缀。
      toast(err.message, 'err');
      setNet(err.status === 429 ? err.message : '操作失败：' + err.message);
      return null;
    }
  }

  async function collectOne(st) {
    if (busy) return;
    const ok = await confirmAction({
      title: '强制收卷',
      body:
        '学号 ' + st.id + ' · 已答 ' + st.answered + '/' + st.total + ' 题。\n' +
        '该生将立即被封存，之后不能再作答（只包含已保存的作答；正在输入、尚未保存的内容不会进卡）。',
      okText: '强制收卷',
      danger: true,
    });
    if (!ok) return;
    setBusy(true);
    const data = await doWrite('/api/monitor/collect', { student_id: st.id });
    setBusy(false);
    if (!data) return;
    toast(data.already ? '已封存，无需重复收卷' : '已收卷：' + (data.file || ''), 'ok');
    setNet('');
    await refresh(false);
  }

  async function blockOne(st) {
    if (busy) return;
    const ok = await confirmAction({
      title: '终止作答',
      body:
        '学号 ' + st.id + ' · 已答 ' + st.answered + '/' + st.total + ' 题。\n' +
        '该生会被立即封存并禁止重新登录；可在表格里点「解除」恢复登录（封存不可撤销）。',
      okText: '终止作答',
      danger: true,
    });
    if (!ok) return;
    setBusy(true);
    const data = await doWrite('/api/monitor/block', { student_id: st.id, reason: '监考终止' });
    setBusy(false);
    if (!data) return;
    toast('已终止', 'ok');
    setNet('');
    await refresh(false);
  }

  async function unblockOne(st) {
    if (busy) return;
    const ok = await confirmAction({
      title: '解除终止',
      body:
        '学号 ' + st.id + ' 将恢复登录资格（已封存的答题卡不会变；' +
        '如果卡已封存，学生登录后看到的是「已交卷」页）。',
      okText: '解除',
      danger: false,
    });
    if (!ok) return;
    setBusy(true);
    const data = await doWrite('/api/monitor/unblock', { student_id: st.id });
    setBusy(false);
    if (!data) return;
    toast('已解除', 'ok');
    setNet('');
    await refresh(false);
  }

  async function collectAll() {
    if (busy) return;
    // 先刷新再算人数：按钮与确认框原来都用 15 s 前的快照，老师确认的集合可能比
    // 服务端实际要收的少一个人（刚登录的）——破坏性操作的确认必须基于「此刻」。
    await refresh(true);
    if (!lastStats) return;
    const counts = lastStats.counts || {};
    const pending = pendingCount(counts);
    if (pending <= 0) return;
    const ids = (lastStats.students || [])
      .filter((s) => s.state === 'answering' || s.state === 'offline' || s.state === 'sealing')
      .map((s) => s.id);
    if (ids.length === 0) {
      // 明细为空但计数说还有人：说明那些人已经封存/终止，没必要弹一个「收 0 人」的确认框。
      toast('表格里没有可收卷的学生（可能已封存）', 'err');
      return;
    }
    // 明细来自表格；按钮上的数字来自服务端计数。两者不一致时以**明细**为准并说明——
    // 监考点的是「看得见的这些人」，不能让确认框里的数字和列表对不上。
    const shown = ids.slice(0, 20).join('、');
    const more = ids.length > 20 ? '…等 ' + ids.length + ' 人' : '';
    const divergence =
      ids.length === pending ? '' : '（服务端统计 ' + pending + ' 人，上表可定位明细 ' + ids.length + ' 人）';

    const ok = await confirmAction({
      title: '批量收卷',
      body:
        '将立即封存以下 ' + ids.length + ' 名未交卷学生：\n' +
        shown +
        more +
        divergence +
        '\n（刚登录、还没出现在上表里的学生也可能包含在内；只封存已保存的作答）',
      okText: '批量收卷',
      danger: true,
      buildExtra: buildBatchExtra,
    });
    if (!ok) return;

    setBusy(true);
    const data = await doWrite('/api/monitor/collect-all', { confirm: CONFIRM_WORD });
    setBusy(false);
    if (!data) return;

    const parts = [];
    const collected = data.collected || [];
    const already = data.already || [];
    const failed = data.failed || [];
    if (collected.length) parts.push('已收卷 ' + collected.length + ' 人');
    if (already.length) parts.push('已封存跳过 ' + already.length + ' 人');
    if (failed.length) {
      parts.push(
        '失败 ' +
          failed.length +
          ' 人（' +
          failed.map((f) => f.id + '：' + f.error).join('；') +
          '）',
      );
    }
    setActionMsg(parts.join('；') || '没有需要收卷的学生。');
    toast(failed.length ? '批量收卷部分失败，见上方摘要' : '批量收卷完成', failed.length ? 'err' : 'ok');
    await refresh(false);
  }

  // ---------------------------------------------------------------- 启动
  function init() {
    token = readToken();
    if (!token) {
      // 没令牌时把「计数 / 批量按钮 / 空表格」一起收起来：否则页面一边说缺令牌、
      // 一边显示「批量收卷（未交卷 0 人）」和空表，反而让人以为已经连上了。
      $('tokenMissing').classList.remove('hidden');
      for (const id of ['counts', 'actions', 'tableWrap']) $(id).classList.add('hidden');
      return;
    }

    $('refreshBtn').addEventListener('click', () => refresh(true));
    $('collectAllBtn').addEventListener('click', () => collectAll());
    document.addEventListener('visibilitychange', () => {
      // 切回前台立刻刷一次：老师最需要的是「现在」而不是 15 秒前的数据。
      if (document.visibilityState === 'visible') refresh(false);
    });

    startTimers();
    refresh(false);
  }

  // 便于用 node 跑纯函数自测（浏览器里 `module` 不存在，这段不会执行）。
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = {
      stateText,
      fmtAgo,
      fmtClock,
      badgeTitle,
      pendingCount,
      batchBtnText,
      remainText,
      countItems,
      netErrText,
    };
  }

  if (typeof document !== 'undefined') init();
})();
