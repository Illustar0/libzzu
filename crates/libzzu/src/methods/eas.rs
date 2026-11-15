use std::ffi::{c_char, c_void};

use jiff::civil::Date;
use serde::Deserialize;
use zzu::eas::{TeachingWeeks as Timetable, UndergradEasClient};

use super::{Guarded, ZzuCasClient, done, parse, reply};
use crate::{
    buffer::ZzuBuffer,
    client::{Client, Service, ZzuCall, ZzuCallback, call, call_blocking, compute},
    error::{FfiError, FfiResult, ZzuStatus},
    runtime::{bytes, create, discard},
};

/// 本科教务客户端不透明句柄，线程安全，须通过 `zzu_eas_free` 释放。
pub struct ZzuEasClient(Client<Guarded<UndergradEasClient>>);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TeachingWeeks {
    semester_id: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TeachingWeek {
    week: u32,
    semester_id: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WeekIndex {
    date: Date,
}

impl Service for Guarded<UndergradEasClient> {
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
            "current_semester" => reply(self.read().await?.current_semester()?),
            "semesters" => reply(&self.read().await?.semesters().await?),
            "teaching_weeks" => {
                let TeachingWeeks { semester_id } = parse(params)?;
                reply(&self.read().await?.teaching_weeks(semester_id).await?)
            }
            "teaching_week" => {
                let TeachingWeek { week, semester_id } = parse(params)?;
                reply(&self.read().await?.teaching_week(week, semester_id).await?)
            }
            "week_index" => {
                let WeekIndex { date } = parse(params)?;
                reply(&self.read().await?.week_index(date).await?)
            }
            _ => Err(FfiError::UnknownMethod(method.into())),
        }
    }
}

/// 将学期课表渲染为 `iCalendar`（`.ics`）文本（当前线程同步完成，不发起网络请求，无需客户端）。
///
/// `weeks` 为 `teaching_weeks` 返回的 UTF-8 JSON 数组；仅导出一周时传入包含 `teaching_week` 结果的单元素数组。
/// 返回 `ZZU_OK` 时 `*out_payload` 为 JSON 字符串形式的 `.ics` 文本，否则为错误 JSON。
///
/// # Safety
///
/// - `weeks` 在 `weeks_len` 非零时必须指向调用期间可读的 `weeks_len` 字节。
/// - `out_payload` 必须非空、对齐且独占可写（`NULL` 时返回 `ZZU_INVALID_INPUT`）；写入的缓冲区须通过 `zzu_buffer_free` 释放。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_timetable_to_calendar(
    weeks: *const u8,
    weeks_len: usize,
    out_payload: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented pointer contracts.
    unsafe {
        compute(out_payload, || {
            let weeks: Timetable = parse(bytes(weeks, weeks_len)?)?;
            reply(&weeks.to_calendar().to_string())
        })
    }
}

/// 使用 `ZzuCasClient` 创建本科教务客户端（本地完成，不发起网络请求）。
///
/// 新句柄与 `cas` 共享认证会话、运行时和时限；创建后释放 `cas` 不影响它。
///
/// # Safety
///
/// - `cas` 必须为 `NULL` 或指向尚未释放的有效句柄（`NULL` 时返回 `ZZU_INVALID_INPUT`）。
/// - `out_eas` 必须非空、对齐且独占可写；成功时写入的句柄须通过 `zzu_eas_free` 释放。
/// - `out_error` 可以为 `NULL`；若非空则必须对齐且独占可写，失败时写入的错误 JSON 须通过 `zzu_buffer_free` 释放。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_eas_new(
    cas: *const ZzuCasClient,
    out_eas: *mut *mut ZzuEasClient,
    out_error: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented input and output pointer contracts.
    unsafe {
        create(out_eas, out_error, || {
            let cas = cas
                .as_ref()
                .ok_or_else(|| FfiError::InvalidInput("CAS 句柄为空".into()))?;
            let http = cas.0.trust_roots().client()?;
            let eas = UndergradEasClient::with_http_client(cas.0.service().clone(), http);
            Ok(ZzuEasClient(cas.0.sibling(Guarded::new(eas))))
        })
    }
}

/// 释放教务客户端句柄（传入 `NULL` 时无操作），不会阻塞，也可以在回调中调用。
///
/// 在途调用不受影响，其回调仍会照常触发。
///
/// # Safety
///
/// `eas` 必须为 `NULL`，或由 `zzu_eas_new` 返回且尚未释放的句柄；释放后不得再使用。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_eas_free(eas: *mut ZzuEasClient) {
    // SAFETY: The caller returns this owned handle exactly once.
    unsafe { discard(eas) }
}

/// 异步调用教务方法（如 `"semesters"`），调用与回调规则同 `zzu_cas_call`。
///
/// # Safety
///
/// `eas` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call`。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_eas_call(
    eas: *const ZzuEasClient,
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
            eas.as_ref().map(|eas| &eas.0),
            method,
            params,
            params_len,
            callback,
            user_data,
            out_call,
        )
    }
}

/// 同步调用教务方法，规则同 `zzu_cas_call_blocking`。
///
/// # Safety
///
/// `eas` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call_blocking`。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_eas_call_blocking(
    eas: *const ZzuEasClient,
    method: *const c_char,
    params: *const u8,
    params_len: usize,
    out_payload: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented pointer contracts.
    unsafe {
        call_blocking(
            eas.as_ref().map(|eas| &eas.0),
            method,
            params,
            params_len,
            out_payload,
        )
    }
}
