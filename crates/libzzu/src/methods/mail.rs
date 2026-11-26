use std::ffi::{c_char, c_void};

use serde::Deserialize;
use zzu::mail::MailClient;

use super::{Guarded, ZzuCasClient, done, parse, reply};
use crate::{
    buffer::ZzuBuffer,
    client::{Client, Service, ZzuCall, ZzuCallback, call, call_blocking},
    error::{FfiError, FfiResult, ZzuStatus},
    runtime::{create, discard},
};

/// 校园邮箱客户端不透明句柄，线程安全，须通过 `zzu_mail_free` 释放。
pub struct ZzuMailClient(Client<Guarded<MailClient>>);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MessagesParams {
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    page_size: Option<u32>,
}

impl Service for Guarded<MailClient> {
    async fn call(&self, method: &str, params: &[u8]) -> FfiResult<Vec<u8>> {
        match method {
            "login" => {
                self.write().await?.login().await?;
                done()
            }
            "logout" => {
                self.write().await?.logout();
                done()
            }
            "is_logged_in" => reply(&self.read().await?.is_logged_in()),
            "webmail_url" => reply(&self.read().await?.webmail_url().await?),
            "unread_count" => reply(&self.read().await?.unread_count().await?),
            "messages" => {
                let MessagesParams { page, page_size } = parse(params)?;
                reply(
                    &self
                        .read()
                        .await?
                        .messages(page.unwrap_or(1), page_size)
                        .await?,
                )
            }
            _ => Err(FfiError::UnknownMethod(method.into())),
        }
    }
}

/// 使用 `ZzuCasClient` 创建校园邮箱客户端（本地完成，不发起网络请求）。
///
/// 新句柄与 `cas` 共享认证会话、运行时和时限；创建后释放 `cas` 不影响它。
///
/// # Safety
///
/// - `cas` 必须为 `NULL` 或指向尚未释放的有效句柄（`NULL` 时返回 `ZZU_INVALID_INPUT`）。
/// - `out_mail` 必须非空、对齐且独占可写；成功时写入的句柄须通过 `zzu_mail_free` 释放。
/// - `out_error` 可以为 `NULL`；若非空则必须对齐且独占可写，失败时写入的错误 JSON 须通过 `zzu_buffer_free` 释放。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_mail_new(
    cas: *const ZzuCasClient,
    out_mail: *mut *mut ZzuMailClient,
    out_error: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented input and output pointer contracts.
    unsafe {
        create(out_mail, out_error, || {
            let cas = cas
                .as_ref()
                .ok_or_else(|| FfiError::InvalidInput("CAS 句柄为空".into()))?;
            let http = cas.0.trust_roots().client()?;
            let mail = MailClient::with_http_client(cas.0.service().clone(), http);
            Ok(ZzuMailClient(cas.0.sibling(Guarded::new(mail))))
        })
    }
}

/// 释放校园邮箱客户端句柄（传入 `NULL` 时无操作），不会阻塞，也可以在回调中调用。
///
/// 在途调用不受影响，其回调仍会照常触发。
///
/// # Safety
///
/// `mail` 必须为 `NULL`，或由 `zzu_mail_new` 返回且尚未释放的句柄；释放后不得再使用。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_mail_free(mail: *mut ZzuMailClient) {
    // SAFETY: The caller returns this owned handle exactly once.
    unsafe { discard(mail) }
}

/// 异步调用校园邮箱方法（如 `"unread_count"`），调用与回调规则同 `zzu_cas_call`。
///
/// # Safety
///
/// `mail` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call`。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_mail_call(
    mail: *const ZzuMailClient,
    method: *const c_char,
    params: *const u8,
    params_len: usize,
    callback: ZzuCallback,
    user_data: *mut c_void,
    out_call: *mut *mut ZzuCall,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented pointer contracts.
    unsafe {
        call(
            mail.as_ref().map(|mail| &mail.0),
            method,
            params,
            params_len,
            callback,
            user_data,
            out_call,
        )
    }
}

/// 同步调用校园邮箱方法，规则同 `zzu_cas_call_blocking`。
///
/// # Safety
///
/// `mail` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call_blocking`。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_mail_call_blocking(
    mail: *const ZzuMailClient,
    method: *const c_char,
    params: *const u8,
    params_len: usize,
    out_payload: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented pointer contracts.
    unsafe {
        call_blocking(
            mail.as_ref().map(|mail| &mail.0),
            method,
            params,
            params_len,
            out_payload,
        )
    }
}
