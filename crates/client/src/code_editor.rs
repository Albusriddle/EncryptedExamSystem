//! 代码简答题编辑器：语法高亮 + 行号。
//!
//! 只影响显示：`TextEdit` 的底层字符串、答题卡 JSON、加密格式都不变。
//! 语言来自题目的 `code_language`；未知/缺省按纯文本处理。

use std::sync::{Mutex, OnceLock};

use eframe::egui;
use eframe::egui::text::LayoutJob;
use eframe::egui::{Color32, FontId, TextFormat, TextStyle};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// 单题代码答案上限（字节），与 `gui::code_value` 保持一致。
pub const CODE_MAX_BYTES: usize = 1024 * 1024;
/// 超过此大小不做语法分析，只显示等宽文本，避免每帧解析 1 MiB。
const HIGHLIGHT_MAX_BYTES: usize = 256 * 1024;
/// 单行渲染宽度上限，避免极端长行创建超大 Galley。
const MAX_EDIT_WIDTH: f32 = 4096.0;

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn themes() -> &'static ThemeSet {
    static SET: OnceLock<ThemeSet> = OnceLock::new();
    SET.get_or_init(ThemeSet::load_defaults)
}

fn theme(ui: &egui::Ui) -> &'static Theme {
    let set = themes();
    let name = if ui.visuals().dark_mode {
        "base16-ocean.dark"
    } else {
        "InspiredGitHub"
    };
    set.themes
        .get(name)
        .or_else(|| set.themes.values().next())
        .expect("syntect 默认主题不应为空")
}

struct CacheEntry {
    key: u64,
    galley: std::sync::Arc<egui::Galley>,
}

/// 小型 LRU：一份试卷可能有多道 code 题，单条缓存会互相冲刷。
const CACHE_MAX: usize = 8;
static CACHE: Mutex<Vec<CacheEntry>> = Mutex::new(Vec::new());

fn cache_key(text: &str, language: Option<&str>, width: f32, dark: bool) -> u64 {
    let mut h = blake3::Hasher::new();
    h.update(text.as_bytes());
    h.update(language.unwrap_or("").as_bytes());
    h.update(&width.to_bits().to_le_bytes());
    h.update(&[u8::from(dark)]);
    u64::from_le_bytes(h.finalize().as_bytes()[..8].try_into().unwrap())
}

fn line_count(text: &str) -> usize {
    text.lines().count().max(1) + usize::from(text.ends_with('\n'))
}

fn monospace(ui: &egui::Ui) -> FontId {
    TextStyle::Monospace.resolve(ui.style())
}

fn text_format(ui: &egui::Ui, color: Color32) -> TextFormat {
    TextFormat::simple(monospace(ui), color)
}

fn plain_job(ui: &egui::Ui, text: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.append(text, 0.0, text_format(ui, ui.visuals().text_color()));
    job
}

/// 生成语法高亮后的 LayoutJob；`text` 内容不会改变。
/// 从题干第一个 fenced code block 的语言标记推断代码语言。
pub fn infer_language(stem: &str) -> Option<&'static str> {
    for line in stem.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("```") else {
            continue;
        };
        let token = rest.trim().to_ascii_lowercase();
        return match token.as_str() {
            "python" | "py" => Some("python"),
            "c" => Some("c"),
            "cpp" | "c++" | "cxx" => Some("cpp"),
            "java" => Some("java"),
            "rust" | "rs" => Some("rust"),
            "javascript" | "js" => Some("javascript"),
            "typescript" | "ts" => Some("typescript"),
            "go" | "golang" => Some("go"),
            "sql" => Some("sql"),
            "bash" | "sh" | "shell" => Some("bash"),
            _ => None,
        };
    }
    None
}

/// 按**代码内容**猜测语言（只在学生点“自动识别语言”时调用）。
///
/// 顺序：题干围栏 → 内容特征。识别不出返回 `None`，绝不猜。
pub fn detect_language(stem: &str, code: &str) -> Option<&'static str> {
    if let Some(lang) = infer_language(stem) {
        return Some(lang);
    }
    let lower = code.to_ascii_lowercase();
    let has = |needle: &str| lower.contains(needle);
    if has("#include <iostream>") || has("std::") || has("using namespace std") {
        return Some("cpp");
    }
    if has("#include <stdio.h>") || has("int main(") || has("printf(") || has("scanf(") {
        return Some("c");
    }
    if has("public static void") || has("system.out.println") || has("import java.") {
        return Some("java");
    }
    if has("fn main") || has("let mut ") || has("impl ") || has("println!(") {
        return Some("rust");
    }
    if has("package main") || has("func main(") || has("fmt.println") {
        return Some("go");
    }
    if has("interface ") && (has(": string") || has(": number")) {
        return Some("typescript");
    }
    if has("function ") || has("console.log(") || has("=>") || has("const ") {
        return Some("javascript");
    }
    if has("#!/bin/bash") || has("#!/bin/sh") || has("echo $") {
        return Some("bash");
    }
    if lower.trim_start().starts_with("select ") || has("select ") && has(" from ") {
        return Some("sql");
    }
    if has("def ") || has("import ") || has("print(") || lower.trim_start().starts_with("class ") {
        return Some("python");
    }
    None
}

fn highlighted_job(ui: &egui::Ui, text: &str, language: Option<&str>) -> LayoutJob {
    if text.len() > HIGHLIGHT_MAX_BYTES {
        return plain_job(ui, text);
    }
    let Some(language) = language else {
        return plain_job(ui, text);
    };
    let syntaxes = syntaxes();
    let Some(syntax) = syntaxes.find_syntax_by_token(language) else {
        return plain_job(ui, text);
    };
    let mut highlighter = HighlightLines::new(syntax, theme(ui));
    let mut job = LayoutJob::default();
    let font = monospace(ui);
    for line in LinesWithEndings::from(text) {
        let Ok(ranges) = highlighter.highlight_line(line, syntaxes) else {
            return plain_job(ui, text);
        };
        for (style, piece) in ranges {
            let color =
                Color32::from_rgb(style.foreground.r, style.foreground.g, style.foreground.b);
            let mut fmt = TextFormat::simple(font.clone(), color);
            fmt.italics = style.font_style.contains(FontStyle::ITALIC);
            fmt.underline = if style.font_style.contains(FontStyle::UNDERLINE) {
                egui::Stroke::new(1.0_f32, color)
            } else {
                egui::Stroke::NONE
            };
            job.append(piece, 0.0, fmt);
        }
    }
    job
}

/// 代码编辑器的渲染结果。
pub struct CodeEditorOutcome {
    pub response: egui::Response,
    /// 学生本帧**显式**改了语言时返回新值（用于写入答题卡）。
    pub chosen_language: Option<String>,
}

/// 决定本次显示用哪种语言。
///
/// 优先级：学生已选（且在允许范围内）→ 老师允许的第一个 → 题干围栏 → 无。
/// **内容启发式不在此自动触发**，只有学生点“自动识别语言”才会用。
fn effective_language(allowed: &[String], stored: Option<&str>, stem: &str) -> Option<String> {
    if let Some(s) = stored
        && (allowed.is_empty() || allowed.iter().any(|a| a == s))
    {
        return Some(s.to_string());
    }
    if let Some(first) = allowed.first() {
        return Some(first.clone());
    }
    infer_language(stem).map(str::to_string)
}

/// 渲染代码编辑器：语言选择 + 行号 + 语法高亮。
///
/// - `allowed`：老师允许的语言；1 个只读、多个可选、空表示未限制；
/// - `stored`：学生此前显式选择并保存在答题卡里的语言；
/// - 内容启发式只在“自动识别语言”按钮被点击时运行。
#[allow(clippy::too_many_arguments)]
pub fn render_code_editor(
    ui: &mut egui::Ui,
    text: &mut String,
    editable: bool,
    idx: usize,
    allowed: &[String],
    stored: Option<&str>,
    stem: &str,
) -> CodeEditorOutcome {
    let mut chosen_language: Option<String> = None;
    let hint_id = egui::Id::new(("code_lang_hint", idx));
    let mut hint: Option<String> = ui.data(|d| d.get_temp(hint_id));

    // ---- 语言选择行 ----
    ui.horizontal_wrapped(|ui| {
        ui.label("语言：");
        if allowed.len() == 1 {
            // 老师只允许一种：只读灰色显示，不给选择机会。
            ui.add_enabled(
                false,
                egui::Label::new(egui::RichText::new(display_name(&allowed[0])).weak()),
            );
        } else {
            let current = effective_language(allowed, stored, stem);
            let current_label = current
                .as_deref()
                .map(display_name)
                .unwrap_or_else(|| "请选择".to_string());
            let mut next: Option<String> = None;
            egui::ComboBox::from_id_salt(("code_lang", idx))
                .selected_text(current_label)
                .width(150.0)
                .show_ui(ui, |ui| {
                    if allowed.is_empty() {
                        for lang in crate::code_editor::ALL_LANGUAGES {
                            let selected = current.as_deref() == Some(*lang);
                            if ui.selectable_label(selected, display_name(lang)).clicked() {
                                next = Some((*lang).to_string());
                            }
                        }
                    } else {
                        for lang in allowed {
                            let selected = current.as_deref() == Some(lang.as_str());
                            if ui.selectable_label(selected, display_name(lang)).clicked() {
                                next = Some(lang.clone());
                            }
                        }
                    }
                });
            if let Some(lang) = next
                && editable
            {
                chosen_language = Some(lang);
                hint = None;
            }
        }

        // 自动识别：单语言题没有意义，直接隐藏。
        if allowed.len() != 1 {
            if ui.button("自动识别语言").clicked() {
                if text.trim().is_empty() {
                    hint = Some("请先粘贴代码，再自动识别。".to_string());
                } else if let Some(lang) = detect_language(stem, text) {
                    if allowed.is_empty() || allowed.iter().any(|a| a == lang) {
                        chosen_language = Some(lang.to_string());
                        hint = Some(format!("已识别为 {}", display_name(lang)));
                    } else {
                        let names: Vec<String> = allowed.iter().map(|a| display_name(a)).collect();
                        hint = Some(format!(
                            "识别为 {}，但本题只允许 {}",
                            display_name(lang),
                            names.join(" / ")
                        ));
                    }
                } else {
                    hint = Some("未能识别语言，请手动选择。".to_string());
                }
            }
        } else {
            hint = None;
        }

        if allowed.is_empty() {
            ui.label(
                egui::RichText::new("本题老师出题时未限制语言，请以考场老师通知为准。")
                    .size(12.0)
                    .color(ui.visuals().warn_fg_color),
            );
        }
    });

    if let Some(msg) = &hint {
        ui.label(
            egui::RichText::new(msg)
                .size(12.0)
                .color(ui.visuals().weak_text_color()),
        );
    }
    ui.data_mut(|d| d.insert_temp(hint_id, hint));

    let language = effective_language(allowed, stored, stem);
    let language = language.as_deref();

    let lines = line_count(text);
    let line_numbers = (1..=lines)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let gutter_color = ui.visuals().weak_text_color();

    let font = monospace(ui);
    let char_w = ui.fonts(|f| f.glyph_width(&font, ' '));
    let max_chars = text.lines().map(|l| l.chars().count()).max().unwrap_or(1);
    let edit_width = ((max_chars as f32 + 2.0) * char_w).clamp(240.0, MAX_EDIT_WIDTH);

    let mut response = None;
    ui.horizontal_top(|ui| {
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(line_numbers)
                .monospace()
                .color(gutter_color)
                .size(font.size),
        );
        ui.add_space(4.0);
        egui::ScrollArea::horizontal()
            .id_salt(("code_scroll", idx))
            .show(ui, |ui| {
                let mut layouter = |ui: &egui::Ui, text: &str, wrap_width: f32| {
                    let dark = ui.visuals().dark_mode;
                    let key = cache_key(text, language, wrap_width, dark);
                    if let Ok(mut cache) = CACHE.lock()
                        && let Some(pos) = cache.iter().position(|e| e.key == key)
                    {
                        let entry = cache.remove(pos);
                        let galley = entry.galley.clone();
                        cache.push(entry);
                        return galley;
                    }
                    let mut job = highlighted_job(ui, text, language);
                    job.wrap.max_width = wrap_width;
                    let galley = ui.fonts(|f| f.layout_job(job));
                    if let Ok(mut cache) = CACHE.lock() {
                        if cache.len() >= CACHE_MAX {
                            cache.remove(0);
                        }
                        cache.push(CacheEntry {
                            key,
                            galley: galley.clone(),
                        });
                    }
                    galley
                };
                response = Some(
                    ui.add_enabled(
                        editable,
                        egui::TextEdit::multiline(text)
                            .id(egui::Id::new(("answer_editor", idx)))
                            .code_editor()
                            .char_limit(CODE_MAX_BYTES)
                            .desired_rows(lines)
                            .desired_width(edit_width)
                            .layouter(&mut layouter)
                            .hint_text("请粘贴完整代码，保留缩进；本题由老师人工评阅"),
                    ),
                );
            });
    });

    CodeEditorOutcome {
        response: response.expect("代码编辑器必须渲染一次"),
        chosen_language,
    }
}

/// 全部受支持语言的展示顺序（老师未限制时使用）。
pub const ALL_LANGUAGES: &[&str] = &[
    "python",
    "c",
    "cpp",
    "java",
    "rust",
    "javascript",
    "typescript",
    "go",
    "sql",
    "bash",
    "text",
];

/// 语言内部名 → 界面显示名。
pub fn display_name(lang: &str) -> String {
    match lang {
        "python" => "Python".into(),
        "c" => "C".into(),
        "cpp" => "C++".into(),
        "java" => "Java".into(),
        "rust" => "Rust".into(),
        "javascript" => "JavaScript".into(),
        "typescript" => "TypeScript".into(),
        "go" => "Go".into(),
        "sql" => "SQL".into(),
        "bash" => "Bash".into(),
        "text" => "纯文本".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_ui(f: impl FnOnce(&egui::Ui)) {
        let mut f = Some(f);
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                if let Some(f) = f.take() {
                    f(ui);
                }
            });
        });
    }

    #[test]
    fn line_count_includes_trailing_blank_line() {
        assert_eq!(line_count(""), 1);
        assert_eq!(line_count("a"), 1);
        assert_eq!(line_count("a\nb"), 2);
        assert_eq!(line_count("a\n"), 2);
    }

    #[test]
    fn highlight_preserves_every_character() {
        let code = "def f(x):\n\treturn \"a\\\\tb\"  # 中文\n";
        run_ui(|ui| {
            let job = highlighted_job(ui, code, Some("python"));
            assert_eq!(job.text, code);
        });
    }

    #[test]
    fn infer_language_from_fenced_stem() {
        assert_eq!(infer_language("```python\nprint(1)\n```"), Some("python"));
        assert_eq!(infer_language("```cpp\nint x;\n```"), Some("cpp"));
        assert_eq!(infer_language("no fence"), None);
    }

    #[test]
    fn detect_language_from_content() {
        assert_eq!(
            detect_language("样例输出", "#include <stdio.h>\nint main(void){}\n"),
            Some("c")
        );
        assert_eq!(
            detect_language("样例输出", "#include <iostream>\nstd::cout << 1;"),
            Some("cpp")
        );
        assert_eq!(
            detect_language("样例输出", "def f(x):\n    return x\n"),
            Some("python")
        );
        assert_eq!(detect_language("样例输出", "完全看不懂的一段话"), None);
    }

    #[test]
    fn effective_language_priority() {
        let allowed = vec!["c".to_string(), "cpp".to_string()];
        // 学生已选且在允许范围内 → 用学生的
        assert_eq!(
            effective_language(&allowed, Some("cpp"), "").as_deref(),
            Some("cpp")
        );
        // 学生选择越权 → 回退老师第一个允许项
        assert_eq!(
            effective_language(&allowed, Some("python"), "").as_deref(),
            Some("c")
        );
        // 没选 → 老师第一个允许项
        assert_eq!(effective_language(&allowed, None, "").as_deref(), Some("c"));
        // 允许为空 → 用题干围栏
        assert_eq!(
            effective_language(&[], None, "```python\nx\n```").as_deref(),
            Some("python")
        );
    }

    #[test]
    fn unknown_language_falls_back_to_plain_text() {
        let code = "some code\n";
        run_ui(|ui| {
            let job = highlighted_job(ui, code, Some("not-a-language"));
            assert_eq!(job.text, code);
        });
    }
}
