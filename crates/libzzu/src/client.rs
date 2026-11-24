use std::{
    ffi::{CStr, c_char, c_void},
    future::Future,
    num::NonZeroU64,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::{Arc, mpsc},
    time::Duration,
};

use tokio::sync::watch;
use zzu::TrustRoots;

use crate::{
    buffer::ZzuBuffer,
    error::{FfiError, FfiResult, ZZU_INVALID_INPUT, ZZU_OK, ZZU_RUNTIME_ERROR, ZzuStatus},
    runtime::{Worker, bytes, discard},
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) fn time_limit(timeout_ms: Option<NonZeroU64>) -> Duration {
    timeout_ms.map_or(DEFAULT_TIMEOUT, |limit| Duration::from_millis(limit.get()))
}

/// 一次在途调用的不透明句柄，仅用于取消，须通过 `zzu_call_free` 释放。
pub struct ZzuCall {
    cancel: watch::Sender<bool>,
}

/// 调用完成回调，必须使用 C 调用约定且不得让异常或 panic 穿出。
///
/// 回调运行在后台工作线程上，须尽快返回；`status` 为 `ZZU_OK` 时 `payload` 为结果 JSON，
/// 否则为错误 JSON。`payload` 恒非空，其所有权移交给回调方，须通过 `zzu_buffer_free` 释放，
/// 可传往其他线程并在回调返回后继续使用。
pub type ZzuCallback = Option<
    unsafe extern "C" fn(user_data: *mut c_void, status: ZzuStatus, payload: *mut ZzuBuffer),
>;

/// 一种客户端的 JSON 方法分发入口。
pub(crate) trait Service: Send + Sync + 'static {
    fn call(&self, method: &str, params: &[u8]) -> impl Future<Output = FfiResult<Vec<u8>>> + Send;
}

/// 各客户端句柄共用的内部结构，封装客户端服务及其运行时与操作时限。
pub(crate) struct Client<T> {
    core: Arc<ClientCore<T>>,
}

struct ClientCore<T> {
    worker: Arc<Worker>,
    timeout: Duration,
    service: T,
}

impl<T> Client<T> {
    pub fn new(worker: Arc<Worker>, timeout: Duration, service: T) -> Self {
        Self {
            core: Arc::new(ClientCore {
                worker,
                timeout,
                service,
            }),
        }
    }

    pub fn service(&self) -> &T {
        &self.core.service
    }

    /// 当前运行时信任的根证书。
    pub fn trust_roots(&self) -> &TrustRoots {
        &self.core.worker.trust_roots
    }

    /// 创建沿用当前运行时与时限的另一种客户端。
    pub fn sibling<U>(&self, service: U) -> Client<U> {
        Client::new(self.core.worker.clone(), self.core.timeout, service)
    }
}

struct Request<T> {
    core: Arc<ClientCore<T>>,
    method: String,
    params: Vec<u8>,
}

impl<T: Service> Request<T> {
    /// 在返回前复制全部输入，调用方的缓冲区只需在本次调用期间有效。
    unsafe fn read(
        client: Option<&Client<T>>,
        method: *const c_char,
        params: *const u8,
        params_len: usize,
    ) -> FfiResult<Self> {
        let client = client.ok_or_else(|| FfiError::InvalidInput("客户端句柄为空".into()))?;
        if method.is_null() {
            return Err(FfiError::InvalidInput("方法名为空".into()));
        }
        // SAFETY: A non-null method name is NUL-terminated and readable during the call.
        let method = unsafe { CStr::from_ptr(method) }
            .to_str()
            .map_err(|_| FfiError::InvalidInput("方法名必须为 UTF-8".into()))?
            .to_owned();
        // SAFETY: The caller guarantees params_len readable bytes during the call.
        let params = unsafe { bytes(params, params_len) }?.to_vec();
        Ok(Self {
            core: client.core.clone(),
            method,
            params,
        })
    }

    fn spawn(
        self,
        cancel: watch::Receiver<bool>,
        complete: impl FnOnce(ZzuStatus, Vec<u8>) + Send + 'static,
    ) {
        let Self {
            core,
            method,
            params,
        } = self;
        let (worker, timeout) = (core.worker.clone(), core.timeout);
        let operation = async move { core.service.call(&method, &params).await };
        worker.spawn(operation, timeout, cancel, complete);
    }
}

struct UserData(*mut c_void);

// SAFETY: The exported call functions document that user_data must be usable from any thread.
unsafe impl Send for UserData {}

impl UserData {
    fn into_inner(self) -> *mut c_void {
        self.0
    }
}

/// 各 `zzu_*_call` 导出函数的共同实现。
pub(crate) unsafe fn call<T: Service>(
    client: Option<&Client<T>>,
    method: *const c_char,
    params: *const u8,
    params_len: usize,
    callback: ZzuCallback,
    user_data: *mut c_void,
    out_call: *mut *mut ZzuCall,
) -> ZzuStatus {
    if !out_call.is_null() {
        // SAFETY: A non-null output slot is writable and does not alias another argument.
        unsafe { out_call.write(ptr::null_mut()) };
    }
    let start = AssertUnwindSafe(|| {
        let Some(callback) = callback else {
            return ZZU_INVALID_INPUT;
        };
        // SAFETY: The caller upholds the documented input pointer contracts.
        let Ok(request) = (unsafe { Request::read(client, method, params, params_len) }) else {
            return ZZU_INVALID_INPUT;
        };
        let (cancel, cancelled) = watch::channel(false);
        if !out_call.is_null() {
            // SAFETY: Written before spawning, so the handle is visible before the callback fires.
            unsafe { out_call.write(Box::into_raw(Box::new(ZzuCall { cancel }))) };
        }
        let user_data = UserData(user_data);
        request.spawn(cancelled, move |status, payload| {
            let payload = ZzuBuffer::into_raw(payload);
            // SAFETY: The callback and user_data satisfy the exported call contract, and
            // the worker delivers this completion exactly once.
            unsafe { callback(user_data.into_inner(), status, payload) };
        });
        ZZU_OK
    });
    catch_unwind(start).unwrap_or(ZZU_INVALID_INPUT)
}

/// 各 `zzu_*_call_blocking` 导出函数的共同实现。
pub(crate) unsafe fn call_blocking<T: Service>(
    client: Option<&Client<T>>,
    method: *const c_char,
    params: *const u8,
    params_len: usize,
    out_payload: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    if out_payload.is_null() {
        return ZZU_INVALID_INPUT;
    }
    // SAFETY: The caller upholds the documented input pointer contracts.
    let outcome = catch_unwind(AssertUnwindSafe(|| unsafe {
        wait_for_call(client, method, params, params_len)
    }))
    .unwrap_or_else(|_| {
        let error = FfiError::Panic;
        (error.status(), error.to_payload())
    });
    let (status, payload) = outcome;
    // SAFETY: The checked output slot receives ownership of this buffer once.
    unsafe { out_payload.write(ZzuBuffer::into_raw(payload)) };
    status
}

/// 在当前线程执行不依赖客户端与运行时的本地计算，结果与错误都写入 `out_payload`。
pub(crate) unsafe fn compute(
    out_payload: *mut *mut ZzuBuffer,
    operation: impl FnOnce() -> FfiResult<Vec<u8>>,
) -> ZzuStatus {
    if out_payload.is_null() {
        return ZZU_INVALID_INPUT;
    }
    let (status, payload) =
        match catch_unwind(AssertUnwindSafe(operation)).unwrap_or(Err(FfiError::Panic)) {
            Ok(payload) => (ZZU_OK, payload),
            Err(error) => (error.status(), error.to_payload()),
        };
    // SAFETY: The checked output slot receives ownership of this buffer once.
    unsafe { out_payload.write(ZzuBuffer::into_raw(payload)) };
    status
}

unsafe fn wait_for_call<T: Service>(
    client: Option<&Client<T>>,
    method: *const c_char,
    params: *const u8,
    params_len: usize,
) -> (ZzuStatus, Vec<u8>) {
    if Worker::is_current_thread() {
        let error = FfiError::InvalidInput("不得在回调线程中调用阻塞接口".into());
        return (error.status(), error.to_payload());
    }
    // SAFETY: The caller upholds the documented input pointer contracts.
    let request = match unsafe { Request::read(client, method, params, params_len) } {
        Ok(request) => request,
        Err(error) => return (error.status(), error.to_payload()),
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    let (_cancel, cancelled) = watch::channel(false);
    request.spawn(cancelled, move |status, payload| {
        let _ = sender.send((status, payload));
    });
    receiver.recv().unwrap_or((ZZU_RUNTIME_ERROR, Vec::new()))
}

/// 请求取消一次在途异步调用，可在任意线程重复调用；操作已结束时为空操作。
///
/// 取消尽力而为：回调仍保证恰好触发一次，状态码通常为 `ZZU_CANCELLED`，
/// 若完成与取消发生竞态也可能收到真实执行结果。非幂等操作（如电费充值）取消后服务端可能已受理，重试前须核实余额。
///
/// # Safety
///
/// `call` 必须为 `NULL`，或指向尚未释放的有效 `ZzuCall` 句柄。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_call_cancel(call: *const ZzuCall) {
    // SAFETY: Non-null handles remain live until zzu_call_free.
    if let Some(call) = unsafe { call.as_ref() } {
        call.cancel.send_replace(true);
    }
}

/// 释放调用句柄（传入 `NULL` 时无操作）；不会取消调用，回调仍照常触发。
///
/// # Safety
///
/// `call` 必须为 `NULL`，或由 `zzu_*_call` 写入且尚未释放的句柄；释放后不得再使用。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_call_free(call: *mut ZzuCall) {
    // SAFETY: The caller returns this owned handle exactly once.
    unsafe { discard(call) }
}

#[cfg(test)]
mod tests {
    use std::{future::pending, slice};

    use serde_json::{Value, json};

    use super::*;
    use crate::{
        ZZU_AUTHENTICATION_ERROR, ZZU_PANIC, ZZU_TIMEOUT, ZZU_UNKNOWN_METHOD, ZzuCasClient,
        ZzuEasClient, ZzuEcardClient, ZzuMailClient, ZzuRuntime, zzu_buffer_data, zzu_buffer_free,
        zzu_buffer_len, zzu_cas_call, zzu_cas_call_blocking, zzu_cas_free, zzu_cas_new,
        zzu_eas_call, zzu_eas_free, zzu_eas_new, zzu_ecard_call, zzu_ecard_free, zzu_ecard_new,
        zzu_mail_call, zzu_mail_call_blocking, zzu_mail_free, zzu_mail_new,
        zzu_portal_discover_blocking, zzu_recharge_order_checkout_url_for, zzu_room_id_parse,
        zzu_runtime_free, zzu_runtime_new, zzu_timetable_to_calendar,
    };

    type Completion = (ZzuStatus, Value);
    type CallFn<T> = unsafe extern "C" fn(
        *const T,
        *const c_char,
        *const u8,
        usize,
        ZzuCallback,
        *mut c_void,
        *mut *mut ZzuCall,
    ) -> ZzuStatus;

    fn take_payload(buffer: *mut ZzuBuffer) -> Value {
        assert!(!buffer.is_null());
        // SAFETY: The buffer is a live allocation from this library and is released once.
        unsafe {
            let bytes = slice::from_raw_parts(zzu_buffer_data(buffer), zzu_buffer_len(buffer));
            let value = serde_json::from_slice(bytes).unwrap();
            zzu_buffer_free(buffer);
            value
        }
    }

    unsafe extern "C" fn record(
        user_data: *mut c_void,
        status: ZzuStatus,
        payload: *mut ZzuBuffer,
    ) {
        // SAFETY: user_data is the boxed sender leaked by call_async for this one completion.
        let sender = unsafe { Box::from_raw(user_data.cast::<mpsc::Sender<Completion>>()) };
        sender.send((status, take_payload(payload))).unwrap();
    }

    fn call_async<T>(
        entry: CallFn<T>,
        client: *const T,
        method: &CStr,
        params: &[u8],
    ) -> Completion {
        let (sender, receiver) = mpsc::channel();
        let user_data = Box::into_raw(Box::new(sender)).cast();
        // SAFETY: Inputs outlive the call and user_data is reclaimed by record.
        let status = unsafe {
            entry(
                client,
                method.as_ptr(),
                params.as_ptr(),
                params.len(),
                Some(record),
                user_data,
                ptr::null_mut(),
            )
        };
        assert_eq!(status, ZZU_OK);
        receiver.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    fn call_cas(client: *const ZzuCasClient, method: &CStr, params: &[u8]) -> Completion {
        call_async(zzu_cas_call, client, method, params)
    }

    fn call_cas_blocking(client: *const ZzuCasClient, method: &CStr, params: &[u8]) -> Completion {
        let mut payload = ptr::null_mut();
        // SAFETY: Inputs outlive the call and the output slot is writable.
        let status = unsafe {
            zzu_cas_call_blocking(
                client,
                method.as_ptr(),
                params.as_ptr(),
                params.len(),
                &raw mut payload,
            )
        };
        (status, take_payload(payload))
    }

    struct Fixture {
        runtime: *mut ZzuRuntime,
        cas: *mut ZzuCasClient,
    }

    impl Fixture {
        fn new() -> Self {
            let mut runtime = ptr::null_mut();
            let mut cas = ptr::null_mut();
            let config = br#"{"account":"","password":"","deviceId":"test-device"}"#;
            // SAFETY: Inputs and output slots are live; handles are released by Drop.
            unsafe {
                assert_eq!(
                    zzu_runtime_new(ptr::null(), 0, &raw mut runtime, ptr::null_mut()),
                    ZZU_OK
                );
                assert_eq!(
                    zzu_cas_new(
                        runtime,
                        config.as_ptr(),
                        config.len(),
                        &raw mut cas,
                        ptr::null_mut()
                    ),
                    ZZU_OK
                );
            }
            Self { runtime, cas }
        }

        fn worker(&self) -> Arc<Worker> {
            // SAFETY: The runtime handle stays live until Drop.
            unsafe { &*self.runtime }.worker.clone()
        }

        fn eas(&self) -> *mut ZzuEasClient {
            let mut eas = ptr::null_mut();
            // SAFETY: The CAS handle is live and the output slot is writable.
            let status = unsafe { zzu_eas_new(self.cas, &raw mut eas, ptr::null_mut()) };
            assert_eq!(status, ZZU_OK);
            eas
        }

        fn ecard(&self) -> *mut ZzuEcardClient {
            let mut ecard = ptr::null_mut();
            // SAFETY: The CAS handle is live and the output slot is writable.
            let status = unsafe { zzu_ecard_new(self.cas, &raw mut ecard, ptr::null_mut()) };
            assert_eq!(status, ZZU_OK);
            ecard
        }

        fn mail(&self) -> *mut ZzuMailClient {
            let mut mail = ptr::null_mut();
            // SAFETY: The CAS handle is live and the output slot is writable.
            let status = unsafe { zzu_mail_new(self.cas, &raw mut mail, ptr::null_mut()) };
            assert_eq!(status, ZZU_OK);
            mail
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // SAFETY: Both handles are owned by this fixture and released once.
            unsafe {
                zzu_cas_free(self.cas);
                zzu_runtime_free(self.runtime);
            }
        }
    }

    fn cancel_pair() -> (watch::Sender<bool>, watch::Receiver<bool>) {
        watch::channel(false)
    }

    #[test]
    fn header_matches_cbindgen_output() {
        assert_eq!(
            include_str!(concat!(env!("OUT_DIR"), "/libzzu.h")),
            include_str!("../include/libzzu.h")
        );
    }

    #[test]
    fn async_and_blocking_calls_share_results_and_error_payloads() {
        let fixture = Fixture::new();
        for call in [call_cas, call_cas_blocking] {
            assert_eq!(call(fixture.cas, c"tokens", b""), (ZZU_OK, Value::Null));
            let (status, error) = call(fixture.cas, c"user_token", b"");
            assert_eq!(status, ZZU_AUTHENTICATION_ERROR);
            assert_eq!(error["status"], json!(ZZU_AUTHENTICATION_ERROR));
            assert_eq!(error["kind"], "not_authenticated");
            assert!(error["message"].is_string());
            assert!(error["request"].is_null());
        }
    }

    #[test]
    fn eas_ecard_and_mail_share_the_cas_session_and_outlive_its_handle() {
        let fixture = Fixture::new();
        let eas = fixture.eas();
        let ecard = fixture.ecard();
        let mail = fixture.mail();
        drop(fixture);
        let (status, error) = call_async(zzu_eas_call, eas, c"login", b"");
        assert_eq!(status, ZZU_AUTHENTICATION_ERROR);
        assert_eq!(error["kind"], "not_authenticated");
        let (status, error) = call_async(zzu_ecard_call, ecard, c"balance", b"");
        assert_eq!(status, ZZU_AUTHENTICATION_ERROR);
        assert_eq!(error["kind"], "not_authenticated");

        assert_eq!(
            call_async(zzu_mail_call, mail, c"is_logged_in", b""),
            (ZZU_OK, json!(false))
        );
        for method in [c"login", c"webmail_url", c"unread_count"] {
            let (status, error) = call_async(zzu_mail_call, mail, method, b"");
            assert_eq!(status, ZZU_AUTHENTICATION_ERROR);
            assert_eq!(error["kind"], "not_authenticated");
        }
        let mut payload = ptr::null_mut();
        // SAFETY: The mail handle and method name are live; the output slot is writable.
        let status = unsafe {
            zzu_mail_call_blocking(mail, c"unknown".as_ptr(), ptr::null(), 0, &raw mut payload)
        };
        assert_eq!(status, ZZU_UNKNOWN_METHOD);
        assert_eq!(take_payload(payload)["status"], json!(ZZU_UNKNOWN_METHOD));

        // SAFETY: All three handles came from their constructors and are released once.
        unsafe {
            zzu_eas_free(eas);
            zzu_ecard_free(ecard);
            zzu_mail_free(mail);
        }
    }

    #[test]
    fn unknown_methods_report_a_typed_status() {
        let fixture = Fixture::new();
        let (status, error) = call_cas(
            fixture.cas,
            c"set_device",
            br#"{"deviceId":"other-device"}"#,
        );
        assert_eq!(status, ZZU_UNKNOWN_METHOD);
        assert_eq!(error["status"], json!(ZZU_UNKNOWN_METHOD));
    }

    #[test]
    fn invalid_params_are_rejected_with_typed_statuses() {
        let fixture = Fixture::new();
        let eas = fixture.eas();
        assert_eq!(
            call_cas(fixture.cas, c"verify_sms", b"[not json").0,
            ZZU_INVALID_INPUT
        );
        // 拼错的字段名必须报错，而不是被静默忽略后退回默认学期。
        assert_eq!(
            call_async(zzu_eas_call, eas, c"timetable", br#"{"semester_id":1}"#).0,
            ZZU_INVALID_INPUT
        );
        // SAFETY: The handle came from its constructor and is released once.
        unsafe { zzu_eas_free(eas) };
    }

    #[test]
    fn timetable_calendar_renders_and_validates_without_a_client() {
        let params = json!({
            "timetable": {
                "semester": {
                    "id": 194,
                    "code": "2026-2027-1",
                    "nameZh": "2026-2027学年第一学期",
                    "schoolYear": "2026-2027",
                    "season": "autumn",
                    "startDate": "2026-09-07",
                    "endDate": "2027-01-17",
                    "weekStartOnSunday": false,
                    "countInTerm": true,
                    "weekIndices": [1, 2]
                },
                "classes": [{
                    "id": 1001,
                    "code": "CS101-001",
                    "origin": "regular",
                    "course": {
                        "id": 2001,
                        "code": "CS101",
                        "nameZh": "程序设计",
                        "credits": 2.0,
                        "periodInfo": {"total": 32, "weeks": 16, "periodsPerWeek": 2}
                    },
                    "teachers": [{"nameZh": "教师"}],
                    "scheduleState": "published"
                }],
                "sessions": [{
                    "classId": 1001,
                    "scheduleGroupId": 3001,
                    "weekIndex": 1,
                    "date": "2026-09-07",
                    "weekday": 1,
                    "startTime": "2026-09-07T08:00:00+08:00[Asia/Shanghai]",
                    "endTime": "2026-09-07T09:40:00+08:00[Asia/Shanghai]",
                    "startUnit": 1,
                    "endUnit": 2,
                    "teachers": [{"nameZh": "教师"}],
                    "kind": "theory"
                }]
            },
            "options": {
                "weeks": [1],
                "alarmMinutesBefore": [15]
            }
        });
        let (status, payload) = compute_json(
            zzu_timetable_to_calendar,
            &serde_json::to_vec(&params).unwrap(),
        );
        assert_eq!(status, ZZU_OK, "{payload}");
        let ics = payload.as_str().unwrap();
        assert!(ics.contains("BEGIN:VEVENT"));
        assert!(ics.contains("BEGIN:VALARM"));
        assert!(ics.contains("SUMMARY:程序设计"));

        let bad_options = json!({
            "timetable": params["timetable"],
            "options": {"weeks": [0]}
        });
        assert_eq!(
            compute_json(
                zzu_timetable_to_calendar,
                &serde_json::to_vec(&bad_options).unwrap()
            )
            .0,
            ZZU_INVALID_INPUT
        );

        let unknown_field = json!({
            "timetable": params["timetable"],
            "extra": true
        });
        assert_eq!(
            compute_json(
                zzu_timetable_to_calendar,
                &serde_json::to_vec(&unknown_field).unwrap()
            )
            .0,
            ZZU_INVALID_INPUT
        );
    }

    type ComputeFn = unsafe extern "C" fn(*const u8, usize, *mut *mut ZzuBuffer) -> ZzuStatus;

    fn compute_json(entry: ComputeFn, input: &[u8]) -> Completion {
        let mut payload = ptr::null_mut();
        // SAFETY: The input outlives the call and the output slot is writable.
        let status = unsafe { entry(input.as_ptr(), input.len(), &raw mut payload) };
        (status, take_payload(payload))
    }

    #[test]
    fn room_ids_are_validated_without_a_client() {
        assert_eq!(
            compute_json(zzu_room_id_parse, b"1-2--3-301"),
            (ZZU_OK, json!("1-2--3-301"))
        );
        assert_eq!(compute_json(zzu_room_id_parse, b"bad").0, ZZU_INVALID_INPUT);
        assert_eq!(
            compute_json(zzu_room_id_parse, &[0xff]).0,
            ZZU_INVALID_INPUT
        );
        // SAFETY: Null output slots are rejected before any write.
        let status = unsafe { zzu_room_id_parse(ptr::null(), 0, ptr::null_mut()) };
        assert_eq!(status, ZZU_INVALID_INPUT);
    }

    #[test]
    fn checkout_urls_are_built_from_the_returned_order() {
        let params = json!({
            "order": {
                "id": "school-order",
                "amount": 10,
                "checkoutUrl": "https://payment.v.zzu.edu.cn/WapCashDesk/e-pay/payways.html",
                "payment": {"orderInfo": {
                    "partnerid": "partner", "journo": "school-order", "merchantno": "merchant",
                    "channelno": "103", "txamt": "1000",
                }},
            },
            "channel": {"id": "0106", "name": "支付宝"},
        });
        let (status, url) = compute_json(
            zzu_recharge_order_checkout_url_for,
            &serde_json::to_vec(&params).unwrap(),
        );
        assert_eq!(status, ZZU_OK, "{url}");
        assert!(url.as_str().unwrap().contains("callPaywayid=0106"));
    }

    #[test]
    fn null_and_malformed_arguments_never_reach_the_worker() {
        let fixture = Fixture::new();
        let mut payload = ptr::null_mut();
        let invalid_utf8 = [0xff_u8, 0];
        // SAFETY: Every pointer is either null or live for the call.
        unsafe {
            assert_eq!(
                zzu_cas_call(
                    ptr::null(),
                    c"tokens".as_ptr(),
                    ptr::null(),
                    0,
                    Some(record),
                    ptr::null_mut(),
                    ptr::null_mut(),
                ),
                ZZU_INVALID_INPUT
            );
            assert_eq!(
                zzu_cas_call(
                    fixture.cas,
                    invalid_utf8.as_ptr().cast(),
                    ptr::null(),
                    0,
                    Some(record),
                    ptr::null_mut(),
                    ptr::null_mut(),
                ),
                ZZU_INVALID_INPUT
            );
            assert_eq!(
                zzu_cas_call(
                    fixture.cas,
                    c"tokens".as_ptr(),
                    ptr::null(),
                    0,
                    None,
                    ptr::null_mut(),
                    ptr::null_mut(),
                ),
                ZZU_INVALID_INPUT
            );
            assert_eq!(
                zzu_cas_call_blocking(fixture.cas, ptr::null(), ptr::null(), 0, &raw mut payload,),
                ZZU_INVALID_INPUT
            );
            zzu_buffer_free(payload);
            assert_eq!(
                zzu_cas_call_blocking(
                    fixture.cas,
                    c"tokens".as_ptr(),
                    ptr::null(),
                    8,
                    &raw mut payload,
                ),
                ZZU_INVALID_INPUT
            );
            zzu_buffer_free(payload);
            assert_eq!(
                zzu_portal_discover_blocking(ptr::null(), ptr::null(), 0, &raw mut payload),
                ZZU_INVALID_INPUT
            );
            zzu_buffer_free(payload);
            let options = br#"{"timeout":1}"#;
            assert_eq!(
                zzu_portal_discover_blocking(
                    fixture.runtime,
                    options.as_ptr(),
                    options.len(),
                    &raw mut payload,
                ),
                ZZU_INVALID_INPUT
            );
            zzu_buffer_free(payload);
        }
    }

    #[test]
    fn invalid_configuration_reports_a_json_error() {
        let mut runtime = ptr::null_mut();
        let mut error = ptr::null_mut();
        let options = br#"{"workerThreads":0}"#;
        // SAFETY: Inputs and output slots are live; the error buffer is consumed once.
        unsafe {
            assert_eq!(
                zzu_runtime_new(
                    options.as_ptr(),
                    options.len(),
                    &raw mut runtime,
                    &raw mut error
                ),
                ZZU_INVALID_INPUT
            );
            assert!(runtime.is_null());
        }
        assert_eq!(take_payload(error)["status"], json!(ZZU_INVALID_INPUT));

        let fixture = Fixture::new();
        for (config, expected_message) in [
            (r#"{"acount":"typo"}"#, "acount"),
            (r#"{"account":"","password":""}"#, "deviceId"),
            (r#"{"account":"","password":"","deviceId":""}"#, "设备 ID"),
            (
                r#"{"account":"","password":"","deviceId":" \t"}"#,
                "设备 ID",
            ),
        ] {
            let mut cas = ptr::null_mut();
            let mut error = ptr::null_mut();
            // SAFETY: Inputs and output slots are live; the error buffer is consumed once.
            let status = unsafe {
                zzu_cas_new(
                    fixture.runtime,
                    config.as_ptr(),
                    config.len(),
                    &raw mut cas,
                    &raw mut error,
                )
            };
            assert_eq!(status, ZZU_INVALID_INPUT);
            assert!(cas.is_null());
            let error = take_payload(error);
            assert_eq!(error["status"], json!(ZZU_INVALID_INPUT));
            assert!(
                error["message"]
                    .as_str()
                    .unwrap()
                    .contains(expected_message)
            );
        }
    }

    #[test]
    fn deadline_expires_pending_operations() {
        let fixture = Fixture::new();
        let (_cancel, cancelled) = cancel_pair();
        let (sender, receiver) = mpsc::channel();
        fixture.worker().spawn(
            pending(),
            Duration::from_millis(20),
            cancelled,
            move |status, _| sender.send(status).unwrap(),
        );
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            ZZU_TIMEOUT
        );
    }

    #[test]
    fn panics_become_a_status_instead_of_unwinding_into_the_host() {
        let fixture = Fixture::new();
        let (_cancel, cancelled) = cancel_pair();
        let (sender, receiver) = mpsc::channel();
        fixture.worker().spawn(
            async { panic!("operation panic") },
            Duration::from_secs(60),
            cancelled,
            move |status, _| sender.send(status).unwrap(),
        );
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            ZZU_PANIC
        );
    }

    #[test]
    fn blocking_calls_are_refused_on_worker_threads() {
        let fixture = Fixture::new();
        let (_cancel, cancelled) = cancel_pair();
        let (sender, receiver) = mpsc::channel();
        let cas = fixture.cas as usize;
        fixture.worker().spawn(
            async { Ok(Vec::new()) },
            Duration::from_secs(60),
            cancelled,
            move |_, _| {
                let cas = cas as *const ZzuCasClient;
                sender.send(call_cas_blocking(cas, c"tokens", b"")).unwrap();
            },
        );
        let (status, _) = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(status, ZZU_INVALID_INPUT);
    }

    #[test]
    fn calls_work_inside_a_host_tokio_runtime() {
        let host = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        host.block_on(async {
            let fixture = Fixture::new();
            assert_eq!(
                call_cas_blocking(fixture.cas, c"is_logged_in", b""),
                (ZZU_OK, json!(false))
            );
        });
    }

    #[test]
    fn eas_classroom_methods_validate_params_and_auth_state() {
        let fixture = Fixture::new();
        let eas = fixture.eas();

        let (status, error) = call_async(zzu_eas_call, eas, c"classroom_catalog", b"");
        assert_eq!(status, ZZU_AUTHENTICATION_ERROR);
        assert_eq!(error["kind"], "not_authenticated");

        // 未知字段应立即返回 ZZU_INVALID_INPUT（严进宽出）。
        let (status, _) = call_async(
            zzu_eas_call,
            eas,
            c"classroom_occupancies_page",
            br#"{"date":"2026-11-23","campusAssoc":2}"#,
        );
        assert_eq!(status, ZZU_INVALID_INPUT);

        // 非法页码、每页条数、座位数区间与并发度均应返回 ZZU_INVALID_INPUT。
        for (method, payload) in [
            (
                c"classroom_occupancies_page",
                br#"{"date":"2026-11-23","page":0}"#.as_slice(),
            ),
            (
                c"classroom_occupancies_page",
                br#"{"date":"2026-11-23","pageSize":0}"#.as_slice(),
            ),
            (
                c"classroom_occupancies_page",
                br#"{"date":"2026-11-23","minSeats":80,"maxSeats":40}"#.as_slice(),
            ),
            (
                c"classroom_occupancies",
                br#"{"date":"2026-11-23","concurrency":0}"#.as_slice(),
            ),
            (
                c"classroom_occupancies",
                br#"{"date":"2026-11-23","pageSize":0}"#.as_slice(),
            ),
        ] {
            let (status, _) = call_async(zzu_eas_call, eas, method, payload);
            assert_eq!(status, ZZU_INVALID_INPUT);
        }

        // 合法参数在未登录时应进入教务客户端鉴权并返回 not_authenticated。
        for method in [c"classroom_occupancies_page", c"classroom_occupancies"] {
            let (status, error) =
                call_async(zzu_eas_call, eas, method, br#"{"date":"2026-11-23"}"#);
            assert_eq!(status, ZZU_AUTHENTICATION_ERROR);
            assert_eq!(error["kind"], "not_authenticated");
        }

        // SAFETY: Handle came from Fixture::eas and is released once.
        unsafe { zzu_eas_free(eas) };
    }
}
