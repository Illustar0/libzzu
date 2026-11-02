use std::{
    ffi::{c_char, c_void},
    net::IpAddr,
    num::NonZeroU64,
    ptr,
};

use serde::{Deserialize, Serialize};
use zzu_core::{
    TrustRoots,
    network::{self, AuthResult, PortalClient, PortalEncryption, SelfServiceClient},
};

use super::{Guarded, done, parse, reply};
use crate::{
    buffer::ZzuBuffer,
    client::{Client, Service, ZzuCall, ZzuCallback, call, call_blocking, compute, time_limit},
    error::{FfiError, FfiResult, ZzuStatus},
    runtime::{ZzuRuntime, bytes, create, discard},
};

/// 校园网 Portal 认证客户端不透明句柄，线程安全，须通过 `zzu_portal_free` 释放。
pub struct ZzuPortalClient(Client<PortalClient>);

/// 校园网自助服务客户端不透明句柄，线程安全，须通过 `zzu_self_service_free` 释放。
pub struct ZzuSelfServiceClient(Client<Guarded<SelfServiceClient>>);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortalConfig {
    base_url: String,
    user_ip: IpAddr,
    timeout_ms: Option<NonZeroU64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SelfServiceConfig {
    base_url: String,
    timeout_ms: Option<NonZeroU64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DiscoverOptions {
    timeout_ms: Option<NonZeroU64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PortalLogin {
    account: String,
    password: String,
    #[serde(default)]
    encryption: PortalEncryption,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PortalAuth {
    is_success: bool,
    result: i64,
    message: String,
    ret_code: Option<i64>,
}

impl From<AuthResult> for PortalAuth {
    fn from(auth: AuthResult) -> Self {
        Self {
            is_success: auth.is_success(),
            result: auth.result,
            message: auth.message,
            ret_code: auth.ret_code,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SelfServiceLogin {
    account: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KickDevice {
    session_id: String,
}

impl Service for PortalClient {
    async fn call(&self, method: &str, params: &[u8]) -> FfiResult<Vec<u8>> {
        match method {
            "login" => {
                let PortalLogin {
                    account,
                    password,
                    encryption,
                } = parse(params)?;
                let auth = self.login(&account, &password, encryption).await?;
                reply(&PortalAuth::from(auth))
            }
            _ => Err(FfiError::UnknownMethod(method.into())),
        }
    }
}

/// Portal 探测发生在 `ZzuPortalClient` 创建之前，所以是 core 中的自由函数，这里单独成服务。
struct PortalDiscovery(TrustRoots);

impl Service for PortalDiscovery {
    async fn call(&self, _method: &str, _params: &[u8]) -> FfiResult<Vec<u8>> {
        reply(&network::discover_portal_info_with_trust_roots(&self.0).await?)
    }
}

impl PortalDiscovery {
    unsafe fn client(
        runtime: *const ZzuRuntime,
        options: *const u8,
        options_len: usize,
    ) -> FfiResult<Client<Self>> {
        // SAFETY: A non-null runtime handle remains live for the duration of the call.
        let runtime = unsafe { runtime.as_ref() }
            .ok_or_else(|| FfiError::InvalidInput("运行时句柄为空".into()))?;
        // SAFETY: The caller guarantees options_len readable bytes during the call.
        let options: DiscoverOptions = parse(unsafe { bytes(options, options_len) }?)?;
        Ok(Client::new(
            runtime.worker.clone(),
            time_limit(options.timeout_ms),
            Self(runtime.worker.trust_roots.clone()),
        ))
    }
}

impl Service for Guarded<SelfServiceClient> {
    async fn call(&self, method: &str, params: &[u8]) -> FfiResult<Vec<u8>> {
        match method {
            "login" => {
                let SelfServiceLogin { account, password } = parse(params)?;
                self.write().await?.login(&account, &password).await?;
                done()
            }
            "is_logged_in" => reply(&self.read().await?.is_logged_in()),
            "online_devices" => reply(&self.read().await?.online_devices().await?),
            "kick_device" => {
                let KickDevice { session_id } = parse(params)?;
                self.read().await?.kick_device(&session_id).await?;
                done()
            }
            "logout" => {
                self.write().await?.logout().await?;
                done()
            }
            _ => Err(FfiError::UnknownMethod(method.into())),
        }
    }
}

/// 异步探测当前网络下的校园网 Portal 地址与本机内网 IP，结果可用于 `zzu_portal_new`。
///
/// `options` 为 UTF-8 JSON 对象，可选字段 `timeoutMs`（总时限，单位毫秒，默认 `15000`）；`options_len` 为 `0` 时使用默认配置（此时 `options` 可传 `NULL`）。
/// `options` 在提交前同步校验，若 JSON 非法则直接返回 `ZZU_INVALID_INPUT` 且不触发回调；其余调用与回调规则同 `zzu_cas_call`。
///
/// # Safety
///
/// - `runtime` 必须为 `NULL` 或指向尚未释放的有效句柄。
/// - `options` 在 `options_len` 非零时必须指向调用期间可读的 `options_len` 字节。
/// - `user_data` 与 `out_call` 的要求同 `zzu_cas_call`。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_portal_discover(
    runtime: *const ZzuRuntime,
    options: *const u8,
    options_len: usize,
    callback: ZzuCallback,
    user_data: *mut c_void,
    out_call: *mut *mut ZzuCall,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented pointer contracts.
    unsafe {
        let discovery = PortalDiscovery::client(runtime, options, options_len).ok();
        call(
            discovery.as_ref(),
            c"discover".as_ptr(),
            ptr::null(),
            0,
            callback,
            user_data,
            out_call,
        )
    }
}

/// 同步探测校园网 Portal，`options` 同 `zzu_portal_discover`，其余规则同 `zzu_cas_call_blocking`。
///
/// # Safety
///
/// - `runtime` 必须为 `NULL` 或指向尚未释放的有效句柄。
/// - `options` 在 `options_len` 非零时必须指向调用期间可读的 `options_len` 字节。
/// - `out_payload` 的要求同 `zzu_cas_call_blocking`。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_portal_discover_blocking(
    runtime: *const ZzuRuntime,
    options: *const u8,
    options_len: usize,
    out_payload: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented pointer contracts.
    unsafe {
        match PortalDiscovery::client(runtime, options, options_len) {
            Ok(discovery) => call_blocking(
                Some(&discovery),
                c"discover".as_ptr(),
                ptr::null(),
                0,
                out_payload,
            ),
            Err(error) => compute(out_payload, || Err(error)),
        }
    }
}

/// 创建校园网 Portal 认证客户端（本地完成，不发起网络请求）。
///
/// `config` 为 UTF-8 JSON 对象：`baseUrl`、`userIp` 必填，`timeoutMs` 可选（默认 `15000` 毫秒）。
///
/// # Safety
///
/// - `runtime` 必须为 `NULL` 或指向尚未释放的有效句柄（`NULL` 时返回 `ZZU_INVALID_INPUT`）。
/// - `config` 在 `config_len` 非零时必须指向调用期间可读的 `config_len` 字节。
/// - `out_portal` 必须非空、对齐且独占可写；成功时写入的句柄须通过 `zzu_portal_free` 释放。
/// - `out_error` 可以为 `NULL`；若非空则必须对齐且独占可写，失败时写入的错误 JSON 须通过 `zzu_buffer_free` 释放。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_portal_new(
    runtime: *const ZzuRuntime,
    config: *const u8,
    config_len: usize,
    out_portal: *mut *mut ZzuPortalClient,
    out_error: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented input and output pointer contracts.
    unsafe {
        create(out_portal, out_error, || {
            let runtime = runtime
                .as_ref()
                .ok_or_else(|| FfiError::InvalidInput("运行时句柄为空".into()))?;
            let config: PortalConfig = parse(bytes(config, config_len)?)?;
            Ok(ZzuPortalClient(Client::new(
                runtime.worker.clone(),
                time_limit(config.timeout_ms),
                PortalClient::with_trust_roots(
                    &config.base_url,
                    config.user_ip,
                    &runtime.worker.trust_roots,
                )?,
            )))
        })
    }
}

/// 释放 Portal 客户端句柄（传入 `NULL` 时无操作），不会阻塞，也可以在回调中调用。
///
/// 在途调用不受影响，其回调仍会照常触发。
///
/// # Safety
///
/// `portal` 必须为 `NULL`，或由 `zzu_portal_new` 返回且尚未释放的句柄；释放后不得再使用。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_portal_free(portal: *mut ZzuPortalClient) {
    // SAFETY: The caller returns this owned handle exactly once.
    unsafe { discard(portal) }
}

/// 异步调用 Portal 方法（如 `"login"`），调用与回调规则同 `zzu_cas_call`。
///
/// # Safety
///
/// `portal` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call`。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_portal_call(
    portal: *const ZzuPortalClient,
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
            portal.as_ref().map(|portal| &portal.0),
            method,
            params,
            params_len,
            callback,
            user_data,
            out_call,
        )
    }
}

/// 同步调用 Portal 方法，规则同 `zzu_cas_call_blocking`。
///
/// # Safety
///
/// `portal` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call_blocking`。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_portal_call_blocking(
    portal: *const ZzuPortalClient,
    method: *const c_char,
    params: *const u8,
    params_len: usize,
    out_payload: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented pointer contracts.
    unsafe {
        call_blocking(
            portal.as_ref().map(|portal| &portal.0),
            method,
            params,
            params_len,
            out_payload,
        )
    }
}

/// 创建校园网自助服务客户端（本地完成，不发起网络请求）。
///
/// `config` 为 UTF-8 JSON 对象：`baseUrl` 必填，`timeoutMs` 可选（默认 `15000` 毫秒）。
///
/// # Safety
///
/// - `runtime` 必须为 `NULL` 或指向尚未释放的有效句柄（`NULL` 时返回 `ZZU_INVALID_INPUT`）。
/// - `config` 在 `config_len` 非零时必须指向调用期间可读的 `config_len` 字节。
/// - `out_service` 必须非空、对齐且独占可写；成功时写入的句柄须通过 `zzu_self_service_free` 释放。
/// - `out_error` 可以为 `NULL`；若非空则必须对齐且独占可写，失败时写入的错误 JSON 须通过 `zzu_buffer_free` 释放。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_self_service_new(
    runtime: *const ZzuRuntime,
    config: *const u8,
    config_len: usize,
    out_service: *mut *mut ZzuSelfServiceClient,
    out_error: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented input and output pointer contracts.
    unsafe {
        create(out_service, out_error, || {
            let runtime = runtime
                .as_ref()
                .ok_or_else(|| FfiError::InvalidInput("运行时句柄为空".into()))?;
            let config: SelfServiceConfig = parse(bytes(config, config_len)?)?;
            Ok(ZzuSelfServiceClient(Client::new(
                runtime.worker.clone(),
                time_limit(config.timeout_ms),
                Guarded::new(SelfServiceClient::with_trust_roots(
                    &config.base_url,
                    &runtime.worker.trust_roots,
                )?),
            )))
        })
    }
}

/// 释放自助服务客户端句柄（传入 `NULL` 时无操作），不会阻塞，也可以在回调中调用。
///
/// 在途调用不受影响，其回调仍会照常触发。
///
/// # Safety
///
/// `service` 必须为 `NULL`，或由 `zzu_self_service_new` 返回且尚未释放的句柄；释放后不得再使用。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_self_service_free(service: *mut ZzuSelfServiceClient) {
    // SAFETY: The caller returns this owned handle exactly once.
    unsafe { discard(service) }
}

/// 异步调用自助服务方法（如 `"online_devices"`），调用与回调规则同 `zzu_cas_call`。
///
/// # Safety
///
/// `service` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call`。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_self_service_call(
    service: *const ZzuSelfServiceClient,
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
            service.as_ref().map(|service| &service.0),
            method,
            params,
            params_len,
            callback,
            user_data,
            out_call,
        )
    }
}

/// 同步调用自助服务方法，规则同 `zzu_cas_call_blocking`。
///
/// # Safety
///
/// `service` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call_blocking`。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_self_service_call_blocking(
    service: *const ZzuSelfServiceClient,
    method: *const c_char,
    params: *const u8,
    params_len: usize,
    out_payload: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented pointer contracts.
    unsafe {
        call_blocking(
            service.as_ref().map(|service| &service.0),
            method,
            params,
            params_len,
            out_payload,
        )
    }
}
