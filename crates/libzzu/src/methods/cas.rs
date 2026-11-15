use std::{
    ffi::{c_char, c_void},
    num::NonZeroU64,
};

use serde::Deserialize;
use zzu::auth::{CasClient, Service, Tokens};

use super::{done, parse, reply};
use crate::{
    buffer::ZzuBuffer,
    client::{
        Client, Service as ClientService, ZzuCall, ZzuCallback, call, call_blocking, time_limit,
    },
    error::{FfiError, FfiResult, ZzuStatus},
    runtime::{ZzuRuntime, bytes, create, discard},
};

/// 统一身份认证（CAS）客户端不透明句柄，线程安全，须通过 `zzu_cas_free` 释放。
pub struct ZzuCasClient(pub(super) Client<CasClient>);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CasConfig {
    account: String,
    password: String,
    timeout_ms: Option<NonZeroU64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetDevice {
    device_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifySms {
    code: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceLogin {
    service: Service,
}

impl ClientService for CasClient {
    async fn call(&self, method: &str, params: &[u8]) -> FfiResult<Vec<u8>> {
        match method {
            "login" => {
                self.login().await?;
                done()
            }
            "reauthenticate" => {
                self.reauthenticate().await?;
                done()
            }
            "logout" => {
                self.logout().await;
                done()
            }
            "is_logged_in" => reply(&self.is_logged_in().await),
            "set_tokens" => {
                self.set_tokens(parse::<Tokens>(params)?).await?;
                done()
            }
            "tokens" => reply(&self.tokens().await),
            "user_token" => reply(&self.user_token().await?),
            "set_device" => {
                let SetDevice { device_id } = parse(params)?;
                self.set_device(device_id).await?;
                done()
            }
            "detect_mfa" => reply(&self.detect_mfa().await?),
            "send_sms" => {
                self.send_sms().await?;
                done()
            }
            "verify_sms" => {
                let VerifySms { code } = parse(params)?;
                self.verify_sms(&code).await?;
                done()
            }
            "user_info" => reply(&self.user_info().await?),
            "service_login_url" => {
                let ServiceLogin { service } = parse(params)?;
                reply(&self.service_login_url(service).await?)
            }
            _ => Err(FfiError::UnknownMethod(method.into())),
        }
    }
}

/// 创建统一身份认证客户端（本地完成，不发起网络请求）。
///
/// `config` 为 UTF-8 JSON 对象：`account`、`password` 必填，`timeoutMs` 可选（单次操作总时限，单位毫秒，默认 `15000`）。
///
/// # Safety
///
/// - `runtime` 必须为 `NULL` 或指向尚未释放的有效句柄（`NULL` 时返回 `ZZU_INVALID_INPUT`）。
/// - `config` 在 `config_len` 非零时必须指向调用期间可读的 `config_len` 字节。
/// - `out_cas` 必须非空、对齐且独占可写；成功时写入的句柄须通过 `zzu_cas_free` 释放。
/// - `out_error` 可以为 `NULL`；若非空则必须对齐且独占可写，失败时写入的错误 JSON 须通过 `zzu_buffer_free` 释放。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_cas_new(
    runtime: *const ZzuRuntime,
    config: *const u8,
    config_len: usize,
    out_cas: *mut *mut ZzuCasClient,
    out_error: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented input and output pointer contracts.
    unsafe {
        create(out_cas, out_error, || {
            let runtime = runtime
                .as_ref()
                .ok_or_else(|| FfiError::InvalidInput("运行时句柄为空".into()))?;
            let config: CasConfig = parse(bytes(config, config_len)?)?;
            let http = runtime.worker.trust_roots.client()?;
            Ok(ZzuCasClient(Client::new(
                runtime.worker.clone(),
                time_limit(config.timeout_ms),
                CasClient::with_http_client(config.account, config.password, http),
            )))
        })
    }
}

/// 释放 CAS 客户端句柄（传入 `NULL` 时无操作），不会阻塞，也可以在回调中调用。
///
/// 已由它创建的 `ZzuEasClient` 与 `ZzuEcardClient` 不受影响，在途调用的回调仍会照常触发。
///
/// # Safety
///
/// `cas` 必须为 `NULL`，或由 `zzu_cas_new` 返回且尚未释放的句柄；释放后不得再使用。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_cas_free(cas: *mut ZzuCasClient) {
    // SAFETY: The caller returns this owned handle exactly once.
    unsafe { discard(cas) }
}

/// 异步调用 CAS 客户端方法；立即返回，绝不在当前调用栈内同步触发回调。
///
/// 返回 `ZZU_OK` 时 `callback` 必定在后台工作线程上恰好触发一次（可能早于本函数返回，但 `*out_call` 保证已在触发前写入）；
/// 返回非零状态码时回调绝不触发。同步返回值仅校验句柄、方法名、回调与输入切片指针；`params` JSON 解析及后续执行错误一律经回调上报。
///
/// `method` 为 NUL 结尾的方法名（如 `"login"`）；`params` 为 UTF-8 JSON 对象，`params_len` 为 `0` 表示无参数（此时 `params` 可传 `NULL`）。
///
/// # Safety
///
/// - `cas` 必须为 `NULL` 或指向尚未释放的有效句柄。
/// - `method` 必须为 `NULL` 或调用期间可读的 NUL 结尾字符串。
/// - `params` 在 `params_len` 非零时必须指向调用期间可读的 `params_len` 字节；本函数返回后不再访问。
/// - `user_data` 由本库原样传回，必须可在任意线程使用，且在回调触发前保持有效。
/// - `out_call` 可以为 `NULL`（无需取消时）；若非空则必须对齐且独占可写，写入的句柄须通过 `zzu_call_free` 释放。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_cas_call(
    cas: *const ZzuCasClient,
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
            cas.as_ref().map(|cas| &cas.0),
            method,
            params,
            params_len,
            callback,
            user_data,
            out_call,
        )
    }
}

/// 同步调用 CAS 客户端方法，阻塞当前线程直至操作完成或达到 `timeoutMs` 时限。
///
/// 供无法接收跨线程回调的语言或场景使用，不支持中途取消。严禁在 `ZzuCallback` 回调线程中调用
/// （直接返回 `ZZU_INVALID_INPUT`，避免阻塞后台工作线程造成死锁）。
///
/// 返回 `ZZU_OK` 时 `*out_payload` 为结果 JSON，否则为错误 JSON。
///
/// # Safety
///
/// - `cas`、`method`、`params` 的要求同 `zzu_cas_call`。
/// - `out_payload` 必须非空、对齐且独占可写（`NULL` 时返回 `ZZU_INVALID_INPUT`）；写入的缓冲区须通过 `zzu_buffer_free` 释放。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_cas_call_blocking(
    cas: *const ZzuCasClient,
    method: *const c_char,
    params: *const u8,
    params_len: usize,
    out_payload: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented pointer contracts.
    unsafe {
        call_blocking(
            cas.as_ref().map(|cas| &cas.0),
            method,
            params,
            params_len,
            out_payload,
        )
    }
}
