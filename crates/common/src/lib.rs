//! 保密考试系统共享核心库：加密、密钥派生、确定性抽题、包格式、时间门禁。
//!
//! 被 builder（打包端）和 client（学生端）共同依赖，保证两端算法完全一致。

pub mod container;
pub mod crypto;
pub mod model;
pub mod picker;
pub mod timegate;

pub use model::*;
