use std::ffi::{c_char, c_void};

use jiff::civil::Date;
use serde::Deserialize;
use zzu::eas::{
    CalendarExportOptions, ClassroomFetchOptions, ClassroomQuery, DEFAULT_CLASSROOM_CONCURRENCY,
    DEFAULT_CLASSROOM_PAGE_SIZE, Timetable, TimetableOptions, UndergradEasClient,
};

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
struct TimetableParams {
    #[serde(default)]
    semester_id: Option<i64>,
    #[serde(default)]
    include_experiments: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TimetableCalendarParams {
    timetable: Timetable,
    #[serde(default)]
    options: Option<CalendarExportOptions>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClassroomOccupanciesPage {
    date: Date,
    #[serde(default)]
    campus_id: Option<i64>,
    #[serde(default)]
    building_ids: Vec<i64>,
    #[serde(default)]
    room_type_ids: Vec<i64>,
    #[serde(default)]
    floors: Vec<i32>,
    #[serde(default)]
    min_seats: Option<u32>,
    #[serde(default)]
    max_seats: Option<u32>,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    page_size: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClassroomOccupancies {
    date: Date,
    #[serde(default)]
    campus_id: Option<i64>,
    #[serde(default)]
    building_ids: Vec<i64>,
    #[serde(default)]
    room_type_ids: Vec<i64>,
    #[serde(default)]
    floors: Vec<i32>,
    #[serde(default)]
    min_seats: Option<u32>,
    #[serde(default)]
    max_seats: Option<u32>,
    #[serde(default)]
    page_size: Option<u32>,
    #[serde(default)]
    concurrency: Option<usize>,
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
            "timetable" => {
                let TimetableParams {
                    semester_id,
                    include_experiments,
                } = parse(params)?;
                let options = TimetableOptions {
                    include_experiments: include_experiments
                        .unwrap_or(TimetableOptions::default().include_experiments),
                };
                reply(&self.read().await?.timetable(semester_id, options).await?)
            }
            "transcript" => reply(&self.read().await?.transcript().await?),
            "exams" => reply(&self.read().await?.exams().await?),
            "classroom_catalog" => reply(&self.read().await?.classroom_catalog().await?),
            "classroom_occupancies_page" => {
                let args: ClassroomOccupanciesPage = parse(params)?;
                let query = ClassroomQuery {
                    date: args.date,
                    campus_id: args.campus_id,
                    building_ids: args.building_ids,
                    room_type_ids: args.room_type_ids,
                    floors: args.floors,
                    min_seats: args.min_seats,
                    max_seats: args.max_seats,
                };
                let page = args.page.unwrap_or(1);
                reply(
                    &self
                        .read()
                        .await?
                        .classroom_occupancies_page(&query, page, args.page_size)
                        .await?,
                )
            }
            "classroom_occupancies" => {
                let args: ClassroomOccupancies = parse(params)?;
                let query = ClassroomQuery {
                    date: args.date,
                    campus_id: args.campus_id,
                    building_ids: args.building_ids,
                    room_type_ids: args.room_type_ids,
                    floors: args.floors,
                    min_seats: args.min_seats,
                    max_seats: args.max_seats,
                };
                let options = ClassroomFetchOptions {
                    page_size: args.page_size.unwrap_or(DEFAULT_CLASSROOM_PAGE_SIZE),
                    concurrency: args.concurrency.unwrap_or(DEFAULT_CLASSROOM_CONCURRENCY),
                };
                reply(
                    &self
                        .read()
                        .await?
                        .classroom_occupancies(&query, options)
                        .await?,
                )
            }
            _ => Err(FfiError::UnknownMethod(method.into())),
        }
    }
}

/// 将学期课表渲染为 `iCalendar`（`.ics`）文本（当前线程同步完成，不发起网络请求，无需客户端）。
///
/// `params` 为 UTF-8 JSON 对象 `{"timetable": Timetable, "options"?: CalendarExportOptions}`，
/// 其中 `timetable` 为 `timetable` 方法返回的课表快照，`options` 可省略。
/// 返回 `ZZU_OK` 时 `*out_payload` 为 JSON 字符串形式的 `.ics` 文本，否则为错误 JSON。
///
/// # Safety
///
/// - `params` 在 `params_len` 非零时必须指向调用期间可读的 `params_len` 字节。
/// - `out_payload` 必须非空、对齐且独占可写（`NULL` 时返回 `ZZU_INVALID_INPUT`）；写入的缓冲区须通过 `zzu_buffer_free` 释放。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_timetable_to_calendar(
    params: *const u8,
    params_len: usize,
    out_payload: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented pointer contracts.
    unsafe {
        compute(out_payload, || {
            let TimetableCalendarParams { timetable, options } = parse(bytes(params, params_len)?)?;
            let options = options.unwrap_or_default();
            reply(&timetable.to_calendar(&options)?.to_string())
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
