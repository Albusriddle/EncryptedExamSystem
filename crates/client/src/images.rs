//! 图片解码与纹理缓存。
//!
//! ## 为什么只认 data URI
//!
//! 考试包必须**离线自包含**：拷一个 exe 到考场就能考。所以图片以
//! `data:image/png;base64,…` 的形式内嵌在题干里，不读任何外部文件，
//! 也就不存在"路径穿越 / 临时文件残留"这类安全问题。
//!
//! ## 上限
//!
//! - 单张解码后 ≤ 4MB：防止一个超大 data URI 把内存打满；
//! - 解码后像素 ≤ 4096×4096：防止解压炸弹；
//! - 纹理缓存 32 张：翻回前面的题时不重复解码。

use std::cell::RefCell;
use std::collections::VecDeque;

use base64::Engine as _;
use eframe::egui;

/// 单张图片解码后的字节上限（base64 前）。
const MAX_BYTES: usize = 4 * 1024 * 1024;
/// 单边像素上限。
const MAX_PIXELS: u32 = 4096;
/// 纹理缓存条数。
const CACHE_LIMIT: usize = 32;

thread_local! {
    static CACHE: RefCell<VecDeque<(u64, egui::TextureHandle)>> = const { RefCell::new(VecDeque::new()) };
}

/// 取图片纹理：命中缓存直接返回，否则解码并上传。
pub fn texture_for_uri(ctx: &egui::Context, uri: &str) -> Option<egui::TextureHandle> {
    let bytes = decode_data_uri(uri)?;
    let key = hash_bytes(&bytes);

    if let Some(hit) = CACHE.with(|c| {
        c.borrow()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, t)| t.clone())
    }) {
        return Some(hit);
    }

    let tex = decode_texture(ctx, &bytes, key)?;
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        c.push_back((key, tex.clone()));
        while c.len() > CACHE_LIMIT {
            c.pop_front();
        }
    });
    Some(tex)
}

fn decode_texture(ctx: &egui::Context, bytes: &[u8], key: u64) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?;
    if img.width() == 0 || img.height() == 0 {
        return None;
    }
    if img.width() > MAX_PIXELS || img.height() > MAX_PIXELS {
        return None;
    }
    let rgba = img.to_rgba8();
    let color = egui::ColorImage::from_rgba_unmultiplied(
        [img.width() as usize, img.height() as usize],
        rgba.as_raw(),
    );
    Some(ctx.load_texture(format!("img-{key:x}"), color, egui::TextureOptions::LINEAR))
}

/// 解析 `data:image/png;base64,…`；其它形式（http/文件路径）一律拒绝。
pub fn decode_data_uri(uri: &str) -> Option<Vec<u8>> {
    let rest = uri.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if !meta.starts_with("image/") || !meta.contains(";base64") {
        return None;
    }
    // base64 体积约为原始数据的 4/3，先按字符串长度挡一道
    if data.len() > MAX_BYTES / 3 * 4 + 8 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .ok()?;
    if bytes.len() > MAX_BYTES {
        return None;
    }
    Some(bytes)
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1×1 透明 PNG。
    const PNG_1PX: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

    #[test]
    fn decodes_png_data_uri() {
        let bytes = decode_data_uri(PNG_1PX).expect("应能解码");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn rejects_http_and_plain_paths() {
        assert!(decode_data_uri("https://example.com/a.png").is_none());
        assert!(decode_data_uri("images/a.png").is_none());
        assert!(decode_data_uri("data:text/plain;base64,AAAA").is_none());
    }

    #[test]
    fn rejects_broken_base64() {
        assert!(decode_data_uri("data:image/png;base64,@@@@").is_none());
    }

    #[test]
    fn decodes_to_texture() {
        let ctx = egui::Context::default();
        let tex = texture_for_uri(&ctx, PNG_1PX).expect("应能生成纹理");
        assert_eq!(tex.size(), [1, 1]);
    }

    #[test]
    fn broken_image_returns_none_instead_of_panic() {
        let ctx = egui::Context::default();
        // 合法的 base64，但不是图片
        let uri = "data:image/png;base64,AAAA";
        assert!(texture_for_uri(&ctx, uri).is_none());
    }
}
