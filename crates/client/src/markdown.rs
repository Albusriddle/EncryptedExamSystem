//! 题干里的 Markdown 围栏代码块解析。
//!
//! ## 为什么要单独做
//!
//! 真实题库的题干里常见这种写法：
//!
//! ```text
//! 有下列程序：
//! ```c
//! int x = 1;
//! ```
//! 下列说法正确的是（ ）。
//! ```
//!
//! 学生端原来把题干整段当纯文本画，围栏和语言标记会原样显示出来（实测反馈）。
//! 这里只做**最小解析**：识别 ``` 围栏，其余一律按文本处理。
//!
//! 支持的边界（都有测试）：
//! - 可带语言标记（```c）也可不带（```）；
//! - Windows 的 CRLF 换行；
//! - **未闭合**的围栏按普通文本处理——一道坏题不能把后面的内容全吞掉。

/// 题干切分后的块。
#[derive(Debug, PartialEq, Eq)]
pub enum Block {
    /// 普通文本（可能含换行）
    Text(String),
    /// 围栏代码块
    Code { lang: String, lines: Vec<String> },
}

/// 把题干按围栏代码块切分。
pub fn split_blocks(input: &str) -> Vec<Block> {
    let text = input.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.split('\n').collect();

    let mut blocks: Vec<Block> = Vec::new();
    let mut buf: Vec<String> = Vec::new();
    let mut i = 0usize;

    while i < lines.len() {
        let line = lines[i];
        if line.trim_start().starts_with("```") {
            // 往后找闭合围栏；找不到就按普通文本处理（不能吞掉后面的题干）
            let mut j = i + 1;
            while j < lines.len() && !lines[j].trim_start().starts_with("```") {
                j += 1;
            }
            if j < lines.len() {
                flush_text(&mut blocks, &mut buf);
                let lang = line.trim_start().trim_start_matches('`').trim().to_string();
                blocks.push(Block::Code {
                    lang,
                    lines: lines[i + 1..j].iter().map(|s| s.to_string()).collect(),
                });
                i = j + 1;
                continue;
            }
        }
        buf.push(line.to_string());
        i += 1;
    }
    flush_text(&mut blocks, &mut buf);
    blocks
}

fn flush_text(blocks: &mut Vec<Block>, buf: &mut Vec<String>) {
    if buf.is_empty() {
        return;
    }
    let joined = buf.join("\n");
    buf.clear();
    if !joined.trim().is_empty() {
        blocks.push(Block::Text(joined));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_one_block() {
        assert_eq!(
            split_blocks("下列说法正确的是（ ）。"),
            vec![Block::Text("下列说法正确的是（ ）。".into())]
        );
    }

    #[test]
    fn fenced_code_with_language() {
        let src = "有下列程序：\n```c\nint x = 1;\n```\n下列说法正确的是（ ）。";
        let blocks = split_blocks(src);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], Block::Text("有下列程序：".into()));
        assert_eq!(
            blocks[1],
            Block::Code {
                lang: "c".into(),
                lines: vec!["int x = 1;".into()]
            }
        );
        assert_eq!(blocks[2], Block::Text("下列说法正确的是（ ）。".into()));
    }

    #[test]
    fn crlf_is_normalised() {
        let src = "题干\r\n```\r\nfoo();\r\n```\r\n后续";
        let blocks = split_blocks(src);
        assert_eq!(blocks.len(), 3);
        assert!(
            matches!(&blocks[1], Block::Code { lines, .. } if lines == &vec!["foo();".to_string()])
        );
    }

    #[test]
    fn unclosed_fence_is_text() {
        let src = "题干\n```c\nint x = 1;\n后面没有闭合";
        let blocks = split_blocks(src);
        assert_eq!(blocks.len(), 1, "未闭合必须整体当文本，不能吞掉内容");
        assert!(matches!(&blocks[0], Block::Text(t) if t.contains("int x = 1;")));
    }

    #[test]
    fn multiple_blocks() {
        let src = "A\n```\n1\n```\nB\n```py\n2\n```\nC";
        let blocks = split_blocks(src);
        assert_eq!(blocks.len(), 5);
        assert!(matches!(&blocks[1], Block::Code { lang, .. } if lang.is_empty()));
        assert!(matches!(&blocks[3], Block::Code { lang, .. } if lang == "py"));
    }

    #[test]
    fn empty_input() {
        assert!(split_blocks("").is_empty());
        assert!(split_blocks("   \n\n").is_empty());
    }
}
