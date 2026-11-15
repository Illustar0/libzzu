//! `libzzu` 提供的校园服务 C ABI 导出层。
//!
//! 每种 `zzu` 客户端对应独立的不透明句柄及 `new`、`free`、`call`、`call_blocking` 四个导出函数：
//! 客户端方法经 `call` 以“方法名 + UTF-8 JSON”分发，不依赖客户端状态的本地纯计算函数单独导出。
//! 函数签名仅使用定宽整数、`size_t`、指针与单一回调类型 `ZzuCallback`，无结构体跨越边界，
//! 因此新增方法或字段均保持二进制兼容。
//!
//! 完整契约、方法目录与跨语言封装指引见 `crates/libzzu/ABI.md`。

#![deny(improper_ctypes_definitions)]

mod buffer;
mod client;
mod error;
mod methods;
mod runtime;

use std::ffi::c_char;

pub use buffer::*;
pub use client::*;
pub use error::*;
pub use methods::*;
pub use runtime::*;

/// C ABI 主版本号：出现破坏性变更时递增。
pub const ZZU_ABI_VERSION_MAJOR: u32 = 1;
/// C ABI 次版本号：新增方法或字段时递增，主版本号递增时归零。
pub const ZZU_ABI_VERSION_MINOR: u32 = 0;

/// 返回链接库实现的 C ABI 版本，编码为 `(major << 16) | minor`。
///
/// 封装层应在加载时校验：主版本号与头文件相等，且次版本号不小于所需版本。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub extern "C" fn zzu_abi_version() -> u32 {
    (ZZU_ABI_VERSION_MAJOR << 16) | ZZU_ABI_VERSION_MINOR
}

/// 返回链接库的软件版本号，为静态存储的 `NUL` 结尾字符串，不得释放。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub extern "C" fn zzu_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}
