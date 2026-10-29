//! 学生端图形界面（egui/eframe）。
//!
//! 设计原则：**GUI 只是壳**。所有业务逻辑在 [`crate::workflow::unlock_with_pin`]，
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
        pin: String,
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

/// 当前学生端包的指纹前 8 位；读不到时返回 unknown。
fn package_fingerprint(opts: &UnlockOpts) -> String {
    let path = opts
        .package
        .as_ref()
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_exe().ok());
    let Some(path) = path else {
        return "unknown".into();
    };
    match std::fs::read(path) {
        Ok(bytes) => blake3::hash(&bytes).to_hex()[..8].to_string(),
        Err(_) => "unknown".into(),
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
    /// 当前学生端包指纹前 8 位，登录页显示，便于核对 PIN 表/exe 是否同次打包。
    package_id: String,
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
    /// 教师授权后进入“交卷后只读回看”模式。
    review_mode: bool,

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
        let package_id = package_fingerprint(&opts);
        Self {
            stage: Stage::Idle {
                input: String::new(),
                pin: String::new(),
                error: None,
            },
            opts,
            key_hint,
            watermark: common::watermark(),
            welcome: Welcome::new(),
            package_id,
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
            review_mode: false,
            last_state_dump: None,
            #[cfg(debug_assertions)]
            last_locked_dump: None,
            clock: None,
            expired: false,
            focus_pending: true,
        }
    }

    /// 启动后台解锁。
    fn start_unlock(&mut self, student_id: u64, pin: String) {
        let (tx, rx) = mpsc::channel();
        let opts = self.opts.clone();
        std::thread::spawn(move || {
            // 结果转成 String 错误：anyhow::Error 不是 Send 的友好类型，
            // 跨线程传递用展示文本足够。
            let pin_arg = if pin.is_empty() {
                None
            } else {
                Some(pin.as_str())
            };
            let result =
                workflow::unlock_with_pin(student_id, pin_arg, &opts).map_err(|e| format!("{e:#}"));
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
                // 若损坏卡备份失败，后续保存绝不能覆盖原文件。
                let mut save_path = path.clone();
                // v0.3.3 及更早的文件名没有指纹段：先迁移，再按新名字读
                let legacy = path.with_file_name(format!(
                    "answers-{}-{}.eea",
                    unlocked.student_id, unlocked.session_id
                ));
                migrate_legacy_sheet(&path, &legacy);

                // 已有答题卡且是同一份卷：恢复；否则新建
                let expected_qids: Vec<String> = unlocked
                    .paper
                    .sections
                    .iter()
                    .flat_map(|sec| sec.items.iter().map(|it| it.id.clone()))
                    .collect();
                let restored = std::fs::read(&path)
                    .ok()
                    .and_then(|b| common::answer_sheet::open(&unlocked.k_manifest, &b).ok())
                    .filter(|s| {
                        s.paper_fingerprint == unlocked.paper.fingerprint()
                            && s.student_id == unlocked.student_id
                            && s.session_id == unlocked.session_id
                            && s.qids == expected_qids
                    });
                if restored.is_none() && path.exists() {
                    // 换卷/换包/损坏时旧卡不能默默丢掉：备份名唯一，绝不覆盖已有备份。
                    let mut bak = path.with_extension("eea.bak");
                    let mut n = 0u32;
                    while bak.exists() {
                        n += 1;
                        bak = path.with_extension(format!("eea.bak.{n}"));
                    }
                    if let Err(e) = std::fs::rename(&path, &bak) {
                        // 备份失败时不能再写回原路径，改用新文件保留原卡；
                        // recovered 也必须递增，不能覆盖上次留下的文件。
                        let mut recovered = path.with_extension("eea.recovered");
                        let mut r = 0u32;
                        while recovered.exists() {
                            r += 1;
                            recovered = path.with_extension(format!("eea.recovered.{r}"));
                        }
                        save_path = recovered;
                        self.save_error = Some(format!(
                            "原有答题卡无法读取，且备份失败：{e}\n原文件保持不动：{}",
                            path.display()
                        ));
                    } else {
                        self.save_error =
                            Some(format!("原有答题卡无法读取，已备份到：{}", bak.display()));
                    }
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
                // 诊断用：EES_SUBMIT=1 直接冻结，验证"交卷后页面"（release 不含）
                #[cfg(debug_assertions)]
                if std::env::var("EES_SUBMIT").is_ok()
                    && let Some(sh) = self.sheet.as_mut()
                {
                    sh.freeze(now_or_wall_clock(Some(now)));
                }
                self.sheet_dirty = true;
                self.last_edit = std::time::Instant::now()
                    .checked_sub(Duration::from_secs(5))
                    .unwrap_or_else(std::time::Instant::now);
                self.sheet_path = Some(save_path);
                // 不要无条件清空：备份失败/损坏提示必须保留到学生看到。
                self.confirm_submit = false;
                self.review_mode = false;
                self.stage = Stage::Paper(Box::new(unlocked));
                true
            }
            Ok(Err(msg)) => {
                self.stage = Stage::Idle {
                    input: String::new(),
                    pin: String::new(),
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
                    pin: String::new(),
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
            self.start_unlock(sid, String::new());
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
        let mut submit: Option<(String, String)> = None;
        // 焦点标志要在闭包内外传递，先取到局部变量：
        // 闭包里已经 `&mut self.stage`，不能再借 `self.focus_pending`。
        let mut want_focus = self.focus_pending;

        egui::CentralPanel::default().show(ctx, |ui| match &mut self.stage {
            Stage::Idle { input, pin, error } => {
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
                    ui.label(
                        egui::RichText::new(format!("考试包 {}", self.package_id))
                            .size(10.0)
                            .color(egui::Color32::GRAY),
                    );
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

                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("登录 PIN（没有则留空）").size(14.0));
                    ui.add(
                        egui::TextEdit::singleline(pin)
                            .hint_text("6 位 PIN")
                            .password(true)
                            .desired_width(240.0),
                    );

                    ui.add_space(16.0);

                    let clicked = ui
                        .add_sized([240.0, 40.0], egui::Button::new("开始考试"))
                        .clicked();
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                    if clicked || enter {
                        submit = Some((input.clone(), pin.clone()));
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
                let mut no_review = false;
                egui::CentralPanel::default().show(ctx, |ui| {
                    panel_rect = ui.max_rect();
                    card_rect = render_locked(
                        ui,
                        unlocked,
                        now,
                        LockedMode::Expired,
                        false,
                        &mut no_review,
                    );
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

            // 提前交卷：直接进"已交卷"页，不要让学生继续盯着试卷
            #[allow(clippy::collapsible_if)]
            if let Some(at) = self
                .sheet
                .as_ref()
                .filter(|s| s.submitted)
                .and_then(|s| s.submitted_unix)
            {
                if !self.review_mode {
                    let review_available =
                        unlocked.student_review == common::model::StudentReviewPolicy::Free;
                    let mut review_requested = false;
                    egui::CentralPanel::default().show(ctx, |ui| {
                        render_locked(
                            ui,
                            unlocked,
                            now,
                            LockedMode::Submitted { at },
                            review_available,
                            &mut review_requested,
                        );
                    });
                    if review_requested {
                        self.review_mode = true;
                    }
                    self.maybe_save_sheet();
                    return;
                }
            }

            let sections: Vec<(String, Vec<(String, usize)>)> = unlocked
                .paper
                .sections
                .iter()
                .map(|s| {
                    // 学生端只按大题分块：部分仅用于抽题，不在 UI 展示
                    let parts = vec![(String::new(), s.items.len())];
                    (s.title.clone(), parts)
                })
                .collect();

            // 诊断：模拟点击某个题号（验证跳转链路）
            if let Some(idx) = self.jump_once.take() {
                self.nav.debug_jump(idx);
            }

            // ① 题号卡（点击只写 pending，不立即滚动）
            //    已答/未答按答题卡算；索引 0 号位空着，题号从 1 开始。
            let mut flags = vec![false];
            if let Some(sheet) = &self.sheet {
                flags.extend(sheet.answers.iter().map(|a| a.is_some()));
            }
            self.nav.set_answered(flags);
            let nav_submitted = self.sheet.as_ref().is_some_and(|s| s.submitted);
            let nav_can_submit = self.sheet.as_ref().is_some_and(|s| !s.submitted) && !self.expired;
            self.nav.show(
                ctx,
                &sections,
                nav_can_submit,
                nav_submitted,
                self.confirm_submit,
            );
            #[allow(clippy::collapsible_if)]
            if self.nav.take_submit_requested() {
                if let Some(sheet) = self.sheet.as_mut() {
                    if !sheet.submitted && !self.expired {
                        if self.confirm_submit {
                            sheet.freeze(now_or_wall_clock(now));
                            self.confirm_submit = false;
                            self.sheet_dirty = true;
                            self.last_edit = std::time::Instant::now();
                        } else {
                            self.confirm_submit = true;
                        }
                    }
                }
            }

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
                        self.review_mode,
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
        if let Some((raw, pin)) = submit {
            let trimmed = raw.trim();
            match trimmed.parse::<u64>() {
                Ok(0) => {
                    self.stage = Stage::Idle {
                        input: raw,
                        pin,
                        error: Some("学号必须是正整数".into()),
                    };
                    self.focus_pending = true;
                }
                Ok(sid) => self.start_unlock(sid, pin),
                Err(_) => {
                    self.stage = Stage::Idle {
                        input: raw,
                        pin,
                        error: Some("学号必须是纯数字".into()),
                    };
                    self.focus_pending = true;
                }
            }
        }
    }
}

/// 渲染试卷。
/// 试卷底部留白下限（pt）；实际使用当前窗口高度，避免高窗口末题不能贴顶。
const BOTTOM_PAD_MIN: f32 = 320.0;

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
#[derive(Clone, Copy)]
enum LockedMode {
    /// 到点收卷
    Expired,
    /// 学生主动交卷（提前）
    Submitted { at: u64 },
}

fn render_locked(
    ui: &mut egui::Ui,
    unlocked: &Unlocked,
    now: Option<u64>,
    mode: LockedMode,
    review_available: bool,
    review_requested: &mut bool,
) -> egui::Rect {
    /// 卡片内容宽度的上限（不含内边距）
    const CARD_MAX_W: f32 = 380.0;

    let mut card = egui::Rect::NOTHING;

    // 窗口被拉得很矮时（或卡片比剩余空间高），用滚动兜底：
    // 宁可滚动，也不能把卡片裁掉一半——裁掉等于信息丢失。
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(36.0);
        ui.vertical_centered(|ui| {
            let (title, tip) = match mode {
                LockedMode::Expired => ("本场考试已结束", "时间到，试卷已关闭。"),
                LockedMode::Submitted { .. } => (
                    "已交卷",
                    "答题卡已提交；如顶部提示保存失败，请立即联系监考。",
                ),
            };
            ui.heading(title);
            ui.add_space(6.0);
            ui.label(egui::RichText::new(tip).size(13.0));
            ui.add_space(14.0);
            ui.label(format!("场次 #{}", unlocked.session_id));
            ui.label(format!("结束时间：{}", unlocked.end));
            if let LockedMode::Submitted { at } = mode {
                ui.label(format!(
                    "交卷时间：{}",
                    common::timegate::format_unix_local_zoned(at)
                ));
                if review_available {
                    ui.add_space(8.0);
                    if ui.button("回看试卷").clicked() {
                        *review_requested = true;
                    }
                }
            }
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

/// 选项切换的纯函数：单选覆盖、多选切换并排序。
fn toggle_key(current: &str, key: &str, multiple: bool) -> Option<String> {
    if !multiple {
        return Some(key.to_string());
    }
    let mut chars: Vec<char> = current.chars().collect();
    if let Some(pos) = chars.iter().position(|c| c.to_string() == key) {
        chars.remove(pos);
    } else if let Some(c) = key.chars().next() {
        chars.push(c);
    }
    chars.sort();
    let joined: String = chars.into_iter().collect();
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
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
            let multiple = item.qtype == "multiple";
            let mut current = sheet.answers[idx].clone().unwrap_or_default();
            for opt in options {
                let on = if multiple {
                    current.contains(&opt.key)
                } else {
                    current == opt.key
                };

                // 题支可能是多行、带 ``` 代码块：按块渲染，整块可点。
                let blocks = crate::markdown::parse_cached(&opt.text);
                let accent = if on {
                    Some(crate::nav::NavState::option_fill(ui.visuals()).1)
                } else {
                    None
                };
                let bg_idx = ui.painter().add(egui::Shape::Noop);
                let inner = ui
                    .scope(|ui| {
                        ui.horizontal_top(|ui| {
                            let key_text = egui::RichText::new(format!("{}.", opt.key)).strong();
                            let key_text = match accent {
                                Some(c) => key_text.color(c),
                                None => key_text,
                            };
                            ui.label(key_text);
                            ui.vertical(|ui| {
                                render_blocks(ui, blocks.as_ref(), true);
                            });
                        });
                    })
                    .response;

                let rect = inner.rect.expand2(egui::vec2(2.0, 2.0));
                let shape = if on {
                    let (fill, bar) = crate::nav::NavState::option_fill(ui.visuals());
                    let bar_rect =
                        egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height()));
                    egui::Shape::Vec(vec![
                        egui::Shape::rect_filled(rect, 4.0, fill),
                        egui::Shape::rect_filled(bar_rect, 2.0, bar),
                    ])
                } else if inner.hovered() {
                    egui::Shape::rect_filled(rect, 4.0, ui.visuals().widgets.hovered.bg_fill)
                } else {
                    egui::Shape::rect_filled(rect, 4.0, egui::Color32::TRANSPARENT)
                };
                ui.painter().set(bg_idx, shape);

                let sense = if editable {
                    egui::Sense::click()
                } else {
                    egui::Sense::hover()
                };
                let resp = ui.interact(
                    rect,
                    ui.make_persistent_id(("option", idx, opt.key.as_str())),
                    sense,
                );
                if resp.clicked() && editable {
                    let value = toggle_key(&current, &opt.key, multiple);
                    if sheet.set_answer(idx, value.clone(), now_unix) {
                        changed = true;
                        current = value.unwrap_or_default();
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
        QuestionBody::Text => {
            // 问答题：多行文本作答（纯展示题不给输入框）。
            if item.qtype == "essay" {
                let mut text = sheet.answers[idx].clone().unwrap_or_default();
                let resp = ui.add_enabled(
                    editable,
                    egui::TextEdit::multiline(&mut text)
                        .desired_rows(6)
                        .desired_width(ui.available_width())
                        .hint_text("请在此作答；本题由老师人工评阅"),
                );
                if resp.changed() && sheet.set_answer(idx, essay_value(&text), now_unix) {
                    changed = true;
                }
                ui.label(
                    egui::RichText::new(format!("{} 字", text.chars().count()))
                        .size(12.0)
                        .color(egui::Color32::GRAY),
                );
            }
        }
    }
    changed
}

/// 问答题答案上限：防止单题把答题卡撑爆。
const ESSAY_MAX_CHARS: usize = 5000;

/// 把多行文本规整成答题卡值：空白视为未作答，超长按字符截断。
fn essay_value(text: &str) -> Option<String> {
    let capped: String = text.chars().take(ESSAY_MAX_CHARS).collect();
    if capped.trim().is_empty() {
        None
    } else {
        Some(capped)
    }
}

/// 空位序号：0→①，1→② …（超过 10 个空时退化为 ①+数字）
fn blank_marker(k: usize) -> String {
    const MARKS: [&str; 10] = ["①", "②", "③", "④", "⑤", "⑥", "⑦", "⑧", "⑨", "⑩"];
    MARKS
        .get(k)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("①+{k}"))
}

/// 渲染题干：把 ``` 围栏代码块单独画成等宽块，其余按文本排版。
/// 取一个**有限**的可用宽度。
///
/// egui 在 `ui.horizontal` 里 `available_width()` 可能返回 `f32::INFINITY`，
/// 直接拿去设 `LayoutJob.wrap.max_width` 或 `ScrollArea::max_width` 会让
/// 布局无界展开（实测内存一路涨到十几 GB 后卡死）。
fn avail_w(ui: &egui::Ui) -> f32 {
    let w = ui.available_width();
    if w.is_finite() {
        w.clamp(80.0, 4096.0)
    } else {
        800.0
    }
}

fn render_stem(ui: &mut egui::Ui, stem: &str) {
    let blocks = crate::markdown::parse_cached(stem);
    render_blocks(ui, blocks.as_ref(), false);
}

/// 渲染一组已解析的块（题干与选项共用）。
///
/// `compact` = 选项里的渲染：代码块**不套横向滚动区**（滚动区会吃掉整块点击），
/// 长行自动折行。
fn render_blocks(ui: &mut egui::Ui, blocks: &[crate::markdown::Block], compact: bool) {
    use crate::markdown::Block;

    for block in blocks.iter() {
        match block {
            Block::Paragraph(spans) => {
                let w = avail_w(ui);
                render_spans(ui, spans, 15.0, w);
            }
            Block::Heading { level, spans } => {
                let size = match *level {
                    1 => 18.0,
                    2 => 16.5,
                    _ => 15.5,
                };
                ui.add_space(2.0);
                let w = avail_w(ui);
                render_spans(ui, spans, size, w);
            }
            Block::List {
                ordered,
                start,
                items,
            } => {
                ui.indent("md_list", |ui| {
                    for (i, item) in items.iter().enumerate() {
                        let bullet = if *ordered {
                            format!("{}. ", *start + i as u64)
                        } else {
                            "• ".to_owned()
                        };
                        let text: String = item
                            .iter()
                            .map(|sp| match sp {
                                crate::markdown::Span::Text(t)
                                | crate::markdown::Span::Code(t)
                                | crate::markdown::Span::Bold(t) => t.as_str(),
                            })
                            .collect();
                        ui.label(egui::RichText::new(format!("{bullet}{text}")).size(15.0));
                    }
                });
            }
            Block::Quote(spans) => {
                egui::Frame::none()
                    .fill(ui.visuals().faint_bg_color)
                    .inner_margin(egui::Margin {
                        left: 10.0,
                        right: 8.0,
                        top: 6.0,
                        bottom: 6.0,
                    })
                    .show(ui, |ui| {
                        let w = avail_w(ui);
                        render_spans(ui, spans, 14.5, w);
                    });
            }
            Block::Code { lines, .. } => {
                if compact {
                    render_mono_block_wrapped(ui, lines);
                } else {
                    render_mono_block(ui, lines, false);
                }
            }
            Block::Table {
                header,
                rows,
                aligns,
            } => render_table(ui, header, rows, aligns),
            Block::Image { alt, src } => render_image(ui, alt, src),
        }
    }
}

/// 行内混排：普通文本 + 行内代码 + 加粗。
///
/// `wrap` 是换行宽度：段落用可用宽度，表格单元格固定 360pt
/// （否则一个 500 字的单元格会把表格撑到没法看）。
fn render_spans(ui: &mut egui::Ui, spans: &[crate::markdown::Span], size: f32, wrap: f32) {
    render_spans_ex(ui, spans, size, wrap, false);
}

fn render_spans_ex(
    ui: &mut egui::Ui,
    spans: &[crate::markdown::Span],
    size: f32,
    wrap: f32,
    bold_text: bool,
) {
    let job = build_job(ui, spans, size, wrap, bold_text);
    ui.label(job);
}

/// 把行内片段拼成 LayoutJob（段落与选项按钮共用）。
fn build_job(
    ui: &egui::Ui,
    spans: &[crate::markdown::Span],
    size: f32,
    wrap: f32,
    bold_text: bool,
) -> egui::text::LayoutJob {
    use crate::markdown::Span;

    let visuals = ui.visuals().clone();
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = wrap.max(60.0);
    job.halign = egui::Align::LEFT;

    for span in spans {
        match span {
            Span::Text(text) => {
                let (font_id, color) = if bold_text {
                    bold_style(ui, size)
                } else {
                    (egui::FontId::proportional(size), visuals.text_color())
                };
                job.append(
                    text,
                    0.0,
                    egui::TextFormat {
                        font_id,
                        color,
                        ..Default::default()
                    },
                );
            }
            Span::Code(code) => job.append(
                code,
                0.0,
                egui::TextFormat {
                    font_id: egui::FontId::monospace(size * 0.95),
                    color: visuals.text_color(),
                    background: visuals.extreme_bg_color,
                    ..Default::default()
                },
            ),
            Span::Bold(text) => {
                let (font_id, color) = bold_style(ui, size);
                job.append(
                    text,
                    0.0,
                    egui::TextFormat {
                        font_id,
                        color,
                        ..Default::default()
                    },
                );
            }
        }
    }

    job
}

/// 加粗：优先用系统粗体字族，找不到就用强调色（缺字形时不能变方块）。
fn bold_style(ui: &egui::Ui, size: f32) -> (egui::FontId, egui::Color32) {
    let visuals = ui.visuals();
    let bold_ready = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(egui::Id::new("ees_bold_family")))
        .unwrap_or(false);
    if bold_ready {
        (
            egui::FontId::new(size, egui::FontFamily::Name("bold".into())),
            visuals.text_color(),
        )
    } else {
        (
            egui::FontId::proportional(size),
            visuals.strong_text_color(),
        )
    }
}

/// 表格：横向滑块（可拖拽）+ 长表格限高纵向滚动。
///
/// ## 为什么不用 `egui::Grid`
///
/// Grid 放进横向 ScrollArea 后，列宽是按"可用宽度"推算的；一旦可用宽度
/// 异常（横向布局里可能是 INFINITY），布局会无界展开——实测内存能涨到
/// 十几 GB 后卡死。这里改成**先量文字宽度、再自己排**：
/// 内容总宽是显式算出来的，ScrollArea 只是视口，不存在无界展开。
fn render_table(
    ui: &mut egui::Ui,
    header: &[Vec<crate::markdown::Span>],
    rows: &[Vec<Vec<crate::markdown::Span>>],
    aligns: &[crate::markdown::Align],
) {
    const PAD: f32 = 16.0;
    const MAX_COL: f32 = 360.0;

    let ncols = header.len().max(1);
    let mut widths = vec![60.0f32; ncols];
    let measure = measure_spans;

    for (i, cell) in header.iter().enumerate() {
        widths[i] = widths[i].max((measure(ui, cell) + PAD).min(MAX_COL));
    }
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(ncols) {
            widths[i] = widths[i].max((measure(ui, cell) + PAD).min(MAX_COL));
        }
    }
    let total: f32 = widths.iter().sum();

    egui::Frame::none()
        .fill(ui.visuals().faint_bg_color)
        .inner_margin(egui::Margin::same(6.0))
        .show(ui, |ui| {
            let avail = avail_w(ui);
            ui.set_max_width(avail);
            let tall = rows.len() > 30;
            let mut area = if tall {
                egui::ScrollArea::both().max_height(360.0).max_width(avail)
            } else {
                egui::ScrollArea::horizontal().max_width(avail)
            };
            area = area.drag_to_scroll(true).auto_shrink([false, true]);
            area.show(ui, |ui| {
                // 显式给出内容宽度：这是避免无界展开的关键
                ui.set_width(total.max(avail - 16.0));
                table_row(ui, header, &widths, aligns, true);
                ui.add_space(2.0);
                for row in rows {
                    table_row(ui, row, &widths, aligns, false);
                }
            });
        });
}

/// 量一段富文本的宽度：按每个 span 自己的字体累加。
///
/// 行内代码是等宽字体，比比例字体宽；统一按比例字体量会让代码列偏窄、代码被折行。
fn measure_spans(ui: &egui::Ui, spans: &[crate::markdown::Span]) -> f32 {
    use std::cell::RefCell;
    use std::collections::HashMap;

    // 每帧对每个单元格重新排版太贵（40×8 的表就是 320 次/帧）。
    // 按 (文字, 是否等宽) 缓存；上限定 1024 条，避免题库特别大时无限增长。
    thread_local! {
        static WIDTHS: RefCell<HashMap<(String, bool), f32>> = RefCell::new(HashMap::new());
    }
    const LIMIT: usize = 1024;

    let mut width = 0.0;
    for span in spans {
        let (text, is_code) = match span {
            crate::markdown::Span::Text(t) => (t.as_str(), false),
            crate::markdown::Span::Bold(t) => (t.as_str(), false),
            crate::markdown::Span::Code(t) => (t.as_str(), true),
        };
        let key = (text.to_owned(), is_code);
        let cached = WIDTHS.with(|c| c.borrow().get(&key).copied());
        let w = match cached {
            Some(w) => w,
            None => {
                let font = if is_code {
                    egui::FontId::monospace(13.3)
                } else {
                    egui::FontId::proportional(14.0)
                };
                let w = ui.fonts(|f| {
                    f.layout_no_wrap(text.to_owned(), font, egui::Color32::WHITE)
                        .size()
                        .x
                });
                WIDTHS.with(|c| {
                    let mut c = c.borrow_mut();
                    if c.len() < LIMIT {
                        c.insert(key, w);
                    }
                });
                w
            }
        };
        width += w;
    }
    width
}

fn table_row(
    ui: &mut egui::Ui,
    cells: &[Vec<crate::markdown::Span>],
    widths: &[f32],
    aligns: &[crate::markdown::Align],
    header: bool,
) {
    use crate::markdown::Align;

    ui.horizontal(|ui| {
        for (i, cell) in cells.iter().enumerate() {
            let Some(w) = widths.get(i) else { break };
            let align = aligns.get(i).copied().unwrap_or(Align::Left);
            let layout = match align {
                Align::Left => egui::Layout::top_down(egui::Align::Min),
                Align::Center => egui::Layout::top_down(egui::Align::Center),
                Align::Right => egui::Layout::top_down(egui::Align::Max),
            };
            ui.allocate_ui_with_layout(egui::vec2(*w, 0.0), layout, |ui| {
                render_spans_ex(ui, cell, 14.0, *w - 4.0, header);
            });
        }
    });
}

/// 图片：只接受 data URI；解码失败显示占位文字，不影响其它题目。
fn render_image(ui: &mut egui::Ui, alt: &str, src: &str) {
    match crate::images::texture_for_uri(ui.ctx(), src) {
        Some(tex) => {
            let max_w = avail_w(ui).min(720.0);
            let size = fit_image(tex.size_vec2(), max_w, 480.0);
            ui.add(egui::Image::new((tex.id(), size)));
        }
        None => {
            ui.label(
                egui::RichText::new(format!("[图片无法显示：{alt}]"))
                    .color(egui::Color32::from_rgb(180, 120, 60)),
            );
        }
    }
}

/// 等比缩放到最大宽/高之内。
fn fit_image(size: egui::Vec2, max_w: f32, max_h: f32) -> egui::Vec2 {
    if size.x <= 0.0 || size.y <= 0.0 {
        return egui::vec2(1.0, 1.0);
    }
    let k = (max_w / size.x).min(max_h / size.y).min(1.0);
    egui::vec2(size.x * k, size.y * k)
}

/// 等宽代码块（折行版）：选项里用，避免 ScrollArea 抢走整块点击。
fn render_mono_block_wrapped(ui: &mut egui::Ui, lines: &[String]) {
    egui::Frame::none()
        .fill(ui.visuals().extreme_bg_color)
        .inner_margin(egui::Margin::same(8.0))
        .show(ui, |ui| {
            for line in lines {
                ui.label(egui::RichText::new(line).monospace().size(13.0));
            }
        });
}

/// 等宽代码块：底色 + 横向滚动（长行不能被裁掉）。
fn render_mono_block(ui: &mut egui::Ui, lines: &[String], numbered: bool) {
    egui::Frame::none()
        .fill(ui.visuals().extreme_bg_color)
        .inner_margin(egui::Margin::same(8.0))
        .show(ui, |ui| {
            egui::ScrollArea::horizontal().show(ui, |ui| {
                for (i, line) in lines.iter().enumerate() {
                    let text = if numbered {
                        format!("{:>3}  {}", i + 1, line)
                    } else {
                        line.clone()
                    };
                    ui.label(egui::RichText::new(text).monospace().size(13.0));
                }
            });
        });
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
        // 选项由 render_answer_inputs 统一绘制（可点击/禁用两种状态）。
        // 这里再画一遍会让每道选择题的选项出现两次——v0.3.5 实测的观感问题。
        QuestionBody::Options { .. } => {}
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
            render_mono_block(ui, code, true);
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
    review_only: bool,
    confirm_submit: &mut bool,
    now_unix: u64,
) -> (egui::Vec2, bool) {
    let mut changed = false;
    let mut qidx = 0usize;
    let viewport_top = ui.clip_rect().top();
    let out = egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(8.0);
        if review_only {
            ui.label(
                egui::RichText::new("📖 只读回看 · 不能修改答案")
                    .size(13.0)
                    .color(egui::Color32::from_rgb(120, 150, 180)),
            );
            ui.add_space(4.0);
        }
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
                    egui::RichText::new("✔ 已交卷")
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
                let resp = ui
                    .push_id(format!("item{seq}"), |ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(format!("{seq}. ({}分)", item.score))
                                    .size(15.0),
                            );
                            render_stem(ui, &item.stem);
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
        // 用当前窗口高度而不是固定 900pt：窗口更高时末题也能贴顶。
        // ScrollArea 内部的 available_height() 在内容撑开后接近 0，不能用。
        ui.add_space(ui.ctx().screen_rect().height().max(BOTTOM_PAD_MIN));

        // 全部题目渲染完毕，此时 rects 是本帧最新的，消费跳转请求。
        nav.apply_pending_scroll(ui);
    });
    nav.update_current(viewport_top);
    (out.state.offset, changed)
}

/// 中文字体候选（按优先级）。
/// 粗体字体候选：Markdown 的 **加粗** 用；全缺失时退化为强调色。
const BOLD_CANDIDATES: &[&str] = &[
    "C:/Windows/Fonts/msyhbd.ttc",
    "C:/Windows/Fonts/simhei.ttf",
    "C:/Windows/Fonts/arialbd.ttf",
    "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
];

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
    let mut ok = false;
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
        ok = true;
        break;
    }

    // 粗体字族：Markdown 的 **加粗** 用它；找不到就退化为强调色（见 bold_style）。
    let mut bold_ok = false;
    for path in BOLD_CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        fonts
            .font_data
            .insert("bold".to_owned(), egui::FontData::from_owned(bytes));
        let mut chain = vec!["bold".to_owned()];
        if fonts.font_data.contains_key("cjk") {
            chain.push("cjk".to_owned());
        }
        if let Some(defaults) = fonts.families.get(&egui::FontFamily::Proportional) {
            chain.extend(defaults.iter().cloned());
        }
        fonts
            .families
            .insert(egui::FontFamily::Name("bold".into()), chain);
        bold_ok = true;
        break;
    }
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("ees_bold_family"), bold_ok));

    if ok {
        ctx.set_fonts(fonts);
    } else {
        eprintln!("⚠ 未找到系统中文字体，界面中文可能显示为方块");
    }
    ok
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

/// 把旧命名（无指纹段）的答题卡迁移到新名字。
///
/// 为什么不能"不迁移直接读旧文件"：旧文件会永远躺在磁盘上，而新版本按新名字
/// 找，学生升级客户端后作答会被当成"没有答题卡"。
fn migrate_legacy_sheet(new_path: &std::path::Path, legacy_path: &std::path::Path) {
    if !new_path.exists() && legacy_path.exists() {
        let _ = std::fs::rename(legacy_path, new_path);
    }
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

    #[test]
    fn essay_value_trims_blank_and_caps_length() {
        assert_eq!(essay_value("   \n\t "), None);
        assert_eq!(essay_value("我的回答"), Some("我的回答".to_string()));

        let long = "字".repeat(ESSAY_MAX_CHARS + 10);
        let v = essay_value(&long).expect("非空");
        assert_eq!(v.chars().count(), ESSAY_MAX_CHARS);
    }

    #[test]
    fn essay_stores_multiline_text_in_sheet() {
        let mut sheet = common::answer_sheet::AnswerSheet {
            version: 1,
            student_id: 1,
            session_id: 1,
            paper_fingerprint: "0".repeat(32),
            qids: vec!["e1".into()],
            answers: vec![None],
            updated_unix: 0,
            submitted: false,
            submitted_unix: None,
        };
        assert!(sheet.set_answer(0, essay_value("第一行\n第二行"), 100));
        assert_eq!(sheet.answers[0].as_deref(), Some("第一行\n第二行"));
        assert!(
            !sheet.set_answer(0, essay_value("第一行\n第二行"), 101),
            "同值不应重复写"
        );
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn legacy_sheet_is_renamed() {
        let dir = std::path::Path::new("target").join("sheet-migrate");
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("answers-2-1.eea");
        let new = dir.join("answers-2-1-91d70557.eea");
        let _ = std::fs::remove_file(&new);
        std::fs::write(&legacy, b"old").unwrap();

        migrate_legacy_sheet(&new, &legacy);
        assert!(new.exists(), "旧文件应被改名为新文件");
        assert!(!legacy.exists(), "旧文件不应残留");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn existing_new_sheet_is_not_overwritten() {
        let dir = std::path::Path::new("target").join("sheet-migrate2");
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("answers-2-1.eea");
        let new = dir.join("answers-2-1-91d70557.eea");
        std::fs::write(&legacy, b"old").unwrap();
        std::fs::write(&new, b"new").unwrap();

        migrate_legacy_sheet(&new, &legacy);
        assert_eq!(
            std::fs::read(&new).unwrap(),
            b"new",
            "新文件不能被旧文件覆盖"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod markdown_render_tests {
    use super::render_stem;
    use eframe::egui;

    /// 无窗口跑一帧，验证渲染路径不 panic。
    fn run_frame(stem: &str) {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    render_stem(ui, stem);
                });
            });
        });
    }

    #[test]
    fn toggle_key_single_replaces() {
        assert_eq!(super::toggle_key("A", "C", false), Some("C".to_string()));
        assert_eq!(super::toggle_key("", "B", false), Some("B".to_string()));
    }

    #[test]
    fn toggle_key_multi_toggles_and_sorts() {
        assert_eq!(super::toggle_key("A", "C", true), Some("AC".to_string()));
        assert_eq!(super::toggle_key("AC", "A", true), Some("C".to_string()));
        assert_eq!(super::toggle_key("C", "A", true), Some("AC".to_string()));
        assert_eq!(super::toggle_key("A", "A", true), None);
    }

    #[test]
    fn option_with_fenced_code_block_renders() {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let blocks = crate::markdown::parse_cached(
                        "程序如下：\n```c\nint x = 1;\n```\n输出是？",
                    );
                    super::render_blocks(ui, blocks.as_ref(), true);
                });
            });
        });
    }

    #[test]
    fn option_job_renders_inline_code_in_monospace() {
        use crate::markdown::Span;

        let ctx = egui::Context::default();
        let mut sections = 0usize;
        let mut mono = false;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let spans = vec![
                    Span::Text("A. 程序的输出为 ".into()),
                    Span::Code("1 10".into()),
                    Span::Text("。".into()),
                ];
                let job = super::build_job(ui, &spans, 15.0, 400.0, false);
                sections = job.sections.len();
                mono = job
                    .sections
                    .iter()
                    .any(|s| matches!(s.format.font_id.family, egui::FontFamily::Monospace));
            });
        });
        assert!(
            sections >= 3,
            "题支里的行内代码必须切成独立 section：{sections}"
        );
        assert!(mono, "行内代码段必须用等宽字体");
    }

    #[test]
    fn code_span_is_measured_wider_than_text() {
        use crate::markdown::Span;

        let ctx = egui::Context::default();
        let mut code_w = 0.0;
        let mut text_w = 0.0;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                code_w = super::measure_spans(ui, &[Span::Code("iiiiiiii".into())]);
                text_w = super::measure_spans(ui, &[Span::Text("iiiiiiii".into())]);
            });
        });
        assert!(
            code_w > text_w,
            "等宽字体应更宽：code={code_w} text={text_w}"
        );
    }

    #[test]
    fn renders_text_blocks() {
        run_frame("普通段落，含 `行内代码` 与 **加粗**。");
        run_frame("# 标题\n\n### 三级");
        run_frame("- 甲\n- 乙\n\n1. 一\n2. 二");
        run_frame("> 引用一行");
    }

    #[test]
    fn renders_code_block() {
        run_frame("```c\nint x = 1;\n```");
    }

    #[test]
    fn renders_table() {
        run_frame("| 列1 | 列2 |\n|:--|--:|\n| a | b |");
    }

    #[test]
    fn renders_every_block_kind_in_one_frame() {
        let stem = "# 标题\n\n普通段落，含 `行内代码` 与 **加粗**。\n\n- 甲\n- 乙\n\n1. 一\n2. 二\n\n> 引用一行\n\n| 列1 | 列2 |\n|:--|--:|\n| a | b |\n\n```c\nint x = 1;\n```\n\n![图](data:image/png;base64,AAAA)";
        run_frame(stem);
    }

    #[test]
    fn renders_every_block_kind() {
        let stem = "# 标题\n\n普通段落，含 `行内代码` 与 **加粗**。\n\n- 甲\n- 乙\n\n1. 一\n2. 二\n\n> 引用一行\n\n| 列1 | 列2 |\n|:--|--:|\n| a | b |\n\n```c\nint x = 1;\n```\n\n![图](data:image/png;base64,AAAA)";
        run_frame(stem);
    }

    #[test]
    fn renders_long_table() {
        let mut stem = String::from("| 列1 | 列2 | 列3 |\n|---|---|---|\n");
        for i in 0..40 {
            stem.push_str(&format!("| 数据{i} | 更多{i} | 更长一点的数据{i} |\n"));
        }
        run_frame(&stem);
    }

    #[test]
    fn renders_broken_image() {
        run_frame("![图](data:image/png;base64,AAAA)");
    }

    #[test]
    fn renders_malformed_input_without_panic() {
        for stem in [
            "```未闭合",
            "| 只有表头 |\n| --- |",
            "**未闭合加粗",
            "`未闭合代码",
            "",
        ] {
            run_frame(stem);
        }
    }
}
