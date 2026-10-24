//! 题干里的 Markdown 子集解析。
//!
//! ## 支持的语法（与 `docs/题库JSON规范.md` 的"支持清单"一致）
//!
//! | 语法 | 例子 |
//! |---|---|
//! | 围栏代码块 | ` ```c … ``` ` |
//! | 行内代码 | `` `int x` `` |
//! | 加粗 | `**重点**` |
//! | 标题 | `# 一级` / `## 二级` / `### 三级` |
//! | 无序列表 | `- 项`（也认 `*` / `+`） |
//! | 有序列表 | `1. 项`（也认 `1)`） |
//! | 引用 | `> 文字` |
//! | 表格 | 竖线表格 + 分隔行 |
//! | 图片 | `![说明](data:image/png;base64,…)` |
//!
//! **不支持**：链接、斜体、删除线、嵌套列表、HTML。
//!
//! ## 两条硬规则
//!
//! 1. **未知/畸形语法一律按文本**，任何输入都不能 panic——
//!    一道坏题不能把整份试卷带崩。
//! 2. 未闭合的围栏、反引号、`**` 都当作普通字符。

/// 表格列对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// 行内片段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    Text(String),
    Code(String),
    Bold(String),
}

/// 块级元素。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Span>),
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<Vec<Span>>,
    },
    Quote(Vec<Span>),
    Code {
        lang: String,
        lines: Vec<String>,
    },
    Table {
        header: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
        aligns: Vec<Align>,
    },
    /// 块级图片；`src` 只接受 data URI（离线自包含，链接一律不支持）。
    Image {
        alt: String,
        src: String,
    },
}

/// 解析缓存：题干每帧都要画，20 题每帧重新解析纯属浪费。
///
/// 键用 blake3(题干) 的前 8 字节；缓存 128 条（一道题反复出现也不会爆）。
pub fn parse_cached(input: &str) -> std::rc::Rc<Vec<Block>> {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    thread_local! {
        static CACHE: RefCell<VecDeque<(u64, std::rc::Rc<Vec<Block>>)>> =
            const { RefCell::new(VecDeque::new()) };
    }
    const LIMIT: usize = 128;
    /// 超过这个大小就不缓存：题干里可能塞着 4MB 的 base64 图片，
    /// 缓存 128 条最坏能留下几百 MB。
    const MAX_CACHE_BYTES: usize = 64 * 1024;

    if input.len() > MAX_CACHE_BYTES {
        return std::rc::Rc::new(parse(input));
    }

    let h = blake3::hash(input.as_bytes());
    let key = u64::from_le_bytes(h.as_bytes()[..8].try_into().unwrap_or_default());

    if let Some(hit) = CACHE.with(|c| {
        c.borrow()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, blocks)| blocks.clone())
    }) {
        return hit;
    }
    let blocks = std::rc::Rc::new(parse(input));
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        c.push_back((key, blocks.clone()));
        while c.len() > LIMIT {
            c.pop_front();
        }
    });
    blocks
}

/// 把题干解析成块列表。
pub fn parse(input: &str) -> Vec<Block> {
    let text = input.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.split('\n').collect();

    let mut blocks: Vec<Block> = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let mut i = 0usize;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed.is_empty() {
            flush_para(&mut blocks, &mut para);
            i += 1;
            continue;
        }

        if trimmed.starts_with("```")
            && let Some((block, consumed)) = take_code(&lines, i)
        {
            flush_para(&mut blocks, &mut para);
            blocks.push(block);
            i += consumed;
            continue;
        }

        if trimmed.starts_with('|') && is_table_start(&lines, i) {
            flush_para(&mut blocks, &mut para);
            // 注意：take_table 返回的是"消耗了几行"，必须累加。
            // 曾经误写成 i = take_table(...)，表格不在第 0 行时 i 会原地踏步，
            // 解析器直接死循环（表现为打开试卷卡死、内存飙升）。
            i += take_table(&lines, i, &mut blocks);
            continue;
        }

        if let Some((alt, src)) = parse_image_line(trimmed) {
            flush_para(&mut blocks, &mut para);
            blocks.push(Block::Image { alt, src });
            i += 1;
            continue;
        }

        if let Some((level, rest)) = parse_heading(trimmed) {
            flush_para(&mut blocks, &mut para);
            blocks.push(Block::Heading {
                level,
                spans: parse_spans(rest),
            });
            i += 1;
            continue;
        }

        if trimmed.starts_with('>') {
            flush_para(&mut blocks, &mut para);
            let mut quoted: Vec<String> = Vec::new();
            while i < lines.len() && lines[i].trim().starts_with('>') {
                quoted.push(
                    lines[i]
                        .trim()
                        .trim_start_matches('>')
                        .trim_start()
                        .to_string(),
                );
                i += 1;
            }
            blocks.push(Block::Quote(parse_spans(&quoted.join("\n"))));
            continue;
        }

        if let Some((ordered, start)) = list_marker(trimmed) {
            flush_para(&mut blocks, &mut para);
            let mut items: Vec<Vec<Span>> = Vec::new();
            while i < lines.len() {
                let t = lines[i].trim();
                if t.is_empty() {
                    break;
                }
                match list_marker(t) {
                    Some((o, _)) if o == ordered => {
                        items.push(parse_spans(strip_list_marker(t)));
                        i += 1;
                    }
                    _ if lines[i].starts_with("  ") && !items.is_empty() => {
                        if let Some(last) = items.last_mut() {
                            last.push(Span::Text(format!("\n{}", t)));
                        }
                        i += 1;
                    }
                    _ => break,
                }
            }
            blocks.push(Block::List {
                ordered,
                start,
                items,
            });
            continue;
        }

        para.push(line.to_string());
        i += 1;
    }
    flush_para(&mut blocks, &mut para);
    blocks
}

fn flush_para(blocks: &mut Vec<Block>, para: &mut Vec<String>) {
    if para.is_empty() {
        return;
    }
    let joined = para.join("\n");
    para.clear();
    if !joined.trim().is_empty() {
        blocks.push(Block::Paragraph(parse_spans(&joined)));
    }
}

fn take_code(lines: &[&str], i: usize) -> Option<(Block, usize)> {
    let lang = lines[i].trim().trim_start_matches('`').trim().to_string();
    let mut j = i + 1;
    while j < lines.len() && !lines[j].trim().starts_with("```") {
        j += 1;
    }
    if j >= lines.len() {
        return None;
    }
    let block = Block::Code {
        lang,
        lines: lines[i + 1..j].iter().map(|s| s.to_string()).collect(),
    };
    Some((block, j + 1 - i))
}

fn is_table_start(lines: &[&str], i: usize) -> bool {
    let Some(sep) = lines.get(i + 1) else {
        return false;
    };
    let cells = split_row(sep);
    !cells.is_empty() && cells.iter().all(|c| is_sep_cell(c))
}

fn is_sep_cell(cell: &str) -> bool {
    let c = cell.trim();
    let c = c.strip_prefix(':').unwrap_or(c);
    let c = c.strip_suffix(':').unwrap_or(c);
    !c.is_empty() && c.chars().all(|ch| ch == '-')
}

fn take_table(lines: &[&str], i: usize, blocks: &mut Vec<Block>) -> usize {
    let header_cells = split_row(lines[i]);
    let aligns: Vec<Align> = split_row(lines[i + 1])
        .iter()
        .map(|c| {
            let c = c.trim();
            match (c.starts_with(':'), c.ends_with(':')) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            }
        })
        .collect();

    let ncols = header_cells.len();
    let header: Vec<Vec<Span>> = header_cells.iter().map(|c| parse_spans(c.trim())).collect();
    let mut rows: Vec<Vec<Vec<Span>>> = Vec::new();
    let mut j = i + 2;
    while j < lines.len() {
        let t = lines[j].trim();
        if t.is_empty() || !t.starts_with('|') {
            break;
        }
        let mut cells: Vec<Vec<Span>> = split_row(lines[j])
            .iter()
            .map(|c| parse_spans(c.trim()))
            .collect();
        cells.resize(ncols, Vec::new());
        cells.truncate(ncols);
        rows.push(cells);
        j += 1;
    }
    blocks.push(Block::Table {
        header,
        rows,
        aligns: (0..ncols)
            .map(|k| aligns.get(k).copied().unwrap_or(Align::Left))
            .collect(),
    });
    j - i
}

/// 按未转义的 `|` 切分；`\|` 还原成 `|`。
fn split_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut chars = t.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' if chars.peek() == Some(&'|') => {
                chars.next();
                cur.push('|');
            }
            '|' => out.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    out.push(cur);
    out
}

fn parse_heading(line: &str) -> Option<(u8, &str)> {
    let level = line.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = line[level..].strip_prefix(' ')?;
    Some((level as u8, rest.trim()))
}

fn list_marker(line: &str) -> Option<(bool, u64)> {
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(m)
            && !rest.trim().is_empty()
        {
            return Some((false, 1));
        }
    }
    let digits: String = line.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = &line[digits.len()..];
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    let rest = rest.strip_prefix(' ')?;
    if rest.trim().is_empty() {
        return None;
    }
    Some((true, digits.parse().unwrap_or(1)))
}

fn strip_list_marker(line: &str) -> &str {
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(m) {
            return rest;
        }
    }
    let digits: String = line.chars().take_while(|c| c.is_ascii_digit()).collect();
    let rest = &line[digits.len()..];
    let rest = rest
        .strip_prefix('.')
        .or_else(|| rest.strip_prefix(')'))
        .unwrap_or(rest);
    rest.strip_prefix(' ').unwrap_or(rest)
}

/// `![alt](data:image/png;base64,…)`；链接（http/file）不当图片。
fn parse_image_line(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("![")?;
    let close = rest.find("](")?;
    let alt = &rest[..close];
    let after = &rest[close + 2..];
    let end = after.rfind(')')?;
    let src = &after[..end];
    if !src.starts_with("data:image/") {
        return None;
    }
    Some((alt.to_string(), src.to_string()))
}

/// 解析行内 `code` 与 **加粗**；其余按文本。
pub fn parse_spans(text: &str) -> Vec<Span> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut i = 0usize;

    while i < chars.len() {
        if chars[i] == '`'
            && let Some(end) = find_char(&chars, i + 1, '`')
        {
            if !buf.is_empty() {
                out.push(Span::Text(std::mem::take(&mut buf)));
            }
            out.push(Span::Code(chars[i + 1..end].iter().collect()));
            i = end + 1;
            continue;
        }
        if chars[i] == '*'
            && i + 1 < chars.len()
            && chars[i + 1] == '*'
            && let Some(end) = find_double_star(&chars, i + 2)
        {
            if !buf.is_empty() {
                out.push(Span::Text(std::mem::take(&mut buf)));
            }
            out.push(Span::Bold(chars[i + 2..end].iter().collect()));
            i = end + 2;
            continue;
        }
        buf.push(chars[i]);
        i += 1;
    }
    if !buf.is_empty() {
        out.push(Span::Text(buf));
    }
    if out.is_empty() {
        out.push(Span::Text(String::new()));
    }
    out
}

fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|&k| chars[k] == target)
}

fn find_double_star(chars: &[char], from: usize) -> Option<usize> {
    let mut k = from;
    while k + 1 < chars.len() {
        if chars[k] == '*' && chars[k + 1] == '*' {
            return Some(k);
        }
        k += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text() {
        assert_eq!(
            parse("下列说法正确的是（ ）。"),
            vec![Block::Paragraph(vec![Span::Text(
                "下列说法正确的是（ ）。".into()
            )])]
        );
    }

    #[test]
    fn fenced_code_with_language() {
        let blocks = parse("有下列程序：\n```c\nint x = 1;\n```\n下列说法正确的是（ ）。");
        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[0], Block::Paragraph(_)));
        assert_eq!(
            blocks[1],
            Block::Code {
                lang: "c".into(),
                lines: vec!["int x = 1;".into()]
            }
        );
    }

    #[test]
    fn unclosed_fence_is_text() {
        let blocks = parse("题干\n```c\nint x = 1;");
        assert_eq!(blocks.len(), 1, "未闭合必须整体当文本");
        assert!(matches!(&blocks[0], Block::Paragraph(_)));
    }

    #[test]
    fn inline_code_and_bold() {
        let spans = parse_spans("这是 `int x` 与 **重点**。");
        assert_eq!(
            spans,
            vec![
                Span::Text("这是 ".into()),
                Span::Code("int x".into()),
                Span::Text(" 与 ".into()),
                Span::Bold("重点".into()),
                Span::Text("。".into()),
            ]
        );
    }

    #[test]
    fn unmatched_markers_stay_text() {
        assert_eq!(parse_spans("a ` b"), vec![Span::Text("a ` b".into())]);
        assert_eq!(parse_spans("a ** b"), vec![Span::Text("a ** b".into())]);
    }

    #[test]
    fn code_wins_over_bold_inside() {
        assert_eq!(parse_spans("`**x**`"), vec![Span::Code("**x**".into())]);
    }

    #[test]
    fn headings() {
        assert!(matches!(
            &parse("## 小节")[0],
            Block::Heading { level: 2, .. }
        ));
        assert!(matches!(&parse("#标题")[0], Block::Paragraph(_)));
    }

    #[test]
    fn unordered_and_ordered_lists() {
        let b = parse("- 甲\n- 乙");
        match &b[0] {
            Block::List { ordered, items, .. } => {
                assert!(!ordered);
                assert_eq!(items.len(), 2);
            }
            other => panic!("应为列表：{other:?}"),
        }
        let b = parse("1. 甲\n2. 乙");
        match &b[0] {
            Block::List {
                ordered,
                start,
                items,
            } => {
                assert!(ordered);
                assert_eq!(*start, 1);
                assert_eq!(items.len(), 2);
            }
            other => panic!("应为有序列表：{other:?}"),
        }
    }

    #[test]
    fn quote_block() {
        let b = parse("> 注意：\n> 只答一个");
        assert!(matches!(&b[0], Block::Quote(_)));
    }

    #[test]
    fn table_with_alignment() {
        let b = parse("| 变量 | 类型 | 说明 |\n|:--|:--:|--:|\n| x | int | 计数 |");
        match &b[0] {
            Block::Table {
                header,
                rows,
                aligns,
            } => {
                assert_eq!(header.len(), 3);
                assert_eq!(rows.len(), 1);
                assert_eq!(aligns, &vec![Align::Left, Align::Center, Align::Right]);
            }
            other => panic!("应为表格：{other:?}"),
        }
    }

    #[test]
    fn table_without_separator_is_text() {
        let b = parse("| 这不是表格\n| 只是文本");
        assert!(matches!(&b[0], Block::Paragraph(_)));
    }

    #[test]
    fn table_ragged_rows_are_padded_or_truncated() {
        let b = parse("| a | b |\n|---|---|\n| 1 |\n| 1 | 2 | 3 |");
        match &b[0] {
            Block::Table { rows, .. } => {
                assert_eq!(rows[0].len(), 2, "缺列要补");
                assert_eq!(rows[1].len(), 2, "多列要截断");
            }
            other => panic!("应为表格：{other:?}"),
        }
    }

    #[test]
    fn table_escaped_pipe() {
        let b = parse("| a | b |\n|---|---|\n| x \\| y | 2 |");
        match &b[0] {
            Block::Table { rows, .. } => {
                let cell = format!("{:?}", rows[0][0]);
                assert!(cell.contains("x | y"), "转义的竖线要还原：{cell}");
            }
            other => panic!("应为表格：{other:?}"),
        }
    }

    #[test]
    fn image_block() {
        let b = parse("![示意图](data:image/png;base64,AAAA)");
        match &b[0] {
            Block::Image { alt, src } => {
                assert_eq!(alt, "示意图");
                assert!(src.starts_with("data:image/png"));
            }
            other => panic!("应为图片：{other:?}"),
        }
    }

    #[test]
    fn http_link_is_not_an_image() {
        let b = parse("![x](https://example.com/a.png)");
        assert!(matches!(&b[0], Block::Paragraph(_)), "链接不支持，按文本");
    }

    #[test]
    fn crlf_is_normalised() {
        let b = parse("题干\r\n```\r\nfoo();\r\n```\r\n后续");
        assert_eq!(b.len(), 3);
    }

    #[test]
    fn empty_input() {
        assert!(parse("").is_empty());
        assert!(parse("   \n\n").is_empty());
    }
}

#[cfg(test)]
mod parse_edge_tests {
    use super::*;

    #[test]
    fn cached_parse_reuses_result() {
        let a = parse_cached("- 甲\n\n| 列 |\n|---|\n| 1 |");
        let b = parse_cached("- 甲\n\n| 列 |\n|---|\n| 1 |");
        assert!(std::rc::Rc::ptr_eq(&a, &b), "同样输入必须命中缓存");
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn list_then_table() {
        let b = parse("- 甲\n- 乙\n\n| 列1 | 列2 |\n|:--|--:|\n| a | b |");
        assert_eq!(b.len(), 2, "应为 列表 + 表格：{b:?}");
        assert!(matches!(b[0], Block::List { .. }));
        assert!(matches!(b[1], Block::Table { .. }));
    }
}
