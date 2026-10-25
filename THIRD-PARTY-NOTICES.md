# 第三方组件与许可声明

本项目（保密考试系统 / EncryptedExamSystem）自身代码以 **Apache License 2.0** 发布，
全文见 `LICENSE`。

发布包（`ExamAdmin-*.zip`）内含下列第三方组件，它们**各自遵循其原始许可**，
本项目的许可不改变、也不限制这些组件自身的权利。

## 1. PySide6 / Qt（管理端 GUI）

| 组件 | 版本 | 许可 |
|---|---|---|
| PySide6（Qt for Python） | 6.12.x | LGPL-3.0 |
| Qt 6（随 PySide6 分发的库） | 6.x | LGPL-3.0 |

- 许可全文：`licenses/LGPL-3.0.txt`（LGPL-3.0 以 GPL-3.0 为基础，全文见 `licenses/GPL-3.0.txt`）
- 上游：https://www.qt.io/ 、 https://doc.qt.io/qtforpython/

**LGPL 合规说明**：PySide6/Qt 以**动态库**形式放在 `_internal/PySide6/` 下，
未与我们的代码静态链接。你可以在安装后**替换**该目录下的库文件
（例如换成自己编译的 Qt 版本），程序会使用替换后的库；我们不对这些库的
替换、调试或逆向施加任何限制。

## 2. Python 运行时

| 组件 | 版本 | 许可 |
|---|---|---|
| CPython | 3.13.x | PSF License |

## 3. Rust 依赖（学生端与命令行工具）

以下 crate 以 MIT 和/或 Apache-2.0 授权，随二进制静态链接：

`egui` / `eframe` / `epaint`（MIT OR Apache-2.0）、`image`（MIT OR Apache-2.0）、
`base64`（MIT OR Apache-2.0）、`blake3`（CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception）、
`serde` / `serde_json`（MIT OR Apache-2.0）、`argon2`（MIT OR Apache-2.0）、
`aes-gcm`（Apache-2.0 OR MIT）、`hkdf` / `sha2` / `hmac`（MIT OR Apache-2.0）、
`rand` / `rand_chacha`（MIT OR Apache-2.0）、`clap`（MIT OR Apache-2.0）、
`anyhow` / `thiserror`（MIT OR Apache-2.0）、`hex` / `zeroize`（MIT OR Apache-2.0）、
`winit` / `glutin`（Apache-2.0）、`ab_glyph`（Apache-2.0）。

完整依赖树与版本见 `Cargo.lock`；各 crate 的许可原文在其发布页可查。

## 4. 字体

界面中文优先使用**系统已安装**的字体（如微软雅黑、黑体、DejaVu Sans），
发布包**不内嵌**任何字体文件。

---

如果你需要这些组件的完整许可原文或源码，请通过仓库 Issue 联系作者。
