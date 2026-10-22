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

use crate::workflow::{self, UnlockOpts, Unlocked};

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
}

impl ExamApp {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: UnlockOpts) -> Self {
        // 中文字体：egui 默认字体不含 CJK，必须手动加载系统字体，
        // 否则界面上所有中文都是"□"。
        install_cjk_font(&cc.egui_ctx);
        Self {
            stage: Stage::Idle {
                input: String::new(),
                error: None,
            },
            opts,
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
                self.stage = Stage::Paper(Box::new(unlocked));
                true
            }
            Ok(Err(msg)) => {
                self.stage = Stage::Idle {
                    input: String::new(),
                    error: Some(msg),
                };
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.stage = Stage::Idle {
                    input: String::new(),
                    error: Some("后台线程异常退出（可能是内存不足）".into()),
                };
                true
            }
        }
    }
}

impl eframe::App for ExamApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 后台工作期间主动请求重绘，否则界面不刷新。
        if matches!(self.stage, Stage::Busy { .. }) {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        self.poll();

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("保密考试系统");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !crate::keys::is_embedded() {
                        ui.colored_label(
                            egui::Color32::from_rgb(200, 120, 0),
                            "开发模式（未嵌入密钥）",
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
                    // 自动聚焦，学生打开就能直接敲。
                    if !resp.has_focus() {
                        resp.request_focus();
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

            Stage::Paper(unlocked) => render_paper(ui, unlocked),
        });

        // 闭包结束后再切状态。
        if let Some(raw) = submit {
            let trimmed = raw.trim();
            match trimmed.parse::<u64>() {
                Ok(0) => {
                    self.stage = Stage::Idle {
                        input: raw,
                        error: Some("学号必须是正整数".into()),
                    };
                }
                Ok(sid) => self.start_unlock(sid),
                Err(_) => {
                    self.stage = Stage::Idle {
                        input: raw,
                        error: Some("学号必须是纯数字".into()),
                    };
                }
            }
        }
    }
}

/// 渲染试卷。
fn render_paper(ui: &mut egui::Ui, unlocked: &Unlocked) {
    egui::ScrollArea::vertical().show(ui, |ui| {
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
                    "网络时间 {} UTC（{} 个源，分歧 {} 秒）",
                    common::timegate::format_unix_utc(net.unix),
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
                ui.label(
                    egui::RichText::new(format!("{seq}. ({}) {}", item.score, item.stem))
                        .size(15.0),
                );
                ui.indent(format!("q{seq}"), |ui| {
                    for (j, opt) in item.options.iter().enumerate() {
                        ui.label(format!("{}. {opt}", workflow::option_label(j)));
                    }
                });
                ui.add_space(8.0);
            }
        }
        ui.add_space(20.0);
    });
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
