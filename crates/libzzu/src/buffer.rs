use crate::runtime::discard;

/// 本库分配的字节缓冲区不透明句柄，内容为 UTF-8 JSON（不含结尾 NUL），须通过 `zzu_buffer_free` 释放。
pub struct ZzuBuffer(Vec<u8>);

impl ZzuBuffer {
    pub(crate) fn into_raw(bytes: Vec<u8>) -> *mut Self {
        Box::into_raw(Box::new(Self(bytes)))
    }
}

/// 返回缓冲区首字节地址（`buffer` 为 `NULL` 时返回 `NULL`），不保证以 NUL 结尾。
///
/// 返回的指针在调用 `zzu_buffer_free` 前有效；长度须通过 `zzu_buffer_len` 获取。
///
/// # Safety
///
/// `buffer` 必须为 `NULL`，或指向尚未释放的有效 `ZzuBuffer` 句柄。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_buffer_data(buffer: *const ZzuBuffer) -> *const u8 {
    // SAFETY: Non-null handles remain live until zzu_buffer_free.
    unsafe { buffer.as_ref() }.map_or(std::ptr::null(), |buffer| buffer.0.as_ptr())
}

/// 返回缓冲区字节长度（`buffer` 为 `NULL` 时返回 `0`）。
///
/// # Safety
///
/// `buffer` 必须为 `NULL`，或指向尚未释放的有效 `ZzuBuffer` 句柄。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_buffer_len(buffer: *const ZzuBuffer) -> usize {
    // SAFETY: Non-null handles remain live until zzu_buffer_free.
    unsafe { buffer.as_ref() }.map_or(0, |buffer| buffer.0.len())
}

/// 释放 `ZzuBuffer`（传入 `NULL` 时无操作），可在任意线程调用。
///
/// # Safety
///
/// `buffer` 必须为 `NULL`，或由本库返回且尚未释放的句柄；释放后不得再访问其数据指针。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_buffer_free(buffer: *mut ZzuBuffer) {
    // SAFETY: The caller returns this owned buffer exactly once.
    unsafe { discard(buffer) }
}
