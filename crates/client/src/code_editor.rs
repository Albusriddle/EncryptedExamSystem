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

/// 渲染代码编辑器：行号 + 语法高亮。
///
/// # 参数
/// - `language`: 题目 `code_language`，如 `python`、`cpp`。
/// - `idx`: 题目下标，用于稳定焦点 ID。
#[allow(clippy::too_many_arguments)]
pub fn render_code_editor(
    ui: &mut egui::Ui,
    text: &mut String,
    editable: bool,
    idx: usize,
    language: Option<&str>,
    stem: &str,
) -> egui::Response {
    let language = language.or_else(|| infer_language(stem));
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
    response.expect("代码编辑器必须渲染一次")
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
    fn unknown_language_falls_back_to_plain_text() {
        let code = "some code\n";
        run_ui(|ui| {
            let job = highlighted_job(ui, code, Some("not-a-language"));
            assert_eq!(job.text, code);
        });
    }
}
