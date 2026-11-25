// 网页端状态：paper 只在允许看到卷面时才有值（回看策略为「仅教师可查看原卷」时，
// 交卷后服务端不再下发卷面，这里切到「已交卷」页）。
let paper = null;
// 用无原型对象：qid 是题库里的字符串，若恰好叫 `__proto__` / `constructor`，
// 普通对象的赋值会静默失效（答案看起来"没保存"，判分却拿不到）。
let answers = Object.create(null);
let submitted = false;
let dirty = new Set();
let editable = true;
let serverOffset = 0;    // 服务端时间 - 本机时间（秒）
let endUnix = 0;
let editors = new Map(); // qid -> CodeMirror 实例
// 多选的已选项：qid -> Set(key)。不能拿答案字符串做子串匹配——选项 key 来自题库，
// 可能是多字符（`"AA"` 会被 `"AAB".includes` 误判成已选）。
let choices = new Map();
let reloginNeeded = false;
let me = null;            // 本次登录的学号（「已交卷」页要显示它）

const $ = (id) => document.getElementById(id);
function toast(text) { const t = $('toast'); t.textContent = text; t.classList.add('show'); setTimeout(() => t.classList.remove('show'), 2000); }
function fmt(sec) { if (sec <= 0) return '已结束'; const h = Math.floor(sec/3600), m = Math.floor(sec%3600/60), s = sec%60; return `${h? h+'时':''}${m}分${s}秒`; }
function serverNow() { return Math.floor(Date.now()/1000 + serverOffset); }

async function api(path, opts = {}) {
  const res = await fetch(path, { credentials: 'same-origin', headers: { 'Content-Type': 'application/json', 'X-EES-CSRF': '1' }, ...opts });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) { const e = new Error(data.error || ('HTTP ' + res.status)); e.status = res.status; throw e; }
  return data;
}

// ---------------------------------------------------------------- 断线感知
function setNet(text) {
  const bar = $('net');
  // 只改文本节点：直接写 bar.textContent 会把常驻的「重新登录」按钮一起抹掉，
  // 学生就失去了恢复入口（网络抖动一次就触发）。
  $('netText').textContent = text || '';
  bar.classList.toggle('show', !!text);
  if (!text) $('reloginBtn').classList.add('hidden');
}
window.addEventListener('online', () => { setNet(''); toast('网络已恢复'); retrySaveSoon(); });
window.addEventListener('offline', () => setNet('网络已断开：作答暂存在本页，恢复后会自动保存。请不要关闭页面。'));

// 会话失效（教师机重启 / 长时间无操作）：答案在服务端磁盘上，重登即可恢复。
// 不提示的话，页面只会一直显示「保存失败（登录已失效）」，学生不知道该怎么办。
function needRelogin() {
  if (reloginNeeded) return;
  reloginNeeded = true;
  // 先把「丢了几题」记下来：已保存的确实不会丢，但还没同步的那几题需要重作答，
  // 文案不能含糊成「答案都不会丢」。
  const lost = dirty.size;
  dirty.clear();
  $('net').classList.add('show');
  $('netText').textContent = lost
    ? `登录已失效（教师机重启或会话过期）：已保存的答案不会丢；有 ${lost} 题还没同步，需要重新作答。`
    : '登录已失效（教师机重启或会话过期）：已保存的答案不会丢，请重新登录。';
  $('reloginBtn').classList.remove('hidden');
}

// ---------------------------------------------------------------- 登录
async function login() {
  $('loginMsg').textContent = '';
  try {
    const student_id = Number($('sid').value.trim());
    if (!student_id) throw new Error('请输入学号');
    const pin = $('pin').value.trim();
    await api('/api/login', { method: 'POST', body: JSON.stringify({ student_id, pin: pin || null }) });
    apply(await api('/api/paper'));
    $('login').classList.add('hidden');
  } catch (e) { $('loginMsg').textContent = e.message; }
}

// 把 /api/paper 的结果落到界面上（登录、刷新、刚交卷都走这里）。
function apply(data) {
  $('login').classList.add('hidden');
  me = data.student_id;
  submitted = !!data.submitted;
  endUnix = data.end_unix || 0;
  serverOffset = (data.server_time || serverNow()) - Math.floor(Date.now()/1000);
  $('title').textContent = `学号 ${data.student_id} · 场次 #${data.session_id}`;
  if (!data.paper) {
    // 已交卷 + 策略不允许回看：不下发卷面，切到「已交卷」页。
    showDone(data.student_id);
    return;
  }
  paper = data.paper;
  answers = Object.assign(Object.create(null), data.answer_text || data.answers || {});
  choices = new Map();
  $('done').classList.add('hidden');
  $('exam').classList.remove('hidden');
  // 已交卷但允许回看：只读展示，并说明处于回看模式。
  editable = !submitted;
  render();
  if (submitted) {
    lockAll();
    const note = $('note');
    note.classList.remove('hidden');
    note.textContent = '已交卷，当前为只读回看（到结束时间后自动收起）。';
  }
  updateClock();
}

/// 切到「已交卷」页。提交后会话 cookie 会立即失效（默认策略），
/// 此时再取 /api/paper 会 401 —— 所以这个页面必须能本地渲染。
function showDone(studentId) {
  // 状态与 UI 一起收敛，不靠调用方记得先置 submitted——漏一个就会出现
  // 「页面显示已交卷，但 beforeunload 还在拦、自动保存还在发」。
  submitted = true;
  editable = false;
  lockAll();
  if (tickTimer) { clearInterval(tickTimer); tickTimer = null; }
  if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
  clearResubmit(); // 已交卷后不该再冒出一个迟到的 /api/submit
  paper = null;
  answers = Object.create(null);
  choices = new Map();
  dirty.clear();
  $('exam').classList.add('hidden');
  $('done').classList.remove('hidden');
  $('doneMsg').textContent = studentId ? `学号 ${studentId} 的答题卡已封存。` : '答题卡已封存。';
}

// 只支持题库实际用到的 Markdown 子集：图片与围栏代码，其余按纯文本显示。
function renderStem(text) {
  const box = document.createElement('div');
  const parts = String(text || '').split(/```/);
  parts.forEach((part, i) => {
    if (i % 2 === 1) {
      const pre = document.createElement('pre'); pre.className = 'stem'; pre.textContent = part.replace(/^[\w+-]*\n/, ''); box.appendChild(pre);
      return;
    }
    const lines = part.split('\n');
    let buf = [];
    const flush = () => { if (buf.length) { const pre = document.createElement('pre'); pre.className = 'stem'; pre.textContent = buf.join('\n'); box.appendChild(pre); buf = []; } };
    for (const line of lines) {
      const m = line.match(/^\s*!\[([^\]]*)\]\((data:image\/(?:png|jpeg);base64,[^)]+)\)\s*$/);
      if (m) {
        flush();
        const img = document.createElement('img'); img.alt = m[1]; img.src = m[2]; img.style.maxWidth = '100%'; box.appendChild(img);
      } else { buf.push(line); }
    }
    flush();
  });
  return box;
}

// 选择题的选项列表：新格式在 body.options（带 key），旧格式是 item.options（字符串数组）。
function optionsOf(item) {
  const body = item.body || {};
  if (body.kind === 'options' && body.options && body.options.length) {
    return body.options.map(o => ({ key: String(o.key), text: o.text == null ? '' : String(o.text) }));
  }
  if (item.options && item.options.length) {
    return item.options.map((t, i) => ({ key: String.fromCharCode(65 + i), text: String(t) }));
  }
  return [];
}

function render() {
  // 旧实例必须还原成 textarea 再扔掉：否则每换一次卷（apply）都会攒下一批脱离 DOM 的
  // CodeMirror 实例，长场次里会一直吃内存。
  editors.forEach(cm => { try { cm.toTextArea(); } catch (_) { /* 已经不在 DOM 里就算了 */ } });
  editors.clear();
  const box = $('questions'); box.innerHTML = '';
  let n = 0;
  for (const sec of paper.sections) {
    const h = document.createElement('h2'); h.textContent = sec.title; h.style.fontSize = '16px'; h.style.margin = '18px 0 6px'; box.appendChild(h);
    for (const item of sec.items) {
      n++;
      const q = document.createElement('div'); q.className = 'card q';
      const no = document.createElement('div'); no.className = 'qno'; no.textContent = `${n}. （${item.score} 分）`; q.appendChild(no);
      q.appendChild(renderStem(item.stem));
      const body = item.body || {};
      const opts = optionsOf(item);
      if (opts.length && (item.qtype === 'single' || item.qtype === 'multiple' || item.qtype === 'judge')) {
        q.appendChild(renderChoices(item, opts));
      } else {
        if (body.kind === 'options' && body.options && body.options.length) {
          const ol = document.createElement('pre'); ol.className = 'stem';
          ol.textContent = body.options.map(o => `${o.key}. ${o.text}`).join('\n');
          q.appendChild(ol);
        } else if (body.kind === 'template') {
          const pre = document.createElement('pre'); pre.className = 'stem'; pre.textContent = body.template || ''; q.appendChild(pre);
        } else if (body.kind === 'code' && body.code) {
          const pre = document.createElement('pre'); pre.className = 'stem'; pre.textContent = body.code.join('\n'); q.appendChild(pre);
        } else if (item.options && item.options.length) {
          const ol = document.createElement('pre'); ol.className = 'stem'; ol.textContent = item.options.map((o, i) => `${String.fromCharCode(65+i)}. ${o}`).join('\n'); q.appendChild(ol);
        }
        // 必须先入 DOM 再初始化 CodeMirror：`CodeMirror.fromTextArea` 内部要用
        // `textarea.parentNode.insertBefore`，textarea 还没挂载时会抛 TypeError，
        // 导致整页渲染中断（v1.4.11–v1.5.1 的代码题空白就是这么来的）。
        const ta = renderText(item);
        q.appendChild(ta);
        if (item.qtype === 'code') initCodeEditor(ta, item);
      }
      box.appendChild(q);
    }
  }
}

// ② 选择题控件：单选/判断用 radio，多选用 checkbox；答案写成 "B" / "AC"（与判分口径一致）。
function renderChoices(item, opts) {
  const wrap = document.createElement('div'); wrap.className = 'choice';
  const multi = item.qtype === 'multiple';
  const current = String(answers[item.id] || '').toUpperCase();
  const picked = pickedSet(item, opts);
  // 把 input 按 options 下标存下来：选项 key 来自题库，可能含引号/方括号，
  // 拼进 querySelector 会抛异常（那题就静默没法多选了）。
  const inputs = [];
  for (const o of opts) {
    const label = document.createElement('label');
    const input = document.createElement('input');
    input.type = multi ? 'checkbox' : 'radio';
    input.name = 'q-' + item.id;
    input.value = o.key;
    input.checked = multi ? picked.has(o.key) : current === o.key;
    input.disabled = !editable;
    input.addEventListener('change', () => {
      if (multi) {
        const keys = opts.filter((_, i) => inputs[i].checked).map(x => x.key).sort();
        picked.clear();
        keys.forEach(k => picked.add(k));
        setAnswer(item.id, keys.join(''));
      } else {
        setAnswer(item.id, o.key);
      }
    });
    inputs.push(input);
    const span = document.createElement('span');
    span.textContent = `${o.key}. ${o.text}`;
    label.appendChild(input); label.appendChild(span); wrap.appendChild(label);
  }
  return wrap;
}

/// 某题已选中的选项 key 集合（多选回显用）。
///
/// 优先用内存里的集合（本次作答期间的权威状态）；首次渲染时从服务端答案回显：
/// 单字符 key 直接按字符拆，多字符 key 退化为「包含」匹配（只用于显示）。
function pickedSet(item, opts) {
  if (choices.has(item.id)) return choices.get(item.id);
  const raw = String(answers[item.id] || '').toUpperCase();
  const set = new Set();
  const allSingle = opts.every(o => o.key.length === 1);
  if (allSingle) {
    for (const ch of raw) if (opts.some(o => o.key === ch)) set.add(ch);
  } else {
    for (const o of opts) if (raw.includes(o.key.toUpperCase())) set.add(o.key);
  }
  choices.set(item.id, set);
  return set;
}

function renderText(item) {
  const ta = document.createElement('textarea');
  ta.dataset.qid = item.id;
  ta.value = answers[item.id] || '';
  ta.disabled = !editable;
  if (item.qtype === 'code') ta.className = 'code';
  ta.placeholder = item.qtype === 'fill' ? '每个空一行，按 ①②③ 顺序填写；自动保存'
    : item.qtype === 'fix' ? '每行一条：行号 改正后的代码（例如 3 return x*x;）；自动保存'
    : '在此作答（自动保存）';
  ta.addEventListener('input', () => setAnswer(item.id, ta.value));
  return ta;
}

/// 在 textarea **已经挂到 DOM 之后**初始化 CodeMirror。
function initCodeEditor(ta, item) {
  if (!window.CodeMirror) return;
  const lang = (item.code_languages && item.code_languages[0]) || '';
  const mode = lang === 'python' ? 'python' : ['c','cpp','java','javascript','typescript','go','rust'].includes(lang) ? 'text/x-csrc' : null;
  const cm = CodeMirror.fromTextArea(ta, { lineNumbers: true, mode, indentUnit: 4, tabSize: 4, lineWrapping: false, readOnly: !editable });
  cm.setSize('100%', '260px');
  cm.on('change', () => { if (editable) setAnswer(item.id, cm.getValue()); });
  editors.set(item.id, cm);
}

function setAnswer(qid, value) {
  if (!editable) return;
  answers[qid] = value;
  dirty.add(qid);
  scheduleSave();
}

// ---------------------------------------------------------------- 保存（含重试）
let saveTimer = null;
let retryTimer = null;
let retryDelay = 2000;
// 在途保存：同一时刻只允许一批。交卷要等它落地，否则「防抖已触发、PUT 还在飞」
// 的那一刻点交卷，服务端快照里会缺最后一题（随后那次 PUT 收到 409 被丢弃）。
let saving = null;
function scheduleSave() { if (submitted || !editable) return; clearTimeout(saveTimer); saveTimer = setTimeout(saveDirty, 600); }

async function saveDirty() {
  // 先看在途的那批：即使 dirty 已被它清空，也必须等它结束（它可能失败）。
  if (saving) return saving;
  if (submitted || !editable || !dirty.size) return true;
  saving = doSaveDirty();
  try { return await saving; } finally { saving = null; }
}

async function doSaveDirty() {
  const items = [...dirty]; dirty.clear();
  try {
    for (const qid of items) await api('/api/answer', { method: 'PUT', body: JSON.stringify({ qid, value: answers[qid] || '' }) });
    retryDelay = 2000;
    $('saveState').textContent = '已保存 ' + new Date().toLocaleTimeString();
    setNet('');
    return true;
  } catch (e) {
    // 失败就把这些题放回队列并退避重试；网络断时不要刷屏。
    items.forEach(q => dirty.add(q));
    $('saveState').textContent = '保存失败：' + e.message;
    if (e.status === 401) { needRelogin(); return false; }
    if (e.status === 409) {
      // 服务端已到点自动收卷：这一批答案不会再被接受，直接切「已交卷」页并说明原因。
      submitted = true;
      lockAll();
      setNet('');
      showDone(me);
      toast('已到结束时间，答题卡已自动封存');
      return false;
    }
    // 4xx（除 401/409）是确定性错误（413 单题超限、400 格式、403 无权限）：
    // 重试永远不会成功，只会刷屏并让学生以为「在重试、马上就好」。
    if (e.status && e.status >= 400 && e.status < 500) {
      setNet(`这一题保存不了（${e.message}）：请报告监考，或把答案改短后重试。`);
      return false;
    }
    // 服务端挂了与断网对学生的观感一样：必须显式告诉他「答案还在，正在重试」。
    setNet('保存失败（' + e.message + '）：作答暂存在本页，正在自动重试；请不要关闭页面。');
    retrySaveSoon();
    return false;
  }
}

function retrySaveSoon() {
  if (retryTimer || submitted) return;
  retryTimer = setTimeout(() => { retryTimer = null; saveDirty(); }, retryDelay);
  retryDelay = Math.min(retryDelay * 2, 20000);
}

// ---------------------------------------------------------------- 时钟（本地倒计时，轮询只校偏移）
let pollTimer = null;
let tickTimer = null;
// 到点自动交卷的退避：原来每秒重试一次，断网时会把日志/网络刷爆。
let autoSubmitDelay = 2000;
let autoSubmitAt = 0;
function updateClock() {
  const tick = () => {
    const remaining = endUnix ? endUnix - serverNow() : 0;
    $('clock').textContent = endUnix ? '剩余 ' + fmt(remaining) : '';
    if (endUnix && remaining <= 0 && !submitted && Date.now() >= autoSubmitAt) {
      autoSubmitAt = Date.now() + autoSubmitDelay;
      autoSubmitDelay = Math.min(autoSubmitDelay * 2, 20000);
      submit(true);
    }
  };
  tick();
  if (!tickTimer) tickTimer = setInterval(tick, 1000);
  if (!pollTimer) {
    pollTimer = setInterval(async () => {
      try {
        const t = await api('/api/time');
        serverOffset = (t.server_time || serverNow()) - Math.floor(Date.now()/1000);
        if (t.end_unix) endUnix = t.end_unix;
        // 服务端字段缺失时保持现状（不要因为 undefined 就误判）。
        if (t.submitted === true && !submitted) {
          submitted = true;
          lockAll();
          setNet('');
          showDone(me);
          toast('已到结束时间，答题卡已自动封存');
          return;
        }
        // 只有「没有待保存的题」时才清横幅：否则会把「保存失败，正在重试」的提示
        // 在 15 秒后悄悄抹掉，学生以为已经存上了。
        // 有 503 退避在跑时别清横幅：那条「答题卡正在封存，请不要关闭页面」正是
        // 学生此刻最需要看到的提示，被 15 s 轮询抹掉会让人以为已经交上了。
        if (!dirty.size && !resubmitTimer) setNet('');
      } catch (e) {
        // 轮询失败不回退本地计时：断线时学生更需要看到剩余时间。
        if (e && e.status === 401) { needRelogin(); return; }
        if (!navigator.onLine) setNet('网络已断开：作答暂存在本页，恢复后会自动保存。请不要关闭页面。');
      }
    }, 15000);
  }
}

function lockAll() {
  editable = false;
  document.querySelectorAll('textarea').forEach(t => t.disabled = true);
  document.querySelectorAll('.choice input').forEach(i => i.disabled = true);
  $('submitBtn').disabled = true;
  editors.forEach(cm => cm.setOption('readOnly', true));
}

// ---------------------------------------------------------------- 交卷
let submitting = false;
// 服务端在「已收卷但答题卡还没写盘」的那几毫秒~几十毫秒窗口里会回 503
// （到点 sweep 与手动交卷撞在一起）。这时**不能**当成交卷成功：卡可能随后回滚，
// 学生却已经看到「已交卷」；也不能当成交卷失败：什么都没坏，只是要等一下。
// 所以单独一条退避重试路径，成功前不置 submitted、不切页、不弹「交卷失败」。
let resubmitTimer = null;   // 待触发的重试（同一时刻只允许一个）
let resubmitAt = 0;         // 下一次允许重试的时刻：挡住退避期内的重复点击与每秒轮询
let resubmitDelay = 2000;   // 2 s → 20 s 指数退避
let resubmitAuto = false;   // 记住这次是「到点自动」还是「手动」：重试时别弹 confirm
async function submit(auto = false, skipConfirm = false) {
  if (submitted || submitting) return;
  // 退避期内：学生再点、或 updateClock 每秒调一次，都直接返回（避免并发提交）。
  // `resubmitTimer` 是唯一真相：存在就代表「已排好一次重试」，不再接受新的交卷请求；
  // 用时间戳比较会出现「定时器已到点但回调还没跑」与「回调里再次 503」互相踩的情况。
  if (resubmitTimer) return;
  // `skipConfirm` 只给 503 退避后的重试用：学生已经在第一次点交卷时确认过了，
  // 每 2–20 秒再弹一次「确定交卷吗」会让人以为页面出问题。
  if (!auto && !skipConfirm && !confirm('交卷后不可再修改，确定交卷吗？')) return;
  submitting = true;
  try {
    // 先把在途/未发的作答落地。保存失败时**不能默默拦死**：到点自动收卷仍会
    // 封存已保存的部分，学生如果坚持要交（比如断网且时间快到了），要给他一条路，
    // 但必须明确告知「这几题不会进卡」。
    const saved = await saveDirty();
    if (!saved && !submitted) {
      if (auto) return; // 到点自动交卷：断网横幅已在提示，等退避后再试
      const pending = dirty.size;
      const go = confirm(`还有 ${pending} 题没有保存成功，交卷后这些答案不会进答题卡。仍要交卷吗？`);
      if (!go) return;
    }
    await api('/api/submit', { method: 'POST', body: '{}' });
    // 交卷 API 一旦返回成功，状态就钉死：后面任何复查失败都不代表「交卷失败」。
    // 也要顺手清掉 503 退避状态（上一次 503 的定时器可能还挂着）。
    clearResubmit();
    submitted = true;
    toast('已交卷');
    showDone(me);
    // 503 退避期间挂着的「正在封存…」横幅必须收掉：否则「已交卷」页上还写着
    // 「请不要关闭页面，稍后会自动重试」，学生会以为没交上。
    setNet('');
    // 尽力而为：允许回看时用服务端卷面替换「已交卷」页（默认策略下这里会 401，属正常）。
    try {
      apply(await api('/api/paper'));
    } catch (e) {
      console.warn('交卷后复查卷面失败（不影响已交卷）', e);
    }
  } catch (e) {
    if (e.status === 401) { needRelogin(); }
    else if (e.status === 503) {
      // 正在封存：既不是成功也不是失败。置 submitted / showDone 都是错的（卡还没写盘）。
      scheduleResubmit(auto);
    } else { toast('交卷失败：' + e.message); }
  } finally { submitting = false; }
}

/// 收到 503（正在封存）后安排一次退避重试。
///
/// 手动与到点自动共用这条路径；`auto` 要原样带过去，否则重试会弹出
/// 「确定交卷吗？」——到点自动交卷不该问学生任何问题。
function scheduleResubmit(auto) {
  resubmitAuto = auto;
  // 横幅一直显示到成功为止：学生这时最需要知道「还没交上，但别关页面」。
  setNet('答题卡正在封存：请不要关闭页面，网络恢复后会自动重试交卷。');
  if (resubmitTimer) return; // 已经排好一次，别叠加
  const wait = resubmitDelay; // 用**本次**的退避值当等待时长，再翻倍给下一次
  resubmitDelay = Math.min(resubmitDelay * 2, 20000);
  resubmitAt = Date.now() + wait;
  toast('正在封存答题卡，请稍候…');
  resubmitTimer = setTimeout(() => {
    resubmitTimer = null;
    resubmitAt = 0;
    if (submitted) return;
    submit(resubmitAuto, true); // 已确认过，重试不再弹确认框
  }, wait);
}

/// 成功（或用户已进入别的终态）时清掉退避状态，避免留下一个迟到的重试。
function clearResubmit() {
  if (resubmitTimer) { clearTimeout(resubmitTimer); resubmitTimer = null; }
  resubmitAt = 0;
  resubmitDelay = 2000;
}

// 刷新/断线重连：带着会话 cookie 直接恢复，不必重新输学号。
async function boot() {
  try {
    apply(await api('/api/paper'));
  } catch (e) {
    if (e.status !== 401) setNet('无法连接服务器：' + e.message + '。请刷新页面重试。');
  }
}
boot();

$('loginBtn').addEventListener('click', login);
$('pin').addEventListener('keydown', e => { if (e.key === 'Enter') login(); });
$('sid').addEventListener('keydown', e => { if (e.key === 'Enter') login(); });
// 关页/切后台时把还没发出去的作答冲掉（keepalive 能在页面卸载后继续发）。
// 不做本地存储：服务端才是权威副本，这里只补「最后 0–600 ms」的缺口。
function flushBeacon() {
  if (submitted || !dirty.size) return;
  // 不等在途的那批：页面卸载时微任务不保证执行，`finally` 基本跑不到；
  // 同一题重复 PUT 是幂等的，宁可多发一次也不要丢。
  for (const qid of [...dirty]) {
    const body = JSON.stringify({ qid, value: answers[qid] || '' });
    const opts = {
      method: 'PUT',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json', 'X-EES-CSRF': '1' },
      body,
    };
    // Fetch 规范：keepalive 请求体上限 64 KiB，超了会被直接拒绝；
    // 大会答案（代码题粘贴）退回普通请求，至少有机会在页面卸载前发出去。
    if (body.length <= 64 * 1024) opts.keepalive = true;
    fetch('/api/answer', opts)
      .then(res => {
        // fetch 对 4xx/5xx 也会 resolve：不判 ok 就会把「没存上」当成「存上了」，
        // 该题从 dirty 里被删掉、再也不会重发，封存的卡里就少了这一题。
        if (res.ok) { dirty.delete(qid); return; }
        if (res.status === 409) { submitted = true; lockAll(); showDone(me); }
      })
      .catch(() => { /* 页面正在关闭，失败也没法再提示；dirty 保留，下次还能再试 */ });
  }
}
window.addEventListener('pagehide', flushBeacon);
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'hidden') flushBeacon();
});

// 主动退出：先冲尾，再让服务端删 token 并清 cookie，最后回登录页。
let loggingOut = false;
async function logout() {
  if (loggingOut) return;
  if (!confirm('退出后需要重新输入学号；已保存的答案不会丢。')) return;
  loggingOut = true;
  const saved = await saveDirty();
  if (!saved) {
    setNet('还有作答没保存成功：请等网络恢复后再退出。');
    loggingOut = false;
    return;
  }
  try {
    await api('/api/logout', { method: 'POST', body: '{}' });
  } catch (e) {
    // 退出失败不能假装成功：此时 reload 会带着仍然有效的 cookie 自动回到原卷。
    setNet('网络不可用，未能退出：请关闭浏览器或稍后重试。');
    loggingOut = false;
    return;
  }
  paper = null;
  dirty.clear();
  location.reload();
}

$('reloginBtn').addEventListener('click', () => { paper = null; location.reload(); });

$('submitBtn').addEventListener('click', () => submit(false));
$('logoutBtn').addEventListener('click', logout);
window.addEventListener('beforeunload', e => { if (!submitted && paper) { e.preventDefault(); e.returnValue = ''; } });
if (!navigator.onLine) setNet('网络已断开：作答暂存在本页，恢复后会自动保存。请不要关闭页面。');
