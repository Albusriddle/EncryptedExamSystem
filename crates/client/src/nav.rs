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
/// 跳转目标找不到时最多重试几帧（防止无限重绘）
const MAX_PENDING_RETRIES: u8 = 3;

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
    /// 跳转目标暂时找不到时的重试次数。
    ///
    /// 必须**有上限**：找不到 rect 时如果不加限制地 `request_repaint()`，
    /// 一旦出现（例如试卷被替换）就是永久 100% CPU。
    pending_retries: u8,
    /// 当前视窗顶部那道题（1 起）。由上一帧的矩形算出，用于绿色描边。
    current: Option<usize>,
    /// 每道题是否已作答（按题号 1 起索引，0 号位弃用）。
    answered: Vec<bool>,
    /// 诊断：本帧题号按钮的矩形 `(题号, rect)`，用于验证一行 5 个且不越界。
    button_rects: Vec<(usize, egui::Rect)>,
    /// 诊断：题号面板的实际可用宽度。
    panel_inner_width: f32,
    /// 诊断：面板内区域的左边界（屏幕坐标）。
    panel_left: f32,
    /// 题号栏底部“交卷”按钮被点击；由 MainApp 消费并走统一的二次确认。
    submit_requested: bool,
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

    /// 换卷时重置：清掉上一份试卷的矩形与待跳转请求。
    ///
    /// 折叠状态要保留（那是用户偏好，不是试卷状态）。
    /// 不重置的话，上一份试卷的 rects 会残留：若恰有一个未消费的
    /// pending，新试卷会按旧坐标滚动，甚至滚到错误位置。
    /// 消费题号栏交卷请求。
    pub fn take_submit_requested(&mut self) -> bool {
        std::mem::take(&mut self.submit_requested)
    }

    pub fn reset_for_new_paper(&mut self) {
        self.pending = None;
        self.pending_retries = 0;
        self.rects.clear();
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
            // 上一帧没记录到（例如刚切换试卷）：下一帧再试，但只重试有限次。
            // 无上限重试会让界面永远重绘（CPU 100%）。
            if self.pending_retries < MAX_PENDING_RETRIES {
                self.pending_retries += 1;
                self.pending = Some(idx);
                ui.ctx().request_repaint();
            }
            // 超过上限：丢弃这次跳转，避免卡死
            return;
        };
        self.pending_retries = 0;
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
        self.pending_retries = 0;
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        sections: &[(String, Vec<(String, usize)>)],
        can_submit: bool,
        submitted: bool,
        confirm: bool,
    ) {
        // 每帧重置：按钮矩形只反映本帧布局，否则多帧累积会让诊断失真。
        self.button_rects.clear();
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
                self.panel_inner_width = ui.available_width();
                self.panel_left = ui.min_rect().left();

                // 题目多时卡片自身也要能滚（行数不限）；底部固定留出交卷按钮。
                let bottom_h = 48.0;
                let scroll_h = (ui.available_height() - bottom_h).max(80.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(scroll_h)
                    .show(ui, |ui| {
                        let mut global = 0usize;
                        for (title, parts) in sections {
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(title)
                                    .size(12.0)
                                    .color(ui.visuals().weak_text_color()),
                            );
                            ui.add_space(2.0);
                            for (part_title, count) in parts {
                                if *count == 0 {
                                    continue;
                                }
                                if !part_title.is_empty() {
                                    ui.label(
                                        egui::RichText::new(format!("  {part_title}"))
                                            .size(11.0)
                                            .color(ui.visuals().weak_text_color()),
                                    );
                                }
                                self.grid(ui, &mut global, *count);
                                ui.add_space(2.0);
                            }
                        }
                    });

                ui.add_space(4.0);
                ui.separator();
                let label = if submitted {
                    "✔ 已交卷"
                } else if confirm {
                    "确认交卷（不可再修改）"
                } else {
                    "交卷"
                };
                let enabled = can_submit && !submitted;
                if ui
                    .add_enabled(
                        enabled,
                        egui::Button::new(label).min_size(egui::vec2(ui.available_width(), 28.0)),
                    )
                    .clicked()
                {
                    self.submit_requested = true;
                }
            });
    }

    /// 把题号卡与题目矩形写到文件，供自动化验证。
    /// 写一份"时钟与收卷状态"快照，供自动化验证。
    ///
    /// 与 `dump_debug` 分开：后者需要题目矩形，收卷后不再渲染题目，
    /// 但这一份必须持续更新，否则无法证明"时间在走、到点变 true"。
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub fn dump_state(&self, path: &std::path::Path, now: Option<u64>, expired: bool) {
        let text = format!(
            "now_unix={}\nexpired={}\n",
            now.map(|n| n.to_string()).unwrap_or_else(|| "none".into()),
            expired
        );
        let _ = std::fs::write(path, text);
    }

    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub fn dump_debug(
        &self,
        sections: &[(String, Vec<(String, usize)>)],
        scroll_offset: egui::Vec2,
        path: &std::path::Path,
    ) {
        let total: usize = sections
            .iter()
            .flat_map(|(_, parts)| parts.iter().map(|(_, n)| *n))
            .sum();
        let mut out = String::new();
        out.push_str(&format!("collapsed={}\n", self.collapsed));
        out.push_str(&format!("panel_width_pt={:.1}\n", expanded_width()));
        out.push_str(&format!("per_row={PER_ROW}\n"));
        out.push_str(&format!("total_questions={total}\n"));
        out.push_str(&format!(
            "current={}\n",
            self.current
                .map(|i| i.to_string())
                .unwrap_or_else(|| "none".into())
        ));
        let mut flags = String::new();
        for i in 1..=total {
            flags.push(if self.answered.get(i).copied().unwrap_or(false) {
                '1'
            } else {
                '0'
            });
        }
        out.push_str(&format!("answered={flags}\n"));
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
        for (t, parts) in sections {
            out.push_str(&format!("  {t}:\n"));
            for (pt, n) in parts {
                out.push_str(&format!("    {pt}: {n}\n"));
            }
        }
        out.push_str(&format!(
            "panel_inner_width={:.1}\n",
            self.panel_inner_width
        ));
        out.push_str("button_rows=\n");
        // 按 y 分组统计每行几个、最右边界是否超出面板
        let mut rows: Vec<(i32, Vec<f32>)> = Vec::new();
        for (_, r) in &self.button_rects {
            let y = r.min.y.round() as i32;
            match rows.iter_mut().find(|(ry, _)| (*ry - y).abs() <= 4) {
                Some((_, v)) => v.push(r.max.x),
                None => rows.push((y, vec![r.max.x])),
            }
        }
        rows.sort_by_key(|(y, _)| *y);
        for (y, xs) in &rows {
            let right = xs.iter().cloned().fold(f32::MIN, f32::max);
            let rel = right - self.panel_left;
            out.push_str(&format!(
                "  y={y} count={} right_rel={rel:.1}/{:.1} overflow={}\n",
                xs.len(),
                self.panel_inner_width,
                rel > self.panel_inner_width + 1.0
            ));
        }
        out.push_str("buttons=\n");
        for (i, r) in &self.button_rects {
            out.push_str(&format!(
                "  #{i}: x_rel={:.1} y={:.1} w={:.1}\n",
                r.min.x - self.panel_left,
                r.min.y,
                r.width()
            ));
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
            // 关键：Grid 的默认 min_cell_size 取 ui.spacing().interact_size
            // （40×18），会把每列撑到 40pt。5 列 = 200 + 间距 24 = 224pt，
            // 远超面板宽度 184pt —— 第 5 个按钮就被挤到窗口外面去了
            // （v0.1.8 实测每行只能看到 4 个）。
            // 这里显式把最小单元格压到按钮尺寸，列宽才会按真实内容计算。
            .min_col_width(BTN)
            .min_row_height(BTN)
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

    /// 题号按钮的三态配色。
    ///
    /// 抽成纯函数是为了能单测：颜色是"肉眼才看得出"的东西，没有断言就只能靠人看。
    pub fn button_style(
        v: &egui::Visuals,
        answered: bool,
        current: bool,
        hovered: bool,
        pressed: bool,
    ) -> (egui::Color32, egui::Color32, egui::Color32, f32) {
        const BLUE: egui::Color32 = egui::Color32::from_rgb(59, 130, 246);
        const GREEN: egui::Color32 = egui::Color32::from_rgb(60, 160, 90);

        // 未答/悬停/按下全部取自主题：写死深灰在白天主题下会变成"深灰底 + 近黑字"，
        // 客户实测看不清第几题（v0.4.1 反馈）。
        let fill = if answered {
            if pressed {
                egui::Color32::from_rgb(37, 99, 235)
            } else {
                BLUE
            }
        } else if pressed {
            v.widgets.active.bg_fill
        } else if hovered {
            v.widgets.hovered.bg_fill
        } else {
            v.widgets.inactive.bg_fill
        };
        let text = if answered {
            egui::Color32::WHITE
        } else {
            v.strong_text_color()
        };
        let (stroke, width) = if current {
            (GREEN, 2.5_f32)
        } else {
            (egui::Color32::TRANSPARENT, 0.0)
        };
        (fill, text, stroke, width)
    }

    /// 选中选项的（底色, 左侧竖条色）。
    ///
    /// 不要用 `selection.bg_fill.linear_multiply()`：浅色主题的 selection 本身就是
    /// 浅蓝，再乘 0.35 几乎变白，白底上"选中"等于看不见（客户 v0.6.6 反馈）。
    /// 这里按主题给固定色，并配一条 3px 竖条，深浅主题都能一眼分辨。
    pub fn option_fill(v: &egui::Visuals) -> (egui::Color32, egui::Color32) {
        if v.dark_mode {
            (
                egui::Color32::from_rgb(30, 58, 95),
                egui::Color32::from_rgb(96, 165, 250),
            )
        } else {
            (
                egui::Color32::from_rgb(219, 234, 254),
                egui::Color32::from_rgb(37, 99, 235),
            )
        }
    }

    /// 相对亮度（sRGB → 线性，再按 0.2126/0.7152/0.0722 加权）。仅测试用。
    #[cfg(test)]
    pub fn luminance(c: egui::Color32) -> f32 {
        fn lin(v: u8) -> f32 {
            let s = v as f32 / 255.0;
            if s <= 0.039_28 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    }

    /// 视窗顶部那道题：取"顶部不超过视窗顶 + 40pt"的最后一道。
    pub fn current_from_rects(
        rects: &std::collections::HashMap<usize, egui::Rect>,
        viewport_top: f32,
    ) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (idx, r) in rects {
            if r.top() <= viewport_top + 40.0 && best.is_none_or(|(_, top)| r.top() >= top) {
                best = Some((*idx, r.top()));
            }
        }
        best.map(|(i, _)| i)
    }

    /// 记录每帧的作答状态（按题号 1 起）。
    pub fn set_answered(&mut self, flags: Vec<bool>) {
        self.answered = flags;
    }

    /// 用本帧的题目矩形 + 视窗顶边，更新"当前题"，供下一帧描边。
    pub fn update_current(&mut self, viewport_top: f32) {
        self.current = Self::current_from_rects(&self.rects, viewport_top);
    }

    /// 单个圆形按钮。
    fn circle_button(&mut self, ui: &mut egui::Ui, idx: usize) {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(BTN, BTN), egui::Sense::click());
        let answered = self.answered.get(idx).copied().unwrap_or(false);
        let current = self.current == Some(idx);
        let (fill, text_color, stroke, width) = Self::button_style(
            ui.visuals(),
            answered,
            current,
            resp.hovered(),
            resp.is_pointer_button_down_on(),
        );
        let painter = ui.painter();
        painter.circle_filled(rect.center(), BTN / 2.0, fill);
        if width > 0.0 {
            painter.circle_stroke(rect.center(), BTN / 2.0, egui::Stroke::new(width, stroke));
        }
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            idx.to_string(),
            egui::FontId::proportional(12.0),
            text_color,
        );
        self.button_rects.push((idx, rect));
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

    /// 面板宽度必须装得下一行 5 个按钮。
    ///
    /// v0.1.8 的真实 bug：Grid 默认 min_cell_size 取 interact_size(40)，
    /// 每列被撑到 40pt，5 列需要 200+24=224pt，而面板内宽只有 168pt，
    /// 第 5 个按钮被挤到窗口外（用户反馈"一行只有 4 个"）。
    /// 修法是 min_col_width(BTN)。这条测试锁住"必要宽度 ≤ 面板内宽"。
    #[test]
    fn row_of_five_fits_in_panel() {
        // 面板 exact_width 会减去左右各 8pt 内边距
        const PANEL_PADDING: f32 = 8.0 * 2.0;
        let inner = expanded_width() - PANEL_PADDING;
        let needed = PER_ROW as f32 * BTN + (PER_ROW - 1) as f32 * GAP;
        assert!(
            needed <= inner,
            "一行 {PER_ROW} 个按钮需要 {needed}pt，但面板内宽只有 {inner}pt"
        );
    }

    /// Grid 的最小列宽必须等于按钮宽度，否则列会被 interact_size 撑宽。
    #[test]
    fn grid_min_col_width_matches_button() {
        // 若误用默认值（interact_size.x = 40），5 列必然溢出
        const EGUI_DEFAULT_INTERACT_W: f32 = 40.0;
        let with_default = PER_ROW as f32 * EGUI_DEFAULT_INTERACT_W + (PER_ROW - 1) as f32 * GAP;
        let with_fix = PER_ROW as f32 * BTN + (PER_ROW - 1) as f32 * GAP;
        const PANEL_PADDING: f32 = 8.0 * 2.0;
        let inner = expanded_width() - PANEL_PADDING;
        assert!(
            with_default > inner,
            "默认列宽本应溢出（这正是当初的 bug），否则本测试失去意义"
        );
        assert!(with_fix <= inner, "修复后必须能装下");
    }

    /// 跳转重试必须有上限：无上限会在丢目标时永久重绘。
    #[test]
    fn pending_retries_are_bounded() {
        // 模拟连续 miss 的行为
        let mut retries: u8 = 0;
        let mut repaints = 0;
        for _ in 0..100 {
            if retries < MAX_PENDING_RETRIES {
                retries += 1;
                repaints += 1;
            }
        }
        assert_eq!(repaints, MAX_PENDING_RETRIES as i32, "重绘次数必须封顶");
        assert!(retries <= MAX_PENDING_RETRIES);
    }

    /// 换卷重置：清矩形与待跳转，但保留折叠偏好。
    #[test]
    fn reset_for_new_paper_keeps_collapsed() {
        let mut s = NavState::default();
        s.record(
            1,
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(10.0, 10.0)),
        );
        s.debug_jump(5);
        s.collapsed = true;

        s.reset_for_new_paper();

        assert!(s.rects.is_empty(), "矩形必须清空");
        assert!(s.pending.is_none(), "待跳转必须清空");
        assert_eq!(s.pending_retries, 0);
        assert!(s.is_collapsed(), "折叠偏好应保留");
    }

    /// 圆形按钮的边长必须等于直径，否则会画成椭圆。
    #[test]
    fn button_is_square_so_circle_stays_round() {
        let size = egui::vec2(BTN, BTN);
        assert_eq!(size.x, size.y, "圆形按钮必须等宽等高");
        assert_eq!(BTN / 2.0, 14.0);
    }
}

#[cfg(test)]
mod nav_state_tests {
    use super::*;

    #[test]
    fn unanswered_uses_theme_colors_in_both_themes() {
        for v in [egui::Visuals::light(), egui::Visuals::dark()] {
            let (fill, text, stroke, w) = NavState::button_style(&v, false, false, false, false);
            assert_eq!(fill, v.widgets.inactive.bg_fill, "未答底色必须跟随主题");
            assert_eq!(text, v.strong_text_color());
            let (fg, bg) = (text.r() as i32, fill.r() as i32);
            // 核心回归：白天主题下不能再是"深灰底 + 近黑字"
            assert!((fg - bg).abs() > 80, "前景背景亮度差太小：fg={fg} bg={bg}");
            assert_eq!(
                (stroke, w),
                (egui::Color32::TRANSPARENT, 0.0),
                "非当前题不描边"
            );
        }
    }

    #[test]
    fn answered_is_blue_with_white_text() {
        let v = egui::Visuals::light();
        let (fill, text, _s, _w) = NavState::button_style(&v, true, false, false, false);
        assert_eq!(fill, egui::Color32::from_rgb(59, 130, 246));
        assert_eq!(text, egui::Color32::WHITE);
    }

    #[test]
    fn current_has_green_stroke() {
        let v = egui::Visuals::dark();
        let (_fill, _text, stroke, w) = NavState::button_style(&v, false, true, false, false);
        assert_eq!(stroke, egui::Color32::from_rgb(60, 160, 90));
        assert!(w > 2.0, "当前题描边要明显：{w}");
    }

    #[test]
    fn current_item_follows_viewport() {
        let mut rects = std::collections::HashMap::new();
        rects.insert(
            1,
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 100.0)),
        );
        rects.insert(
            2,
            egui::Rect::from_min_size(egui::pos2(0.0, 120.0), egui::vec2(10.0, 100.0)),
        );
        rects.insert(
            3,
            egui::Rect::from_min_size(egui::pos2(0.0, 240.0), egui::vec2(10.0, 100.0)),
        );

        assert_eq!(NavState::current_from_rects(&rects, 0.0), Some(1));
        assert_eq!(NavState::current_from_rects(&rects, 130.0), Some(2));
        assert_eq!(NavState::current_from_rects(&rects, 300.0), Some(3));
        assert_eq!(
            NavState::current_from_rects(&std::collections::HashMap::new(), 0.0),
            None
        );
    }

    #[test]
    fn option_fill_visible_in_both_themes() {
        let light = NavState::option_fill(&egui::Visuals::light());
        let dark = NavState::option_fill(&egui::Visuals::dark());

        assert!(
            NavState::luminance(light.0) > 0.75,
            "浅色主题底色应足够浅（白底上能看出边界）"
        );
        assert!(NavState::luminance(dark.0) < 0.10, "深色主题底色应足够深");
        assert!(
            (NavState::luminance(light.1) - NavState::luminance(light.0)).abs() > 0.25,
            "浅色主题竖条要能和底色区分"
        );
        assert!(
            (NavState::luminance(dark.1) - NavState::luminance(dark.0)).abs() > 0.25,
            "深色主题竖条要能和底色区分"
        );
    }
}
