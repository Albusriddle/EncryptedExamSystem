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
        install_cjk_font(&cc.egui_ctx);
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
                ui.heading("保密考试系统");
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
            egui::CentralPanel::default().show(ctx, |ui| {
                scroll_offset = render_paper(ui, unlocked, &mut self.nav);
            });

            // 诊断：EES_DEBUG_NAV=<文件> 时输出题号卡与题目矩形，
            // 便于验证"跳转目标是否被正确记录"（截屏在 DPI 下不可靠）。
            if let Some(path) = diag_path("EES_DEBUG_NAV") {
                self.nav.dump_debug(&sections, scroll_offset, &path);
            }
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

/// 渲染试卷。返回滚动区域的当前偏移，便于诊断验证"跳转是否真的滚动了"。
fn render_paper(
    ui: &mut egui::Ui,
    unlocked: &Unlocked,
    nav: &mut crate::nav::NavState,
) -> egui::Vec2 {
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
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "网络时间 {}（{} 个源，分歧 {} 秒）",
                    common::timegate::format_unix_local_zoned(net.unix),
                    net.sources,
                    net.spread
                ))
                .size(12.0)
                .color(egui::Color32::GRAY),
            );
        }

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!("试卷指纹 {}", unlocked.paper.fingerprint()))
                .size(11.0)
                .color(egui::Color32::GRAY),
        );

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
                                for (j, opt) in item.options.iter().enumerate() {
                                    ui.label(format!("{}. {opt}", workflow::option_label(j)));
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
    out.state.offset
}

/// 安装中文字体。
///
/// egui 自带字体只覆盖拉丁字母，中文会显示成豆腐块。
/// 这里按优先级尝试 Windows 自带的中文字体；找不到时**不 panic**，
/// 只是界面中文会是方块——总比崩掉好。
fn install_cjk_font(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\msyh.ttc", // 微软雅黑
        r"C:\Windows\Fonts\msyhbd.ttc",
        r"C:\Windows\Fonts\simhei.ttf", // 黑体
        r"C:\Windows\Fonts\simsun.ttc", // 宋体
        "/System/Library/Fonts/PingFang.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ];

    let mut fonts = egui::FontDefinitions::default();
    for path in CANDIDATES {
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
        return;
    }
    // 找不到中文字体：记录但不中断。
    eprintln!("⚠ 未找到系统中文字体，界面中文可能显示为方块");
}

/// 启动 GUI。
pub fn run(opts: UnlockOpts) -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 720.0])
            .with_min_inner_size([600.0, 480.0])
            .with_title("保密考试系统"),
        ..Default::default()
    };
    eframe::run_native(
        "保密考试系统",
        native_options,
        Box::new(move |cc| Ok(Box::new(ExamApp::new(cc, opts)))),
    )
}
