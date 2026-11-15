//! 各客户端的 C 导出与方法分发：把方法名与 JSON 参数映射到 `zzu` 调用，并把结果编码回 JSON。
//!
//! 每个文件对应 `zzu` 的一种客户端，导出句柄与其构造方式都和 `zzu` 保持一致。

mod cas;
mod eas;
mod ecard;
mod network;

use std::{
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};

use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::error::{FfiError, FfiResult};

pub use cas::*;
pub use eas::*;
pub use ecard::*;
pub use network::*;

/// 解析 JSON 参数；空输入等同于 `{}`，方便无参方法传 `NULL`。
pub(crate) fn parse<T: DeserializeOwned>(params: &[u8]) -> FfiResult<T> {
    let params = if params.is_empty() { b"{}" } else { params };
    serde_json::from_slice(params)
        .map_err(|error| FfiError::InvalidInput(format!("JSON 参数无效：{error}")))
}

fn reply<T: Serialize + ?Sized>(value: &T) -> FfiResult<Vec<u8>> {
    serde_json::to_vec(value).map_err(FfiError::Encode)
}

fn done() -> FfiResult<Vec<u8>> {
    reply(&())
}

/// 以公平异步读写锁保护客户端状态：查询持共享读锁并发执行，会话变更与写操作持独占写锁；
/// 任一锁守卫在栈展开期间释放即标记损坏，后续读写均拒绝访问。
pub(crate) struct Guarded<T> {
    value: RwLock<T>,
    is_poisoned: AtomicBool,
}

impl<T> Guarded<T> {
    fn new(value: T) -> Self {
        Self {
            value: RwLock::new(value),
            is_poisoned: AtomicBool::new(false),
        }
    }

    async fn read(&self) -> FfiResult<Guard<'_, RwLockReadGuard<'_, T>>> {
        let guard = self.value.read().await;
        if self.is_poisoned.load(Ordering::Acquire) {
            return Err(FfiError::Poisoned);
        }
        Ok(Guard {
            guard,
            is_poisoned: &self.is_poisoned,
        })
    }

    async fn write(&self) -> FfiResult<Guard<'_, RwLockWriteGuard<'_, T>>> {
        let guard = self.value.write().await;
        if self.is_poisoned.load(Ordering::Acquire) {
            return Err(FfiError::Poisoned);
        }
        Ok(Guard {
            guard,
            is_poisoned: &self.is_poisoned,
        })
    }
}

struct Guard<'a, G> {
    guard: G,
    is_poisoned: &'a AtomicBool,
}

impl<G: Deref> Deref for Guard<'_, G> {
    type Target = G::Target;
    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<G: DerefMut> DerefMut for Guard<'_, G> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl<G> Drop for Guard<'_, G> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.is_poisoned.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures_util::FutureExt;

    use super::*;

    #[tokio::test]
    async fn panic_while_locked_poisons_the_service() {
        let service = Arc::new(Guarded::new(0_u32));
        let task = service.clone();
        let outcome = std::panic::AssertUnwindSafe(async move {
            let mut value = task.write().await.unwrap();
            *value = 1;
            panic!("operation panic");
        })
        .catch_unwind()
        .await;
        assert!(outcome.is_err());
        assert!(matches!(service.read().await, Err(FfiError::Poisoned)));
        assert!(matches!(service.write().await, Err(FfiError::Poisoned)));
    }

    #[tokio::test]
    async fn writes_wait_for_all_queries_and_block_new_queries_until_done() {
        let service = Guarded::new(0_u32);
        let first = service.read().await.unwrap();
        let second = service.read().await.unwrap();
        let write = service.write();
        tokio::pin!(write);
        assert!(write.as_mut().now_or_never().is_none());
        drop(first);
        assert!(write.as_mut().now_or_never().is_none());
        drop(second);
        let mut update = write.await.unwrap();
        *update = 1;
        assert!(service.read().now_or_never().is_none());
        drop(update);
        assert_eq!(*service.read().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn panic_in_a_query_poisons_future_reads_and_writes() {
        let service = Guarded::new(0_u32);
        let outcome = std::panic::AssertUnwindSafe(async {
            let _query = service.read().await.unwrap();
            panic!("query panic");
        })
        .catch_unwind()
        .await;
        assert!(outcome.is_err());
        assert!(matches!(service.read().await, Err(FfiError::Poisoned)));
        assert!(matches!(service.write().await, Err(FfiError::Poisoned)));
    }
}
