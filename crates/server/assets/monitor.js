// 监考页：15 s 轮询 + 收卷 / 批量收卷 / 终止 / 解除。
//
// 设计要点（为什么这么写）：
// 1. **令牌只走请求头**。URL 里的 `?t=` 会被浏览器历史、截图、Referer 带走；
//    首屏读到后立刻 `history.replaceState` 抹掉，之后只用 `X-Monitor-Token`。
// 2. **不用 window.confirm**：原生弹窗在长时间监考里容易被学生看到、也无法显示
//    「已答 5/8 题」这类关键信息；统一走页面里的 `<dialog>`。
// 3. **不拼 HTML 字符串**：学号、终止原因来自服务端，一律 `textContent`（CSP 只挡脚本，
//    拼字符串本身也不会被执行，但没有理由给自己留 XSS 面）。
// 4. **写操作幂等且可恢复**：终止 = 服务端封存 → 吊销 → 封禁，界面提供「解除」。
// 5. **明文告警常驻**：`plaintext_warning` 为真时顶部红条；`write_mode=tls-only` 时
//    写按钮直接禁用并给 tooltip，而不是点了才报错。

(function () {
  'use strict';

  const TOKEN_KEY = 'ees_monitor_token';
  const POLL_MS = 15000;
  const CONFIRM_WORD = '收卷';

  const $ = (id) => document.getElementById(id);

  let token = '';
  let lastStats = null;
  let busy = false;
  let stopped = false;
  let pollTimer = null;
  let clockTimer = null;
  let clockOffsetMs = 0;
  let toastTimer = null;

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
      history.replaceState({}, '', location.pathname);
      return fromUrl;
    }
    try {
      return (sessionStorage.getItem(TOKEN_KEY) || '').trim();
    } catch (_) {
      return '';
    }
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
    $('netText').textContent = text || '';
    $('net').classList.toggle('hidden', !text);
  }

  function setActionMsg(text) {
    $('actionMsg').textContent = text || '';
  }

  // ---------------------------------------------------------------- 确认对话框
  // 用 `<dialog>` 而不是 window.confirm：确认框里要显示「已答 5/8 题」、批量收卷要
  // 求输入确认词，原生弹窗做不到。
  function confirmAction(opts) {
    return new Promise((resolve) => {
      const dlg = $('confirmDlg');
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
    const items = [
      ['名单', counts.roster, ''],
      ['已登录', counts.logged_in, 'emphasis'],
      ['在线', counts.online, 'good'],
      ['已交卷', counts.submitted, 'good'],
      ['封存中', counts.sealing, 'warn'],
      ['未登录', counts.not_logged_in, 'bad'],
      ['已终止', counts.blocked, 'bad'],
    ];
    for (const [label, num, cls] of items) {
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

  function opButton(label, cls, title, disabled, onClick) {
    const b = document.createElement('button');
    b.textContent = label;
    b.className = cls;
    b.title = title;
    b.disabled = Boolean(disabled);
    b.addEventListener('click', onClick);
    return b;
  }

  function buildRow(st, tlsOnly) {
    const tr = document.createElement('tr');
    tr.className = 'row-' + st.state;

    const idCell = document.createElement('th');
    idCell.scope = 'row';
    idCell.textContent = String(st.id);
    tr.appendChild(idCell);

    const stateCell = document.createElement('td');
    const badge = document.createElement('span');
    badge.className = 'state state-' + st.state;
    badge.textContent = stateText(st.state);
    if (st.blocked_reason) badge.title = '终止原因：' + st.blocked_reason;
    if (st.submitted_at) badge.title = '交卷时间：' + fmtClock(st.submitted_at);
    stateCell.appendChild(badge);
    tr.appendChild(stateCell);

    const answered = document.createElement('td');
    answered.textContent = st.state === 'not_logged_in' ? '—' : st.answered + '/' + st.total;
    tr.appendChild(answered);

    const seen = document.createElement('td');
    seen.textContent = st.state === 'not_logged_in' ? '—' : fmtAgo(st.last_seen);
    tr.appendChild(seen);

    const login = document.createElement('td');
    login.textContent = st.state === 'not_logged_in' ? '—' : fmtClock(st.logged_in_at);
    tr.appendChild(login);

    const ops = document.createElement('td');
    const writeBlocked = tlsOnly
      ? '当前为明文，写操作已被服务端禁用（--monitor-write-mode）'
      : '';

    if (st.state === 'blocked') {
      ops.appendChild(
        opButton('解除', 'unblock', '恢复该生的登录资格（已封存的答题卡不变）', tlsOnly || busy, () => {
          unblockOne(st);
        }),
      );
    } else if (st.state === 'not_logged_in') {
      ops.appendChild(
        opButton('收卷', 'secondary', '该生还没登录，没有可封存的内容', true, () => {}),
      );
      ops.appendChild(
        opButton('终止', 'danger', '该生还没登录，无需终止', true, () => {}),
      );
    } else {
      const canCollect = st.state !== 'submitted';
      ops.appendChild(
        opButton(
          '收卷',
          'secondary',
          tlsOnly
            ? writeBlocked
            : canCollect
              ? '立即封存该生的答题卡'
              : '该生已交卷，无需重复收卷',
          tlsOnly || busy || !canCollect,
          () => {
            collectOne(st);
          },
        ),
      );
      ops.appendChild(
        opButton(
          '终止',
          'danger',
          tlsOnly ? writeBlocked : '封存并禁止该生重新登录（可在之后解除）',
          tlsOnly || busy,
          () => {
            blockOne(st);
          },
        ),
      );
    }
    tr.appendChild(ops);
    return tr;
  }

  function renderRows(stats) {
    const tbody = $('rows');
    while (tbody.firstChild) tbody.removeChild(tbody.firstChild);
    const list = (stats.students || []).slice().sort((a, b) => a.id - b.id);
    $('empty').classList.toggle('hidden', list.length > 0);
    const tlsOnly = stats.write_mode === 'tls-only';
    for (const st of list) tbody.appendChild(buildRow(st, tlsOnly));
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
    renderCounts(stats.counts || {});
    renderRows(stats);

    const tlsOnly = stats.write_mode === 'tls-only';
    const pending = pendingCount(stats.counts);
    const btn = $('collectAllBtn');
    btn.textContent = batchBtnText(stats.counts);
    btn.disabled = busy || pending === 0 || tlsOnly;
    btn.title = tlsOnly
      ? '当前为明文，写操作已被服务端禁用（--monitor-write-mode）'
      : pending === 0
        ? '没有未交卷的学生'
        : '把尚未交卷的学生立刻封存';

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
      if (document.visibilityState !== 'hidden') refresh();
    }, POLL_MS);
    clockTimer = setInterval(updateClock, 1000);
  }

  async function refresh() {
    if (stopped) return;
    try {
      const stats = await api('/api/monitor/stats');
      lastStats = stats;
      clockOffsetMs = (stats.server_time || 0) * 1000 - Date.now();
      render(stats);
      setNet('');
    } catch (err) {
      if (err.status === 401) {
        // 令牌错/被换掉了：继续轮询只会刷屏，停下来并告诉老师从哪拿新地址。
        stopped = true;
        stopTimers();
        setNet('监考令牌无效或已过期：请重新打开启动横幅里的完整地址（?t=…）。');
        return;
      }
      // 其余错误保留最后一次表格：断线时「最后一次数据」比空白有用。
      setNet('已断开：' + err.message + '，正在重试（显示的是最后一次数据）');
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
      toast(err.message, 'err');
      setNet('操作失败：' + err.message);
      return null;
    }
  }

  async function collectOne(st) {
    if (busy) return;
    const ok = await confirmAction({
      title: '强制收卷',
      body:
        '学号 ' + st.id + ' · 已答 ' + st.answered + '/' + st.total + ' 题。\n' +
        '该生将立即被封存，之后不能再作答（已保存的答案会保留）。',
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
    await refresh();
  }

  async function blockOne(st) {
    if (busy) return;
    const ok = await confirmAction({
      title: '终止作答',
      body:
        '学号 ' + st.id + ' · 已答 ' + st.answered + '/' + st.total + ' 题。\n' +
        '该生会被立即封存并禁止重新登录；可在表格里点「解除」恢复。',
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
    await refresh();
  }

  async function unblockOne(st) {
    if (busy) return;
    const ok = await confirmAction({
      title: '解除终止',
      body: '学号 ' + st.id + ' 将恢复登录资格（已封存的答题卡不会变）。',
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
    await refresh();
  }

  async function collectAll() {
    if (busy || !lastStats) return;
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
        '将立即封存以下 ' + ids.length + ' 名未交卷学生：\n' + shown + more + divergence,
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
    await refresh();
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

    $('refreshBtn').addEventListener('click', () => refresh());
    $('collectAllBtn').addEventListener('click', () => collectAll());
    document.addEventListener('visibilitychange', () => {
      // 切回前台立刻刷一次：老师最需要的是「现在」而不是 15 秒前的数据。
      if (document.visibilityState === 'visible') refresh();
    });

    startTimers();
    refresh();
  }

  init();
})();
