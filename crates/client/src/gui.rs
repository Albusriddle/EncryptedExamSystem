//! 学生端图形界面（egui/eframe）。
//!
//! 设计原则：**GUI 只是壳**。所有业务逻辑在 [`crate::workflow::unlock`]，
//! 这里只负责收集学号、把耗时操作丢到后台线程、把结果画出来。
//!
//! ## 为什么用 mpsc 而不是 Arc<Mutex<...>>
//!
//! `Receiver::try_recv()` 天然对应"还没好 / 好了 / 线程挂了"三种状态，
//! 不需要额外的状态位；而 egui 每帧都要 `update`，锁会成为性能隐患。
//!
//! ## 为什么必须 request_repaint_after
//!
//! egui 默认只在有输入事件时重绘。后台线程完成时没有输入事件，
//! 不主动请求重绘的话界面会一直停在"正在获取时间…"，看起来像卡死。

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui;

/// 诊断开关只存在于 debug 构建。
///
/// 这些开关曾长期在发布版里生效：`EES_AUTOLOGIN` 可免输学号、
/// `EES_DEBUG_*` 可把内部状态写到任意路径。它们不构成越权
/// （时间门禁照常执行），但把测试钩子留在发给学生的二进制里不合适。
#[cfg(debug_assertions)]
fn diag_env<T: std::str::FromStr>(key: &str) -> Option<T> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<T>().ok())
}

#[cfg(not(debug_assertions))]
fn diag_env<T: std::str::FromStr>(_key: &str) -> Option<T> {
    None
}

#[cfg(debug_assertions)]
fn diag_path(key: &str) -> Option<std::path::PathBuf> {
    std::env::var_os(key).map(std::path::PathBuf::from)
}

#[cfg(not(debug_assertions))]
fn diag_path(_key: &str) -> Option<std::path::PathBuf> {
    None
}

use crate::nav::NavState;
use crate::welcome::Welcome;
use crate::workflow::{self, KeySource, UnlockOpts, Unlocked};

/// 界面阶段。
enum Stage {
    /// 等待输入学号
    Idle {
        input: String,
        error: Option<String>,
    },
    /// 后台工作中
    Busy {
        rx: Receiver<Result<Unlocked, String>>,
        started: Instant,
    },
    /// 已解锁，展示试卷
    Paper(Box<Unlocked>),
}

/// 用单调时钟推进的"网络时间"。
///
/// 门禁只在解锁那一刻取一次网络时间；之后如果一直显示那个值，学生看到的
/// 时间就是**冻住的**——既看不出还剩多久，也会让人怀疑程序坏了。
///
/// 但**不能**直接用本机时钟（学生改系统时间就能骗过显示，甚至影响"到点收卷"）。
/// 正确做法：记下"同步时刻的网络时间"与"当时的单调时钟"，之后
/// `网络时间 = 基准 + 单调时钟已流逝的时长`。
///
/// `Instant` 是单调的，不受系统时间调整影响；机器休眠期间它照常计时
/// （这正是我们要的：睡过去的时间也算进考试时长）。
pub struct SyncedClock {
    /// 同步那一刻的网络时间（Unix 秒）
    base_unix: u64,
    /// 同步那一刻的单调时钟
    synced_at: std::time::Instant,
}

impl SyncedClock {
    pub fn new(base_unix: u64) -> Self {
        Self {
            base_unix,
            synced_at: std::time::Instant::now(),
        }
    }

    /// 当前网络时间（Unix 秒）。
    pub fn now_unix(&self) -> u64 {
        self.now_unix_at(self.synced_at.elapsed())
    }

    /// 从"同步时刻"起经过 `elapsed` 后的网络时间。
    ///
    /// 单独抽出来是为了能脱离真实时间测试推进逻辑——否则只能 sleep。
    pub fn now_unix_at(&self, elapsed: std::time::Duration) -> u64 {
        self.base_unix + elapsed.as_secs()
    }
}

pub struct ExamApp {
    stage: Stage,
    opts: UnlockOpts,
    /// 启动时预判的密钥来源，决定是否显示"开发模式"横幅。
    key_hint: KeySource,
    /// 顶部水印文本，构造时算一次即可。
    watermark: String,
    /// 启动欢迎弹窗（含"不再提示"）
    welcome: Welcome,
    /// 右侧题号导航卡
    nav: NavState,
    /// 诊断用：启动即自动解锁指定学号（`EES_AUTOLOGIN=<学号>`）。
    /// 仅供自动化验证/远程排障，正常启动不设置该变量。
    autologin: Option<u64>,
    /// 诊断用：首帧模拟点击题号 `EES_NAV_JUMP=<N>`
    jump_once: Option<usize>,
    /// 中文字体是否加载成功（false 时界面中文会变成方块，需要提示）
    cjk_ok: bool,
    /// 上一次写诊断文件的 (时间, 是否收卷)，用于避免每帧写盘
    last_state_dump: Option<(Option<u64>, bool)>,
    /// 上一次写"收卷页布局"诊断的 (窗口矩形, 卡片矩形)。
    ///
    /// 只有 debug 构建才读它（release 下整段诊断代码不编译），
    /// 所以字段本身也要跟着 cfg——否则 release 里"赋值后从未读取"会告警。
    #[cfg(debug_assertions)]
    last_locked_dump: Option<(egui::Rect, egui::Rect)>,
    /// 答题卡（解锁成功后才有）
    sheet: Option<common::answer_sheet::AnswerSheet>,
    /// 答题卡落盘路径
    sheet_path: Option<std::path::PathBuf>,
    /// 是否有未落盘的修改
    sheet_dirty: bool,
    /// 最近一次修改时间（用于 400ms 防抖）
    last_edit: std::time::Instant,
    /// 保存失败的原因（显示给学生/监考）
    save_error: Option<String>,
    /// 交卷二次确认：第一次点击后置 true，再点一次才真正冻结
    confirm_submit: bool,
    /// 网络时间时钟（解锁成功后才有；debug 跳过门禁时为 None）
    clock: Option<SyncedClock>,
    /// 是否已到结束时刻（到点后锁定试卷）
    expired: bool,
    /// 是否需要在下一帧把焦点交给学号输入框。
    ///
    /// 只在**首次启动**与**从错误返回 Idle** 时置 `true`。
    /// 早期实现是每帧都 `request_focus()`，结果学生按 Tab 或点击别处后
    /// 焦点立刻被抢回，根本无法离开输入框。
    focus_pending: bool,
}

impl ExamApp {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: UnlockOpts) -> Self {
        // 中文字体：egui 默认字体不含 CJK，必须手动加载系统字体，
        // 否则界面上所有中文都是"□"。
        let cjk_ok = install_cjk_font(&cc.egui_ctx);
        let key_hint = workflow::peek_key_source(&opts);
        Self {
            stage: Stage::Idle {
                input: String::new(),
                error: None,
            },
            opts,
            key_hint,
            watermark: common::watermark(),
            welcome: Welcome::new(),
            nav: NavState::default(),
            jump_once: diag_env::<usize>("EES_NAV_JUMP").filter(|v| *v > 0),
            autologin: diag_env::<u64>("EES_AUTOLOGIN").filter(|v| *v > 0),
            cjk_ok,
            sheet: None,
            sheet_path: None,
            sheet_dirty: false,
            last_edit: std::time::Instant::now(),
            save_error: None,
            confirm_submit: false,
            last_state_dump: None,
            #[cfg(debug_assertions)]
            last_locked_dump: None,
            clock: None,
            expired: false,
            focus_pending: true,
        }
    }

    /// 启动后台解锁。
    fn start_unlock(&mut self, student_id: u64) {
        let (tx, rx) = mpsc::channel();
        let opts = self.opts.clone();
        std::thread::spawn(move || {
            // 结果转成 String 错误：anyhow::Error 不是 Send 的友好类型，
            // 跨线程传递用展示文本足够。
            let result = workflow::unlock(student_id, &opts).map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
        });
        self.stage = Stage::Busy {
            rx,
            started: Instant::now(),
        };
    }

    /// 轮询后台结果。返回 `true` 表示状态有变化。
    fn poll(&mut self) -> bool {
        let Stage::Busy { rx, .. } = &self.stage else {
            return false;
        };
        match rx.try_recv() {
            Ok(Ok(unlocked)) => {
                // 换卷：清掉上一份试卷残留的矩形与跳转请求
                self.nav.reset_for_new_paper();
                // 以解锁时取到的网络时间为基准建时钟；之后由单调时钟推进
                self.clock = unlocked.net.as_ref().map(|n| SyncedClock::new(n.unix));
                self.expired = false;
                let now = self.clock.as_ref().map(|c| c.now_unix()).unwrap_or(0);
                let path = answer_sheet_path(
                    unlocked.student_id,
                    unlocked.session_id,
                    &unlocked.paper.fingerprint(),
                );
                // 已有答题卡且是同一份卷：恢复；否则新建
                let restored = std::fs::read(&path)
                    .ok()
                    .and_then(|b| common::answer_sheet::open(&unlocked.k_manifest, &b).ok())
                    .filter(|s| s.paper_fingerprint == unlocked.paper.fingerprint());
                if restored.is_none() && path.exists() {
                    // 换卷/换包时旧卡不能默默丢掉：留一份 .bak，学生换回原包还能恢复
                    let _ = std::fs::rename(&path, path.with_extension("eea.bak"));
                }
                self.sheet = Some(match restored {
                    Some(s) => s,
                    None => common::answer_sheet::AnswerSheet::from_paper(
                        &unlocked.paper,
                        unlocked.student_id,
                        unlocked.session_id,
                        now,
                    ),
                });
                // 诊断用：EES_ANSWER="0=B,1=AC" 预填答案，仅供自动化验证（release 不含）
                #[cfg(debug_assertions)]
                if let Ok(spec) = std::env::var("EES_ANSWER")
                    && let Some(sh) = self.sheet.as_mut()
                {
                    {
                        for part in spec.split(',') {
                            if let Some((idx, val)) = part.split_once('=')
                                && let Ok(i) = idx.parse::<usize>()
                            {
                                sh.set_answer(i, Some(val.to_string()), 1);
                            }
                        }
                    }
                }
                // 新建/恢复后立即落盘：万一学生答了两题就断电，磁盘上也有一份
                self.sheet_dirty = true;
                self.last_edit = std::time::Instant::now()
                    .checked_sub(Duration::from_secs(5))
                    .unwrap_or_else(std::time::Instant::now);
                self.sheet_path = Some(path);
                self.save_error = None;
                self.confirm_submit = false;
                self.stage = Stage::Paper(Box::new(unlocked));
                true
            }
            Ok(Err(msg)) => {
                self.stage = Stage::Idle {
                    input: String::new(),
                    error: Some(msg),
                };
                // 回到输入态时重新聚焦，方便学生改学号。
                self.focus_pending = true;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.stage = Stage::Idle {
                    input: String::new(),
                    error: Some("后台线程异常退出（可能是内存不足）".into()),
                };
                self.focus_pending = true;
                true
            }
        }
    }
}

impl eframe::App for ExamApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // 关窗前立即落盘，避免丢掉最后 400ms 的输入
        self.save_sheet_now();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 欢迎弹窗置顶绘制；关闭时把焦点还给学号输入框
        if self.welcome.show(ctx) {
            self.focus_pending = true;
        }

        // 诊断：启动即自动解锁（跳过人工输入学号）
        if let Some(sid) = self.autologin.take() {
            self.start_unlock(sid);
        }

        // 后台工作期间主动请求重绘，否则界面不刷新。
        if matches!(self.stage, Stage::Busy { .. }) {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        self.poll();

        if let Some(err) = &self.save_error {
            egui::TopBottomPanel::top("save_error").show(ctx, |ui| {
                ui.colored_label(egui::Color32::from_rgb(200, 60, 60), err);
            });
        }

        // 水印条带：声明在最前，所以位于窗口最顶部。
        egui::TopBottomPanel::top("watermark").show(ctx, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(&self.watermark)
                            .size(11.0)
                            .color(egui::Color32::from_gray(130)),
                    );
                });
            });
            ui.add_space(2.0);
        });

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading(common::branding().display_name());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // 只有在"需要学生自己出密钥"时才提示开发模式；
                    // 包内嵌/编译期内嵌都是正式分发形态，不该吓学生。
                    if matches!(self.key_hint, KeySource::Master) {
                        ui.colored_label(
                            egui::Color32::from_rgb(200, 120, 0),
                            "开发模式（需要密钥文件）",
                        );
                    }
                });
            });
            ui.add_space(6.0);
        });

        // 闭包内只读 `self.stage` 并记录"要做什么"，
        // 真正的状态切换放到闭包外面——否则 `self.start_unlock(...)`
        // 会和 `&mut self.stage` 的借用冲突。
        let mut submit: Option<String> = None;
        // 焦点标志要在闭包内外传递，先取到局部变量：
        // 闭包里已经 `&mut self.stage`，不能再借 `self.focus_pending`。
        let mut want_focus = self.focus_pending;

        egui::CentralPanel::default().show(ctx, |ui| match &mut self.stage {
            Stage::Idle { input, error } => {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    // 字体缺失时中文全是方块，此时中文提示也没用——用英文说明。
                    if !self.cjk_ok {
                        ui.colored_label(
                            egui::Color32::from_rgb(200, 60, 60),
                            "CJK font not found: Chinese text may appear as boxes. \
                             Install Microsoft YaHei / SimHei and restart.",
                        );
                        ui.add_space(10.0);
                    }
                    ui.label(egui::RichText::new("请输入学号").size(18.0));
                    ui.add_space(12.0);

                    let resp = ui.add(
                        egui::TextEdit::singleline(input)
                            .hint_text("学号")
                            .desired_width(240.0)
                            .font(egui::TextStyle::Heading),
                    );
                    // 只在需要时聚焦一次（不是每帧），否则无法用 Tab 切走。
                    if want_focus {
                        resp.request_focus();
                        want_focus = false;
                    }

                    ui.add_space(16.0);

                    let clicked = ui
                        .add_sized([240.0, 40.0], egui::Button::new("开始考试"))
                        .clicked();
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                    if clicked || enter {
                        submit = Some(input.clone());
                    }

                    if let Some(e) = error {
                        ui.add_space(14.0);
                        ui.colored_label(egui::Color32::from_rgb(200, 60, 60), e.as_str());
                    }

                    ui.add_space(28.0);
                    ui.label(
                        egui::RichText::new("提示：本场考试按网络时间自动判定，无需选择场次。")
                            .size(12.0)
                            .color(egui::Color32::GRAY),
                    );
                });
            }

            Stage::Busy { started, .. } => {
                ui.add_space(80.0);
                ui.vertical_centered(|ui| {
                    ui.spinner();
                    ui.add_space(16.0);
                    let elapsed = started.elapsed().as_secs_f32();
                    // 分阶段提示：取时约 0.25s，Argon2 约 0.1s；
                    // 超过 3 秒说明网络慢，给学生一个心理预期。
                    let msg = if elapsed < 1.0 {
                        "正在获取网络时间…"
                    } else if elapsed < 3.0 {
                        "正在校验考试时间与名单…"
                    } else {
                        "网络较慢，仍在尝试获取可信时间…"
                    };
                    ui.label(egui::RichText::new(msg).size(16.0));
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(format!("已用时 {elapsed:.1} 秒"))
                            .size(12.0)
                            .color(egui::Color32::GRAY),
                    );
                });
            }

            Stage::Paper(_) => {}
        });

        self.focus_pending = want_focus;

        // 试卷阶段：题号卡与试卷需要在同一个 pass 里按顺序渲染，
        // 因此不放进 CentralPanel 的闭包，而是单独处理。
        if let Stage::Paper(unlocked) = &self.stage {
            // 每秒重绘：网络时间要"走"，到点要自动收卷。
            // 没有这一行，界面只会在鼠标动的时候刷新，时间看起来是冻住的。
            ctx.request_repaint_after(std::time::Duration::from_secs(1));

            let now = self.clock.as_ref().map(|c| c.now_unix());
            if !self.expired && now.is_some_and(|n| workflow::is_expired(n, unlocked.end_unix)) {
                self.expired = true;
                if let Some(sheet) = self.sheet.as_mut()
                    && !sheet.submitted
                {
                    sheet.freeze(now_or_wall_clock(now));
                    self.sheet_dirty = true;
                }
                // 标题栏也换成"已结束"：学生切到别的窗口后，一眼能看出状态；
                // 同时让 release 包的行为可以被外部自动化验证（读窗口标题）。
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                    "{} · 考试已结束",
                    common::branding().product
                )));
            }

            // 诊断放在收卷判断**之前**：这样收卷后 dump 仍在更新，
            // 能验证"时间在走 + 到点变 expired=true"，不依赖截图。
            // 只在"秒数或收卷状态变化"时写盘：这一帧会因为鼠标移动而
            // 每秒重绘几十次，每帧都写就是几十次磁盘 IO。
            let snapshot = (now, self.expired);
            if self.last_state_dump != Some(snapshot) {
                self.last_state_dump = Some(snapshot);
                if let Some(path) = diag_path("EES_DEBUG_NAV") {
                    // 单独一个 .state 文件：完整 dump 只在渲染试卷时写，
                    // 收卷后就不写了，无法用来观察"到点"这件事。
                    let state_path = path.with_extension("state");
                    self.nav.dump_state(&state_path, now, self.expired);
                }
            }

            if self.expired {
                // 到点收卷：直接关掉试卷与题号卡，避免"时间到了还能看题"。
                let mut card_rect = egui::Rect::NOTHING;
                let mut panel_rect = egui::Rect::NOTHING;
                egui::CentralPanel::default().show(ctx, |ui| {
                    panel_rect = ui.max_rect();
                    card_rect = render_locked(ui, unlocked, now);
                });
                // 布局自检：卡片必须水平居中、且落在下半屏、且不被裁掉。
                #[cfg(debug_assertions)]
                if self.last_locked_dump != Some((panel_rect, card_rect))
                    && let Some(path) = diag_path("EES_DEBUG_NAV")
                {
                    self.last_locked_dump = Some((panel_rect, card_rect));
                    let out = path.with_extension("locked");
                    let _ = std::fs::write(
                        &out,
                        format!(
                            "window={panel_rect:?}\ncard={card_rect:?}\n\
                             center_dx={:.1}\ncard_top_ratio={:.3}\ncard_bottom_ratio={:.3}\n",
                            card_rect.center().x - panel_rect.center().x,
                            (card_rect.min.y - panel_rect.min.y) / panel_rect.height(),
                            (card_rect.max.y - panel_rect.min.y) / panel_rect.height(),
                        ),
                    );
                }
                let _ = (card_rect, panel_rect);
                self.maybe_save_sheet();
                return;
            }

            let sections: Vec<(String, usize)> = unlocked
                .paper
                .sections
                .iter()
                .map(|s| (s.title.clone(), s.items.len()))
                .collect();

            // 诊断：模拟点击某个题号（验证跳转链路）
            if let Some(idx) = self.jump_once.take() {
                self.nav.debug_jump(idx);
            }

            // ① 题号卡（点击只写 pending，不立即滚动）
            self.nav.show(ctx, &sections);

            // ② 试卷：逐题记录矩形；渲染完成后在同一个 Ui 里消费 pending。
            //    必须同一个 CentralPanel：一个 pass 只能有一个 CentralPanel。
            let mut scroll_offset = egui::Vec2::ZERO;
            let mut sheet_changed = false;
            egui::CentralPanel::default().show(ctx, |ui| {
                if let Some(sheet) = self.sheet.as_mut() {
                    // 已交卷的卡即便还没到结束时间也不能改——界面要看起来也不可改，
                    // 否则监考会以为"点了没反应"是程序坏了。
                    let editable = !self.expired && !sheet.submitted;
                    let (off, ch) = render_paper(
                        ui,
                        unlocked,
                        &mut self.nav,
                        now,
                        sheet,
                        editable,
                        &mut self.confirm_submit,
                        now_or_wall_clock(now),
                    );
                    scroll_offset = off;
                    if ch {
                        sheet_changed = true;
                    }
                }
            });

            if sheet_changed {
                // 改动只标记脏；真正写盘由 update() 里的 400ms 防抖统一处理
                self.sheet_dirty = true;
                self.last_edit = std::time::Instant::now();
            }

            // 诊断：EES_DEBUG_NAV=<文件> 时输出题号卡与题目矩形，
            // 便于验证"跳转目标是否被正确记录"（截屏在 DPI 下不可靠）。
            if let Some(path) = diag_path("EES_DEBUG_NAV") {
                self.nav.dump_debug(&sections, scroll_offset, &path);
            }
        }

        self.maybe_save_sheet();
        if self.sheet_dirty {
            // 防抖期间也要有重绘，否则学生停手后没人触发保存
            ctx.request_repaint_after(Duration::from_millis(200));
        }

        // 闭包结束后再切状态。
        if let Some(raw) = submit {
            let trimmed = raw.trim();
            match trimmed.parse::<u64>() {
                Ok(0) => {
                    self.stage = Stage::Idle {
                        input: raw,
                        error: Some("学号必须是正整数".into()),
                    };
                    self.focus_pending = true;
                }
                Ok(sid) => self.start_unlock(sid),
                Err(_) => {
                    self.stage = Stage::Idle {
                        input: raw,
                        error: Some("学号必须是纯数字".into()),
                    };
                    self.focus_pending = true;
                }
            }
        }
    }
}

/// 渲染试卷。
/// 试卷底部留白（pt）。约一屏高度，让最后一题也能贴顶。
const BOTTOM_PAD: f32 = 900.0;

/// 把秒数格式化成"X 小时 Y 分 Z 秒"。
fn format_duration(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h} 小时 {m} 分 {s} 秒")
    } else if m > 0 {
        format!("{m} 分 {s} 秒")
    } else {
        format!("{s} 秒")
    }
}

/// 到点后的锁定页：不再渲染任何题目。
///
/// 页面下半部放一块圆角小卡片，内容与启动欢迎弹窗一致（版本/作者/许可证等）。
/// 用 `Frame` 而不是浮层：它在流式布局里，天然不会盖住上面的"已结束"信息。
///
/// 返回卡片矩形（供 debug 自检）。
fn render_locked(ui: &mut egui::Ui, unlocked: &Unlocked, now: Option<u64>) -> egui::Rect {
    /// 卡片内容宽度的上限（不含内边距）
    const CARD_MAX_W: f32 = 380.0;

    let mut card = egui::Rect::NOTHING;

    // 窗口被拉得很矮时（或卡片比剩余空间高），用滚动兜底：
    // 宁可滚动，也不能把卡片裁掉一半——裁掉等于信息丢失。
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(36.0);
        ui.vertical_centered(|ui| {
            ui.heading("本场考试已结束");
            ui.add_space(14.0);
            ui.label(format!("场次 #{}", unlocked.session_id));
            ui.label(format!("结束时间：{}", unlocked.end));
            if let Some(n) = now {
                ui.label(format!(
                    "当前网络时间：{}",
                    common::timegate::format_unix_local_zoned(n)
                ));
            }
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new("试卷已关闭，无法继续查看。如需帮助请联系监考老师。")
                    .color(egui::Color32::GRAY),
            );
        });

        // 居中偏下：把卡片推到剩余空间的下半部。
        // 注意：这里在 ScrollArea 内部，available_height() 是"视口剩余高度"。
        // 取 25% 作为留白，卡片整体的中心大约落在窗口的 60% 高度处（偏下）。
        let left = ui.available_height();
        ui.add_space((left * 0.25).max(16.0));

        // 窗口很窄时卡片跟着缩，避免撑出横向滚动条
        let card_w = (ui.available_width() - 40.0).clamp(240.0, CARD_MAX_W);
        ui.vertical_centered(|ui| {
            let frame = egui::Frame::none()
                .rounding(egui::Rounding::same(12.0))
                .inner_margin(egui::Margin::same(14.0))
                .fill(ui.visuals().extreme_bg_color)
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    ui.visuals().widgets.noninteractive.bg_stroke.color,
                ));
            let resp = frame.show(ui, |ui| {
                ui.set_width(card_w);
                crate::welcome::brand_body(ui);
            });
            card = resp.response.rect;
        });
    });
    card
}

/// 渲染一道题的作答区。返回是否有改动。
///
/// 编码规则（与 `common::answer_sheet` 一致）：
/// - 单选/判断：`Some("B")`
/// - 多选：`Some("AC")`（升序拼接）
/// - 补全/改错：JSON 数组文本
fn render_answer_inputs(
    ui: &mut egui::Ui,
    item: &common::picker::PaperItem,
    sheet: &mut common::answer_sheet::AnswerSheet,
    idx: usize,
    editable: bool,
    now_unix: u64,
) -> bool {
    use common::answer_sheet::{decode_fixes, decode_parts, encode_fixes, encode_parts};
    use common::model::QuestionBody;

    if idx >= sheet.answers.len() {
        return false;
    }
    let mut changed = false;

    match &item.body {
        QuestionBody::Options { options } => {
            let current = sheet.answers[idx].clone().unwrap_or_default();
            let multiple = item.qtype == "multiple";
            let mut selected: Vec<String> = if multiple {
                current.chars().map(|c| c.to_string()).collect()
            } else {
                Vec::new()
            };
            for opt in options {
                let on = if multiple {
                    selected.iter().any(|k| k == &opt.key)
                } else {
                    current == opt.key
                };
                let text = format!("{}. {}", opt.key, opt.text);
                if ui
                    .add_enabled(editable, egui::SelectableLabel::new(on, text))
                    .clicked()
                {
                    if multiple {
                        if let Some(pos) = selected.iter().position(|k| k == &opt.key) {
                            selected.remove(pos);
                        } else {
                            selected.push(opt.key.clone());
                        }
                        selected.sort();
                        let joined = selected.concat();
                        let value = if joined.is_empty() {
                            None
                        } else {
                            Some(joined)
                        };
                        if sheet.set_answer(idx, value, now_unix) {
                            changed = true;
                        }
                    } else if sheet.set_answer(idx, Some(opt.key.clone()), now_unix) {
                        changed = true;
                    }
                }
            }
        }
        QuestionBody::Template { blank_count, .. } => {
            let n = *blank_count as usize;
            let mut parts = sheet.answers[idx]
                .as_deref()
                .map(decode_parts)
                .unwrap_or_default();
            parts.resize(n, String::new());
            let before = parts.clone();
            ui.horizontal_wrapped(|ui| {
                for (k, part) in parts.iter_mut().enumerate() {
                    ui.label(format!("{}、", blank_marker(k)));
                    ui.add_enabled(
                        editable,
                        egui::TextEdit::singleline(part)
                            .desired_width(160.0)
                            .hint_text("作答"),
                    );
                }
            });
            if parts != before && sheet.set_answer(idx, Some(encode_parts(&parts)), now_unix) {
                changed = true;
            }
        }
        QuestionBody::Code { blank_count, .. } => {
            let n = *blank_count as usize;
            let mut fixes = sheet.answers[idx]
                .as_deref()
                .map(decode_fixes)
                .unwrap_or_default();
            fixes.resize(n, (String::new(), String::new()));
            let before = fixes.clone();
            for (k, (line, text)) in fixes.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("第 {} 处：", k + 1));
                    ui.add_enabled(
                        editable,
                        egui::TextEdit::singleline(line)
                            .desired_width(48.0)
                            .hint_text("行号"),
                    );
                    ui.add_enabled(
                        editable,
                        egui::TextEdit::singleline(text)
                            .desired_width(320.0)
                            .hint_text("改为"),
                    );
                });
            }
            if fixes != before && sheet.set_answer(idx, Some(encode_fixes(&fixes)), now_unix) {
                changed = true;
            }
        }
        QuestionBody::Text => {}
    }
    changed
}

/// 空位序号：0→①，1→② …（超过 10 个空时退化为 ①+数字）
fn blank_marker(k: usize) -> String {
    const MARKS: [&str; 10] = ["①", "②", "③", "④", "⑤", "⑥", "⑦", "⑧", "⑨", "⑩"];
    MARKS
        .get(k)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("①+{k}"))
}

/// 按题目形态渲染正文。
///
/// 三种形态的差别不是"排版喜好"，而是**信息结构**：
/// - 选项题：键（A/B/C/D）是答案的坐标，必须原样显示；
/// - 补全题：`____①____` 是作答位置的锚点，必须等宽显示，否则对不齐；
/// - 改错题：行号是"第几行错了"的唯一坐标，必须逐行带号。
fn render_body(ui: &mut egui::Ui, item: &common::picker::PaperItem) {
    use common::model::QuestionBody;

    match &item.body {
        QuestionBody::Options { options } => {
            for opt in options {
                ui.label(format!("{}. {}", opt.key, opt.text));
            }
        }
        QuestionBody::Template {
            template,
            blank_count,
        } => {
            ui.label(
                egui::RichText::new(format!(
                    "本小题有 {blank_count} 处空白，请在答题纸上按 ①②③ 顺序作答"
                ))
                .size(12.0)
                .color(egui::Color32::GRAY),
            );
            egui::Frame::none()
                .fill(ui.visuals().extreme_bg_color)
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    // 横向滚动：代码行不能被裁掉，裁掉等于看不到后半句
                    egui::ScrollArea::horizontal().show(ui, |ui| {
                        ui.label(egui::RichText::new(template).monospace().size(13.0));
                    });
                });
        }
        QuestionBody::Code { code, blank_count } => {
            ui.label(
                egui::RichText::new(format!("本小题有 {blank_count} 处错误，请指出行号并改正"))
                    .size(12.0)
                    .color(egui::Color32::GRAY),
            );
            egui::Frame::none()
                .fill(ui.visuals().extreme_bg_color)
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    egui::ScrollArea::horizontal().show(ui, |ui| {
                        for (i, line) in code.iter().enumerate() {
                            // 行号右对齐到 3 位，代码才能对齐成块
                            ui.label(
                                egui::RichText::new(format!("{:>3}  {}", i + 1, line))
                                    .monospace()
                                    .size(13.0),
                            );
                        }
                    });
                });
        }
        QuestionBody::Text => {
            // 旧题库：没有 body，只有 options 数组
            for (j, opt) in item.options.iter().enumerate() {
                ui.label(format!("{}. {opt}", workflow::option_label(j)));
            }
        }
    }
}

/// 渲染试卷。返回滚动区域的当前偏移，便于诊断验证"跳转是否真的滚动了"。
#[allow(clippy::too_many_arguments)]
fn render_paper(
    ui: &mut egui::Ui,
    unlocked: &Unlocked,
    nav: &mut crate::nav::NavState,
    now: Option<u64>,
    sheet: &mut common::answer_sheet::AnswerSheet,
    editable: bool,
    confirm_submit: &mut bool,
    now_unix: u64,
) -> (egui::Vec2, bool) {
    let mut changed = false;
    let mut qidx = 0usize;
    let out = egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("学号 {}", unlocked.student_id)).strong());
            ui.separator();
            ui.label(format!("场次 #{}", unlocked.session_id));
            ui.separator();
            ui.label(format!("{} ~ {}", unlocked.start, unlocked.end));
        });

        if let Some(net) = &unlocked.net {
            // 显示的是 SyncedClock 推进后的"现在"，不是解锁那一刻的固定值。
            let shown = now.unwrap_or(net.unix);
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "网络时间 {}（{} 个源，分歧 {} 秒）",
                    common::timegate::format_unix_local_zoned(shown),
                    net.sources,
                    net.spread
                ))
                .size(12.0)
                .color(egui::Color32::GRAY),
            );

            if now.is_some() {
                let left = workflow::remaining_secs(shown, unlocked.end_unix);
                let text = format!("距离结束还有 {}", format_duration(left));
                // 最后 5 分钟标红，给学生明确的紧迫感
                let color = if left <= 300 {
                    egui::Color32::from_rgb(200, 60, 60)
                } else {
                    egui::Color32::GRAY
                };
                ui.label(egui::RichText::new(text).size(12.0).color(color));
            }
        }

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!("试卷指纹 {}", unlocked.paper.fingerprint()))
                .size(11.0)
                .color(egui::Color32::GRAY),
        );

        // 作答进度 + 交卷（二次确认，避免误点）
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("已作答 {}/{}", sheet.answered(), sheet.qids.len()))
                    .size(13.0),
            );
            if sheet.submitted {
                ui.label(
                    egui::RichText::new("✔ 已交卷（答题卡已保存）")
                        .size(13.0)
                        .color(egui::Color32::from_rgb(60, 160, 90)),
                );
            } else if editable {
                ui.separator();
                let label = if *confirm_submit {
                    "确认交卷（不可再修改）"
                } else {
                    "交卷"
                };
                if ui.button(label).clicked() {
                    if *confirm_submit {
                        sheet.freeze(now_unix);
                        *confirm_submit = false;
                        changed = true;
                    } else {
                        *confirm_submit = true;
                    }
                }
                if *confirm_submit && ui.button("取消").clicked() {
                    *confirm_submit = false;
                }
            }
        });

        ui.add_space(12.0);
        ui.separator();

        let mut seq = 0usize;
        for sec in &unlocked.paper.sections {
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new(format!(
                    "【{}】共 {} 题，{} 分",
                    sec.title,
                    sec.items.len(),
                    sec.total_score()
                ))
                .size(16.0)
                .strong(),
            );
            ui.add_space(6.0);
            for item in &sec.items {
                seq += 1;
                let this_index = qidx;
                qidx += 1;
                // 记录整道题（题干 + 选项）的矩形，供题号卡跳转用。
                // 用 group 把一道题的控件包起来，拿到的是一个整体 rect，
                // 比只取题干那一行的 rect 更准确（跳过去能看到完整题目）。
                let resp = ui
                    .push_id(format!("item{seq}"), |ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{seq}. ({}) {}",
                                    item.score, item.stem
                                ))
                                .size(15.0),
                            );
                            ui.indent(format!("q{seq}"), |ui| {
                                render_body(ui, item);
                                if render_answer_inputs(
                                    ui, item, sheet, this_index, editable, now_unix,
                                ) {
                                    changed = true;
                                }
                            });
                        })
                    })
                    .response;
                nav.record(seq, resp.rect);
                ui.add_space(8.0);
            }
        }
        // 底部留白：让**最后一题也能真正置顶**。
        //
        // 滚动区最多只能滚到 `内容高 - 视窗高`；若最后几道题之后的空白
        // 不足一屏，跳过去时会被"顶到上限"，题号卡点了也到不了顶部。
        // 实测 10 道题时第 10 题只能滚到 553，而它的目标位置是 1124。
        // 这里按视窗高度留出一屏空白，使任意题目都能贴顶。
        // 固定留白一屏：ScrollArea 内部的 available_height() 在内容撑开后
        // 会变成 0，用它算留白几乎不生效（实测只多了 20pt）。
        // 这里改用固定值，保证任意题目都能滚到顶部。
        ui.add_space(BOTTOM_PAD);

        // 全部题目渲染完毕，此时 rects 是本帧最新的，消费跳转请求。
        nav.apply_pending_scroll(ui);
    });
    (out.state.offset, changed)
}

/// 中文字体候选（按优先级）。
const CJK_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\msyh.ttc", // 微软雅黑
    r"C:\Windows\Fonts\msyhbd.ttc",
    r"C:\Windows\Fonts\simhei.ttf", // 黑体
    r"C:\Windows\Fonts\simsun.ttc", // 宋体
    "/System/Library/Fonts/PingFang.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
];

/// 安装中文字体；返回是否成功。
///
/// egui 自带字体只覆盖拉丁字母，中文会显示成豆腐块。
/// 找不到任何候选字体时**不 panic**，但要在界面上提示——否则学生只看到
/// 一屏方块，不知道为什么（中文提示本身也是方块，所以提示用英文）。
fn install_cjk_font(ctx: &egui::Context) -> bool {
    install_cjk_font_from(ctx, CJK_CANDIDATES)
}

fn install_cjk_font_from(ctx: &egui::Context, candidates: &[&str]) -> bool {
    let mut fonts = egui::FontDefinitions::default();
    for path in candidates {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        fonts
            .font_data
            .insert("cjk".to_owned(), egui::FontData::from_owned(bytes));
        // 插到最前面：优先用 CJK 字体渲染，缺字时回退到默认。
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "cjk".to_owned());
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .push("cjk".to_owned());
        ctx.set_fonts(fonts);
        return true;
    }
    eprintln!("⚠ 未找到系统中文字体，界面中文可能显示为方块");
    false
}

/// 启动 GUI。
pub fn run(opts: UnlockOpts) -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 720.0])
            .with_min_inner_size([600.0, 480.0])
            .with_title(common::branding().product.clone()),
        ..Default::default()
    };
    eframe::run_native(
        &common::branding().product,
        native_options,
        Box::new(move |cc| Ok(Box::new(ExamApp::new(cc, opts)))),
    )
}

#[cfg(test)]
mod clock_tests {
    use super::SyncedClock;
    use std::time::Duration;

    /// 网络时间必须随时间推进，而**不是**固定在解锁那一刻。
    #[test]
    fn clock_advances_with_elapsed() {
        let c = SyncedClock::new(1_000_000);
        assert_eq!(c.now_unix_at(Duration::from_secs(0)), 1_000_000);
        assert_eq!(c.now_unix_at(Duration::from_secs(1)), 1_000_001);
        assert_eq!(c.now_unix_at(Duration::from_secs(3_600)), 1_003_600);
        // 亚秒不进位（显示到秒即可）
        assert_eq!(c.now_unix_at(Duration::from_millis(999)), 1_000_000);
    }

    /// 推进后的时间要能与结束时刻比较，触发收卷。
    #[test]
    fn clock_reaches_end() {
        let c = SyncedClock::new(1_000_000);
        let end = 1_000_010;
        assert!(!crate::workflow::is_expired(
            c.now_unix_at(Duration::from_secs(9)),
            end
        ));
        assert!(crate::workflow::is_expired(
            c.now_unix_at(Duration::from_secs(10)),
            end
        ));
    }
}

#[cfg(test)]
mod font_tests {
    use super::{CJK_CANDIDATES, install_cjk_font_from};
    use eframe::egui;

    /// 候选表不能是空的，否则永远加载不到字体。
    #[test]
    fn candidates_not_empty() {
        assert!(CJK_CANDIDATES.len() >= 3);
    }

    /// 找不到字体时必须返回 false（界面据此显示英文提示），而不是 panic。
    #[test]
    fn missing_font_reports_failure() {
        let ctx = egui::Context::default();
        assert!(!install_cjk_font_from(&ctx, &["/definitely/not/here.ttf"]));
        assert!(!install_cjk_font_from(&ctx, &[]));
    }
}

#[cfg(test)]
mod brand_wiring {
    /// 收卷页与欢迎弹窗必须共用同一份品牌内容。
    ///
    /// 两处各写一遍的话，改版权信息时必然漏掉一处——这里用"读源码"的接线测试守住。
    #[test]
    fn locked_page_reuses_welcome_brand_body() {
        let gui_src = include_str!("gui.rs");
        assert!(
            gui_src.contains("crate::welcome::brand_body(ui)"),
            "收卷页应调用 welcome::brand_body，而不是自己另写一份"
        );
        let welcome_src = include_str!("welcome.rs");
        assert!(
            welcome_src.contains("pub fn brand_body"),
            "brand_body 应定义在 welcome.rs"
        );
    }

    /// 产品名必须来自 branding.json：改名时窗口标题要跟着变。
    #[test]
    fn product_name_comes_from_branding() {
        let gui_src = include_str!("gui.rs");
        assert!(
            gui_src.contains("common::branding().display_name()"),
            "主标题应使用 branding().display_name()"
        );
        assert!(
            gui_src.contains("with_title(common::branding().product"),
            "窗口标题应使用 branding().product"
        );
        // rustfmt 会把 format! 拆成多行，所以分两段断言
        assert!(
            gui_src.contains("\"{} · 考试已结束\""),
            "收卷标题的模板应保留"
        );
        assert!(
            gui_src.matches("common::branding().product").count() >= 2,
            "窗口标题与收卷标题都应使用 branding().product"
        );
    }
}

/// 答题卡落盘路径。
///
/// 优先放在**学生端所在目录**（监考可以直接拷走），不可写时回退到
/// `%LOCALAPPDATA%\ExamSystem\answers\`：机房常见 Program Files、只读 U 盘、
/// 光盘，直接写同目录会失败。
/// 没有网络时钟时（debug / 跳门禁）回退到本机时间，避免写出 1970 的时间戳。
fn now_or_wall_clock(now: Option<u64>) -> u64 {
    now.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    })
}

fn answer_sheet_path(
    student_id: u64,
    session_id: u32,
    paper_fingerprint: &str,
) -> std::path::PathBuf {
    let name = answer_sheet_file_name(student_id, session_id, paper_fingerprint);

    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
        && is_writable(dir)
    {
        return dir.join(&name);
    }

    let base = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let dir = base.join("ExamSystem").join("answers");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(name)
}

/// 答题卡文件名。
///
/// **必须带上试卷指纹**：同一学生同一场次在不同包里都叫 `answers-<学号>-<场次>.eea`，
/// 老师在同一台机器上试开第二份包就会把学生的真实答案覆盖掉（实测：第二份包
/// 会把第一份挤成 `.bak`，第三份再把 `.bak` 顶掉——真的会丢数据）。
fn answer_sheet_file_name(student_id: u64, session_id: u32, paper_fingerprint: &str) -> String {
    let short = paper_fingerprint.get(..8).unwrap_or(paper_fingerprint);
    format!("answers-{student_id}-{session_id}-{short}.eea")
}

/// 目录可写探测：写一个探针文件再删掉。
fn is_writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(".ees-write-probe");
    match std::fs::write(&probe, b"x") {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

impl ExamApp {
    /// 防抖保存：改动后 400ms 落盘；已交卷的卡立即落盘。
    fn maybe_save_sheet(&mut self) {
        if !self.sheet_dirty {
            return;
        }
        let submitted = self.sheet.as_ref().is_some_and(|s| s.submitted);
        if !submitted && self.last_edit.elapsed() < Duration::from_millis(400) {
            return;
        }
        self.save_sheet_now();
    }

    /// 立即把答题卡加密写盘。
    fn save_sheet_now(&mut self) {
        let (k_manifest, path) = match (&self.stage, &self.sheet_path) {
            (Stage::Paper(u), Some(p)) => (u.k_manifest, p.clone()),
            _ => return,
        };
        let Some(sheet) = self.sheet.as_ref() else {
            return;
        };
        let sealed = match common::answer_sheet::seal(&k_manifest, sheet) {
            Ok(b) => b,
            Err(e) => {
                self.save_error = Some(format!("答题卡加密失败：{e}"));
                return;
            }
        };
        match common::answer_sheet::write_atomic(&path, &sealed) {
            Ok(()) => {
                self.sheet_dirty = false;
                self.save_error = None;
            }
            Err(e) => {
                // 保存失败必须让学生/监考看见：静默失败等于答题卡丢了
                self.save_error = Some(format!("保存答题卡失败：{e}\n路径：{}", path.display()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_sheet_name_includes_paper_fingerprint() {
        let a = answer_sheet_file_name(2, 1, "91d7055714f35f870b8790ff3a19a20f");
        let b = answer_sheet_file_name(2, 1, "ffffffffffffffffffffffffffffffff");
        assert_eq!(a, "answers-2-1-91d70557.eea");
        assert_ne!(a, b, "不同试卷必须落到不同文件，否则会互相覆盖");
    }

    #[test]
    fn answer_sheet_name_tolerates_short_fingerprint() {
        let name = answer_sheet_file_name(2, 1, "abc");
        assert_eq!(name, "answers-2-1-abc.eea");
    }
}
