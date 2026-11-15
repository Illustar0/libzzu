use std::{
    cell::Cell,
    future::{Future, pending},
    num::NonZeroUsize,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::Arc,
    time::Duration,
};

use futures_util::FutureExt;
use serde::Deserialize;
use tokio::{
    runtime::{Builder, Handle, Runtime},
    sync::watch,
};
use zzu::TrustRoots;

use crate::{
    buffer::ZzuBuffer,
    error::{FfiError, FfiResult, ZZU_OK, ZzuStatus},
    methods::parse,
};

const DEFAULT_WORKER_THREADS: usize = 2;

thread_local! {
    static IS_WORKER_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// 后台异步运行时不透明句柄，可供多个客户端共享，须通过 `zzu_runtime_free` 释放。
pub struct ZzuRuntime {
    pub(crate) worker: Arc<Worker>,
}

/// 持有 Tokio 运行时与各客户端共用的根证书；由句柄、客户端与在途任务共同引用，最后一个引用释放时才关闭。
pub(crate) struct Worker {
    runtime: Option<Runtime>,
    pub trust_roots: TrustRoots,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeOptions {
    worker_threads: Option<NonZeroUsize>,
    root_certificates_pem: Option<String>,
}

/// Android 平台校验器会在工作线程上同步下载吊销列表并阻塞超时，因此必须显式提供根证书。
fn trust_roots(root_certificates_pem: Option<String>) -> FfiResult<TrustRoots> {
    match root_certificates_pem {
        Some(pem) => Ok(TrustRoots::from_pem_bundle(pem.as_bytes())?),
        None if cfg!(target_os = "android") => Err(FfiError::InvalidInput(
            "Android 上必须提供 rootCertificatesPem".into(),
        )),
        None => Ok(TrustRoots::default()),
    }
}

impl Worker {
    fn new(worker_threads: usize, trust_roots: TrustRoots) -> FfiResult<Self> {
        let runtime = Builder::new_multi_thread()
            .worker_threads(worker_threads)
            .thread_name("zzu-worker")
            .on_thread_start(|| IS_WORKER_THREAD.set(true))
            .enable_all()
            .build()?;
        Ok(Self {
            runtime: Some(runtime),
            trust_roots,
        })
    }

    fn handle(&self) -> &Handle {
        self.runtime
            .as_ref()
            .expect("live worker owns its runtime")
            .handle()
    }

    pub fn is_current_thread() -> bool {
        IS_WORKER_THREAD.get()
    }

    /// 启动一次操作，并保证 `complete` 恰好执行一次。
    ///
    /// 任务持有运行时引用，因此释放句柄不会丢弃在途操作，也就不会漏掉回调。
    pub fn spawn<F, C>(
        self: &Arc<Self>,
        operation: F,
        limit: Duration,
        cancel: watch::Receiver<bool>,
        complete: C,
    ) where
        F: Future<Output = FfiResult<Vec<u8>>> + Send + 'static,
        C: FnOnce(ZzuStatus, Vec<u8>) + Send + 'static,
    {
        let worker = self.clone();
        self.handle().spawn(async move {
            let outcome = AssertUnwindSafe(bounded(operation, limit, cancel))
                .catch_unwind()
                .await;
            match outcome.unwrap_or(Err(FfiError::Panic)) {
                Ok(payload) => complete(ZZU_OK, payload),
                Err(error) => complete(error.status(), error.to_payload()),
            }
            drop(worker);
        });
    }
}

async fn bounded<F>(
    operation: F,
    limit: Duration,
    cancel: watch::Receiver<bool>,
) -> FfiResult<Vec<u8>>
where
    F: Future<Output = FfiResult<Vec<u8>>>,
{
    tokio::select! {
        biased;
        () = cancelled(cancel) => Err(FfiError::Cancelled),
        outcome = tokio::time::timeout(limit, operation) => {
            outcome.map_err(|_| FfiError::Timeout(limit))?
        }
    }
}

/// 等待取消信号；调用句柄被释放后信号永远不会到来，操作照常运行至结束。
async fn cancelled(mut cancel: watch::Receiver<bool>) {
    if cancel.wait_for(|is_cancelled| *is_cancelled).await.is_err() {
        pending::<()>().await;
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// 把新建对象写入 `out`；失败时写入错误 JSON，`out` 保持为 `NULL`。
pub(crate) unsafe fn create<T>(
    out: *mut *mut T,
    out_error: *mut *mut ZzuBuffer,
    build: impl FnOnce() -> FfiResult<T>,
) -> ZzuStatus {
    if !out_error.is_null() {
        // SAFETY: A non-null output slot is writable and does not alias another argument.
        unsafe { out_error.write(ptr::null_mut()) };
    }
    let outcome = if out.is_null() {
        Err(FfiError::InvalidInput("输出指针为空".into()))
    } else {
        // SAFETY: A non-null output slot is writable and does not alias another argument.
        unsafe { out.write(ptr::null_mut()) };
        catch_unwind(AssertUnwindSafe(build)).unwrap_or(Err(FfiError::Panic))
    };
    match outcome {
        Ok(value) => {
            // SAFETY: Success implies out passed the null check above.
            unsafe { out.write(Box::into_raw(Box::new(value))) };
            ZZU_OK
        }
        Err(error) => {
            if !out_error.is_null() {
                // SAFETY: The checked output slot receives ownership of this buffer once.
                unsafe { out_error.write(ZzuBuffer::into_raw(error.to_payload())) };
            }
            error.status()
        }
    }
}

pub(crate) unsafe fn discard<T>(pointer: *mut T) {
    if pointer.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: A live allocation returned by this library is transferred back exactly once.
        drop(unsafe { Box::from_raw(pointer) });
    }));
}

/// 创建后台异步运行时，通常每个进程只需创建一个。
///
/// `options` 为 UTF-8 JSON 对象，可选字段 `workerThreads`（正整数，默认 `2`）与
/// `rootCertificatesPem`（PEM 格式的根证书，提供时只信任它们，Android 上必填）；
/// `options_len` 为 `0` 时使用默认配置（此时 `options` 可传 `NULL`）。
///
/// # Safety
///
/// - `options` 在 `options_len` 非零时必须指向调用期间可读的 `options_len` 字节。
/// - `out_runtime` 必须非空、对齐且独占可写；成功时写入的句柄须通过 `zzu_runtime_free` 释放。
/// - `out_error` 可以为 `NULL`；若非空则必须对齐且独占可写，失败时写入的错误 JSON 须通过 `zzu_buffer_free` 释放。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_runtime_new(
    options: *const u8,
    options_len: usize,
    out_runtime: *mut *mut ZzuRuntime,
    out_error: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented input and output pointer contracts.
    unsafe {
        create(out_runtime, out_error, || {
            let options: RuntimeOptions = parse(bytes(options, options_len)?)?;
            let worker_threads = options
                .worker_threads
                .map_or(DEFAULT_WORKER_THREADS, NonZeroUsize::get);
            let trust_roots = trust_roots(options.root_certificates_pem)?;
            Ok(ZzuRuntime {
                worker: Arc::new(Worker::new(worker_threads, trust_roots)?),
            })
        })
    }
}

/// 释放运行时句柄（传入 `NULL` 时无操作），不会阻塞，也可以在回调中调用。
///
/// 仍在使用该运行时的客户端与在途调用不受影响：运行时在它们全部结束后才关闭。
///
/// # Safety
///
/// `runtime` 必须为 `NULL`，或由 `zzu_runtime_new` 返回且尚未释放的句柄；释放后不得再使用。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_runtime_free(runtime: *mut ZzuRuntime) {
    // SAFETY: The caller returns this owned handle exactly once.
    unsafe { discard(runtime) }
}

pub(crate) unsafe fn bytes<'a>(data: *const u8, len: usize) -> FfiResult<&'a [u8]> {
    if len == 0 {
        return Ok(&[]);
    }
    if data.is_null() {
        return Err(FfiError::InvalidInput("数据指针为空".into()));
    }
    // SAFETY: A non-null input points to len readable bytes for the duration of the call.
    Ok(unsafe { std::slice::from_raw_parts(data, len) })
}
