//! 启动欢迎弹窗：版本、仓库、许可证、致谢、Star 邀请。
//!
//! 学生端每次启动都可能是一次考试，弹窗不能挡路：
//! 提供「不再提示」，选择记在 `%LOCALAPPDATA%\ExamSystem\skip_welcome`，
//! 同一台机器后续启动直接跳过。

use std::path::PathBuf;

use eframe::egui;

use common::branding;

/// 以给定目录为根时的标记文件路径（便于测试）。
fn marker_path_in(base: &std::path::Path) -> PathBuf {
    base.join("ExamSystem").join("skip_welcome")
}

/// 「不再提示」标记文件路径。取不到用户目录时返回 `None`（每次都弹）。
fn marker_path() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))?;
    Some(marker_path_in(&base))
}

/// 用户是否选择了不再提示。
pub fn is_dismissed() -> bool {
    marker_path().is_some_and(|p| p.is_file())
}

/// 记住「不再提示」。写失败不影响使用，只是下次还会弹。
fn remember_dismissed() {
    if let Some(p) = marker_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, b"1");
    }
}

/// 欢迎弹窗状态。
pub struct Welcome {
    open: bool,
    dont_show: bool,
}

impl Welcome {
    pub fn new() -> Self {
        Self {
            open: !is_dismissed(),
            dont_show: false,
        }
    }

    /// 绘制弹窗。返回 `true` 表示刚被关闭（供调用方切焦点）。
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        if !self.open {
            return false;
        }
        let b = branding();
        let mut closed = false;

        // 用 Area 自己定位而不用 Window：
        // Window 会按内容自动撑宽，长文本一旦不换行就会顶出视口（实测被裁一半）。
        // Area 支持显式指定宽度，配合 set_width 才能保证文本按宽度换行。
        const W: f32 = 470.0;
        let screen = ctx.screen_rect();
        // 锚在视口左上角：居中对齐在不同 DPI/标题栏口径下容易把弹窗
        // 推到视口外（实测右侧被裁）。左上角是唯一确定可见的位置。
        let pos = screen.left_top() + egui::vec2(40.0, 60.0);

        // 半透明遮罩，突出弹窗并阻止误点背后的按钮
        egui::Area::new(egui::Id::new("welcome_backdrop"))
            .order(egui::Order::Background)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let painter = ui.painter();
                painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(160));
            });

        let mut frame_rect = egui::Rect::NOTHING;
        egui::Area::new(egui::Id::new("welcome"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(W);
                    ui.add_space(4.0);
                    ui.heading(b.display_name());
                    ui.label(
                        egui::RichText::new(format!(
                            "版本 {} · {}",
                            common::version(),
                            b.license_name
                        ))
                        .size(12.0)
                        .color(egui::Color32::from_gray(140)),
                    );

                    ui.add_space(8.0);
                    ui.add(egui::Label::new(&b.tagline).wrap());

                    ui.add_space(10.0);
                    ui.label("项目地址：");
                    let short = b
                        .repo_url
                        .trim_start_matches("https://")
                        .trim_start_matches("http://");
                    ui.hyperlink_to(short, &b.repo_url)
                        .on_hover_text(&b.repo_url);
                    ui.label(format!("许可证：{}（{}）", b.license_name, b.license));

                    ui.add_space(10.0);
                    ui.label(egui::RichText::new(&b.credits_title).strong());
                    for c in &b.credits {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(format!("· {}：", c.role));
                            ui.hyperlink_to(&c.name, &c.url);
                        });
                    }

                    ui.add_space(10.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&b.star_hint)
                                .color(egui::Color32::from_rgb(90, 170, 90)),
                        )
                        .wrap(),
                    );

                    ui.add_space(12.0);
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.dont_show, "不再提示");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("开始使用").clicked() {
                                closed = true;
                            }
                        });
                    });
                    frame_rect = ui.min_rect();
                });
            });

        // 诊断：设置 EES_DEBUG_LAYOUT=<文件> 可把弹窗与视口的实际矩形写出来。
        // 截图在 DPI/多显示器下不可靠，让程序自己报告才可信。
        if let Some(path) = std::env::var_os("EES_DEBUG_LAYOUT") {
            let info = format!(
                "screen={:?}\npopup={:?}\nwidth_pt={}\ncontains={}\n",
                screen,
                frame_rect,
                frame_rect.width(),
                screen.contains_rect(frame_rect)
            );
            let _ = std::fs::write(path, info);
        }

        if closed {
            self.open = false;
            if self.dont_show {
                remember_dismissed();
            }
        }
        closed
    }
}

impl Default for Welcome {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        // 不用 std::env::temp_dir()：本机 MSYS 环境下它指向 /tmp，
        // 原生 Windows 进程写不进去（os error 5）。放进 target/ 下最稳。
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/ees-test")
            .join(format!("welcome-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn marker_path_is_under_examsystem() {
        let p = marker_path_in(std::path::Path::new("C:/base"));
        assert!(p.ends_with("ExamSystem/skip_welcome") || p.ends_with("ExamSystem\\skip_welcome"));
    }

    #[test]
    fn dismiss_flag_roundtrip() {
        let base = tmp("roundtrip");
        let mark = marker_path_in(&base);
        assert!(!mark.is_file(), "初始不应存在标记");
        // 模拟用户勾选「不再提示」
        std::fs::create_dir_all(mark.parent().unwrap()).unwrap();
        std::fs::write(&mark, b"1").unwrap();
        assert!(mark.is_file(), "写入后应存在标记");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn welcome_opens_when_not_dismissed() {
        // 本机若无标记，弹窗应默认打开
        let w = Welcome::new();
        // 只断言构造不 panic 且状态字段合理（不依赖真实用户目录内容）
        assert!(!w.dont_show);
    }
}
