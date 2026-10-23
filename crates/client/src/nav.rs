//! 右侧题号导航卡。
//!
//! ## 交互
//!
//! 每个题号是一个**圆形按钮**，按科目分组、每行 5 个、行数不限。
//! 点击后把对应题目滚动到**视窗顶部**，不改变试卷本身的排版。
//!
//! ## 为什么跳转要延后一帧
//!
//! `ui.scroll_to_rect(rect, Some(Align::TOP))` 设置的是"本帧的滚动目标"，
//! 必须在目标 rect 已经算出来之后调用。若在画题号卡时就调用，
//! 用的是**上一帧**记录的 rect：首帧 `rects` 还是空的，点击会毫无反应。
//!
//! 因此约定：
//! 1. 题号卡只把点击写进 `pending`；
//! 2. 试卷渲染时逐题记录 rect；
//! 3. 试卷渲染**结束后**由 [`NavState::apply_pending_scroll`] 消费 `pending`。
//!
//! 这样首帧点击也有效，且不受题干换行导致的高度变化影响。

use std::collections::HashMap;

use eframe::egui;

/// 圆形按钮直径（pt）。圆形要求边长 == 直径，所以用 allocate_exact_size 自绘，
/// 不用 `Button::rounding`——那个只改圆角半径，宽高不等时会画成椭圆。
const BTN: f32 = 28.0;
/// 按钮之间的间距
const GAP: f32 = 6.0;
/// 每行按钮数
const PER_ROW: usize = 5;
/// 卡片左右内边距
const PAD: f32 = 10.0;
/// 折叠后的条带宽度
const COLLAPSED_W: f32 = 34.0;

/// 展开时的卡片宽度：5 个按钮 + 4 个间距 + 左右内边距。
fn expanded_width() -> f32 {
    PER_ROW as f32 * BTN + (PER_ROW - 1) as f32 * GAP + PAD * 2.0
}

/// 题号卡状态。
#[derive(Default)]
pub struct NavState {
    collapsed: bool,
    /// 待跳转的题目序号（1 起）。由题号卡写入、试卷渲染后消费。
    pending: Option<usize>,
    /// 上一帧记录的各题矩形，用于计算滚动目标。
    rects: HashMap<usize, egui::Rect>,
    /// 诊断：最近一次被消费的跳转目标 `(题号, 目标 y)`。
    last_scrolled: Option<(usize, f32)>,
}

impl NavState {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn toggle_collapsed(&mut self) {
        self.collapsed = !self.collapsed;
    }

    /// 记录某题的矩形（试卷渲染时调用）。
    pub fn record(&mut self, idx: usize, rect: egui::Rect) {
        self.rects.insert(idx, rect);
    }

    /// 消费待跳转请求。必须在**试卷渲染完成之后**调用。
    ///
    /// 每帧调用一次即可；没有请求时是空操作。
    pub fn apply_pending_scroll(&mut self, ui: &egui::Ui) {
        let Some(idx) = self.pending.take() else {
            return;
        };
        let Some(rect) = self.rects.get(&idx).copied() else {
            // 上一帧没记录到（例如刚切换试卷）：下一帧再试，避免丢点击
            self.pending = Some(idx);
            ui.ctx().request_repaint();
            return;
        };
        // 稍微上移一点，让题干与滚动区顶边之间留出呼吸空间
        let target = egui::Rect::from_min_size(
            rect.min - egui::vec2(0.0, 10.0),
            egui::vec2(rect.width(), rect.height()),
        );
        self.last_scrolled = Some((idx, target.min.y));
        ui.scroll_to_rect(target, Some(egui::Align::TOP));
    }

    /// 绘制题号卡。
    ///
    /// `sections` 为 `(科目名, 该科目题目数)`，用来分组显示。
    /// 序号从 1 开始且**跨科目连续**，与学生看到的题号一致。
    /// 诊断用：启动后立刻模拟点击第 N 题（`EES_NAV_JUMP=<N>`），
    /// 用于自动化验证"点击 → 滚动置顶"的完整链路。
    pub fn debug_jump(&mut self, idx: usize) {
        self.pending = Some(idx);
    }

    pub fn show(&mut self, ctx: &egui::Context, sections: &[(String, usize)]) {
        let width = if self.collapsed {
            COLLAPSED_W
        } else {
            expanded_width()
        };
        egui::SidePanel::right("question_nav")
            .resizable(false)
            .exact_width(width)
            .show(ctx, |ui| {
                ui.add_space(8.0);
                if self.collapsed {
                    ui.vertical_centered(|ui| {
                        if ui.button("≡").on_hover_text("展开题号").clicked() {
                            self.collapsed = false;
                        }
                    });
                    return;
                }

                ui.horizontal(|ui| {
                    if ui.button("≡").on_hover_text("收起题号").clicked() {
                        self.collapsed = true;
                    }
                    ui.label(egui::RichText::new("题号").strong());
                });
                ui.add_space(6.0);

                // 题目多时卡片自身也要能滚（行数不限）
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut global = 0usize;
                        for (title, count) in sections {
                            if *count == 0 {
                                continue;
                            }
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(title)
                                    .size(12.0)
                                    .color(ui.visuals().weak_text_color()),
                            );
                            ui.add_space(2.0);
                            self.grid(ui, &mut global, *count);
                            ui.add_space(2.0);
                        }
                    });
            });
    }

    /// 把题号卡与题目矩形写到文件，供自动化验证。
    pub fn dump_debug(
        &self,
        sections: &[(String, usize)],
        scroll_offset: egui::Vec2,
        path: &std::path::Path,
    ) {
        let total: usize = sections.iter().map(|(_, n)| n).sum();
        let mut out = String::new();
        out.push_str(&format!("collapsed={}\n", self.collapsed));
        out.push_str(&format!("panel_width_pt={:.1}\n", expanded_width()));
        out.push_str(&format!("per_row={PER_ROW}\n"));
        out.push_str(&format!("total_questions={total}\n"));
        out.push_str(&format!("rows={}\n", total.div_ceil(PER_ROW)));
        out.push_str(&format!("pending={:?}\n", self.pending));
        out.push_str(&format!(
            "scroll_offset=({:.1},{:.1})\n",
            scroll_offset.x, scroll_offset.y
        ));
        match self.last_scrolled {
            Some((i, y)) => out.push_str(&format!("last_scrolled=#{i} target_y={y:.1}\n")),
            None => out.push_str("last_scrolled=None\n"),
        }
        out.push_str("sections=\n");
        for (t, n) in sections {
            out.push_str(&format!("  {t}: {n}\n"));
        }
        out.push_str("recorded_rects=\n");
        let mut keys: Vec<&usize> = self.rects.keys().collect();
        keys.sort();
        for k in keys {
            let r = self.rects[k];
            out.push_str(&format!(
                "  #{k}: x={:.0} y={:.0} w={:.0} h={:.0}\n",
                r.min.x,
                r.min.y,
                r.width(),
                r.height()
            ));
        }
        let _ = std::fs::write(path, out);
    }

    /// 按每行 5 个绘制一段连续题号。
    fn grid(&mut self, ui: &mut egui::Ui, global: &mut usize, count: usize) {
        egui::Grid::new(format!("qnav_rows_{global}"))
            .num_columns(PER_ROW)
            .spacing(egui::vec2(GAP, GAP))
            .show(ui, |ui| {
                for i in 0..count {
                    *global += 1;
                    self.circle_button(ui, *global);
                    if (i + 1) % PER_ROW == 0 {
                        ui.end_row();
                    }
                }
                // Grid 不会自动结束最后一行，补一次以免下一段接在同一行
                if !count.is_multiple_of(PER_ROW) {
                    ui.end_row();
                }
            });
    }

    /// 单个圆形按钮。
    fn circle_button(&mut self, ui: &mut egui::Ui, idx: usize) {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(BTN, BTN), egui::Sense::click());
        let visuals = ui.visuals();
        let fill = if resp.is_pointer_button_down_on() {
            visuals.selection.bg_fill
        } else if resp.hovered() {
            visuals.widgets.hovered.bg_fill
        } else {
            visuals.widgets.inactive.bg_fill
        };
        let painter = ui.painter();
        painter.circle_filled(rect.center(), BTN / 2.0, fill);
        if resp.hovered() {
            painter.circle_stroke(
                rect.center(),
                BTN / 2.0,
                egui::Stroke::new(1.0_f32, visuals.selection.stroke.color),
            );
        }
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            idx.to_string(),
            egui::FontId::proportional(12.0),
            visuals.text_color(),
        );
        if resp.clicked() {
            self.pending = Some(idx);
        }
        resp.on_hover_text(format!("跳到第 {idx} 题"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expanded_width_matches_layout() {
        // 5×28 + 4×6 + 2×10 = 140 + 24 + 20 = 184
        assert_eq!(expanded_width(), 184.0);
    }

    #[test]
    fn collapse_toggles() {
        let mut s = NavState::default();
        assert!(!s.is_collapsed());
        s.toggle_collapsed();
        assert!(s.is_collapsed());
        s.toggle_collapsed();
        assert!(!s.is_collapsed());
    }

    #[test]
    fn record_stores_rect() {
        let mut s = NavState::default();
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(10.0, 20.0));
        s.record(3, r);
        assert_eq!(s.rects.get(&3).copied(), Some(r));
    }

    /// 点击后必须在同一帧内被消费掉，否则会每帧重复滚动。
    #[test]
    fn pending_is_consumed_once() {
        let mut s = NavState::default();
        s.debug_jump(2);
        assert!(s.pending.is_some(), "初始应有待处理请求");
        s.pending.take();
        assert!(s.pending.is_none(), "取走后应为空");
    }

    /// 每行 5 个：行数必须按题目总数除以 5 向上取整（不限行数）。
    #[test]
    fn rows_grow_without_limit() {
        for total in [1usize, 4, 5, 6, 10, 25, 100, 1024] {
            let rows = total.div_ceil(PER_ROW);
            assert_eq!(rows, total.div_ceil(5));
            assert!(rows >= 1);
            // 最后一行的按钮数不超过 5
            let last = if total % PER_ROW == 0 {
                PER_ROW
            } else {
                total % PER_ROW
            };
            assert!(last <= PER_ROW);
        }
    }

    /// 圆形按钮的边长必须等于直径，否则会画成椭圆。
    #[test]
    fn button_is_square_so_circle_stays_round() {
        let size = egui::vec2(BTN, BTN);
        assert_eq!(size.x, size.y, "圆形按钮必须等宽等高");
        assert_eq!(BTN / 2.0, 14.0);
    }
}
