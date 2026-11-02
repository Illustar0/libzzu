use std::{error::Error as _, time::Duration};

use serde::Serialize;
use zzu_core::{AuthenticationErrorKind, Error, RequestContext, TransportErrorKind};

/// C ABI 统一使用的状态码类型（`ZZU_OK` 表示成功，非零表示失败大类）。
///
/// 封装层应把未识别的取值当作 `ZZU_UNKNOWN_ERROR` 处理。
pub type ZzuStatus = i32;
/// 操作成功完成。
pub const ZZU_OK: ZzuStatus = 0;
/// 调用方传入了空指针、非 UTF-8 文本、非法 JSON 或取值非法的参数。
pub const ZZU_INVALID_INPUT: ZzuStatus = 1;
/// 网络连接或底层传输失败。
pub const ZZU_TRANSPORT_ERROR: ZzuStatus = 2;
/// 服务端返回非成功的 HTTP 状态码。
pub const ZZU_HTTP_ERROR: ZzuStatus = 3;
/// 响应报文或协议数据解码失败。
pub const ZZU_DECODE_ERROR: ZzuStatus = 4;
/// 身份认证或多因素认证失败（细分原因见错误 JSON 的 `kind` 字段）。
pub const ZZU_AUTHENTICATION_ERROR: ZzuStatus = 5;
/// 远端服务返回业务级拒绝或失败结果。
pub const ZZU_REMOTE_ERROR: ZzuStatus = 6;
/// 请求的目标资源（如指定学期 ID）不存在。
pub const ZZU_NOT_FOUND: ZzuStatus = 7;
/// 密钥解析或加解密处理失败。
pub const ZZU_CRYPTO_ERROR: ZzuStatus = 8;
/// 内部异步运行时初始化失败。
pub const ZZU_RUNTIME_ERROR: ZzuStatus = 9;
/// Rust 内部发生 panic，或客户端先前已因 panic 损坏；须释放该客户端句柄并重建。
pub const ZZU_PANIC: ZzuStatus = 10;
/// 未归类的未知错误。
pub const ZZU_UNKNOWN_ERROR: ZzuStatus = 11;
/// 单次操作超过配置的总时限。
pub const ZZU_TIMEOUT: ZzuStatus = 12;
/// 操作已由 `zzu_call_cancel` 取消。
pub const ZZU_CANCELLED: ZzuStatus = 13;
/// 当前链接库不支持该方法名（通常意味着封装层比链接库新）。
pub const ZZU_UNKNOWN_METHOD: ZzuStatus = 14;

#[derive(Debug, thiserror::Error)]
pub(crate) enum FfiError {
    #[error(transparent)]
    Core(#[from] Error),
    #[error("{0}")]
    InvalidInput(String),
    #[error("未知方法：{0}")]
    UnknownMethod(String),
    #[error("无法识别付款动作，请更新链接库版本")]
    UnsupportedPaymentAction,
    #[error("运行环境初始化失败")]
    Runtime(#[from] std::io::Error),
    #[error("操作超过 {} 毫秒总时限", .0.as_millis())]
    Timeout(Duration),
    #[error("操作已取消")]
    Cancelled,
    #[error("结果序列化失败")]
    Encode(#[source] serde_json::Error),
    #[error("Rust 内部 panic，请释放并重新创建客户端")]
    Panic,
    #[error("服务已被先前的 panic 破坏，请释放并重新创建客户端")]
    Poisoned,
}

pub(crate) type FfiResult<T> = Result<T, FfiError>;

impl FfiError {
    pub fn status(&self) -> ZzuStatus {
        match self {
            Self::Core(error) => match error {
                Error::InvalidInput(_) => ZZU_INVALID_INPUT,
                Error::Transport(_) => ZZU_TRANSPORT_ERROR,
                Error::Http(_) => ZZU_HTTP_ERROR,
                Error::Decode(_) => ZZU_DECODE_ERROR,
                Error::Authentication(_) => ZZU_AUTHENTICATION_ERROR,
                Error::Remote(_) => ZZU_REMOTE_ERROR,
                Error::NotFound(_) => ZZU_NOT_FOUND,
                Error::Crypto(_) => ZZU_CRYPTO_ERROR,
                _ => ZZU_UNKNOWN_ERROR,
            },
            Self::InvalidInput(_) => ZZU_INVALID_INPUT,
            Self::UnknownMethod(_) => ZZU_UNKNOWN_METHOD,
            Self::UnsupportedPaymentAction => ZZU_DECODE_ERROR,
            Self::Runtime(_) => ZZU_RUNTIME_ERROR,
            Self::Timeout(_) => ZZU_TIMEOUT,
            Self::Cancelled => ZZU_CANCELLED,
            Self::Encode(_) => ZZU_UNKNOWN_ERROR,
            Self::Panic | Self::Poisoned => ZZU_PANIC,
        }
    }

    fn kind(&self) -> Option<&'static str> {
        match self {
            Self::Core(Error::Transport(error)) => match error.kind() {
                TransportErrorKind::Timeout => Some("timeout"),
                TransportErrorKind::Connection => Some("connection"),
                _ => None,
            },
            Self::Core(Error::Authentication(error)) => match error.kind() {
                AuthenticationErrorKind::InvalidCredentials => Some("invalid_credentials"),
                AuthenticationErrorKind::NotAuthenticated => Some("not_authenticated"),
                AuthenticationErrorKind::TokenExpired => Some("token_expired"),
                AuthenticationErrorKind::MfaRequired => Some("mfa_required"),
                AuthenticationErrorKind::InvalidMfaState => Some("invalid_mfa_state"),
                AuthenticationErrorKind::MfaVerificationFailed => Some("mfa_verification_failed"),
                _ => None,
            },
            _ => None,
        }
    }

    fn cause(&self) -> Option<String> {
        let mut source = self.source()?;
        let mut cause = source.to_string();
        while let Some(next) = source.source() {
            cause.push_str(": ");
            cause.push_str(&next.to_string());
            source = next;
        }
        Some(cause)
    }

    /// 编码错误 JSON；字段全为字符串与整数，序列化不会失败。
    pub fn to_payload(&self) -> Vec<u8> {
        let request = match self {
            Self::Core(error) => error.request(),
            _ => None,
        };
        serde_json::to_vec(&ErrorPayload {
            status: self.status(),
            kind: self.kind(),
            message: self.to_string(),
            cause: self.cause(),
            request: request.map(RequestPayload::from),
        })
        .unwrap_or_default()
    }
}

#[derive(Serialize)]
struct ErrorPayload<'a> {
    status: ZzuStatus,
    kind: Option<&'static str>,
    message: String,
    cause: Option<String>,
    request: Option<RequestPayload<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestPayload<'a> {
    method: &'a str,
    url: &'a str,
    status_code: Option<u16>,
    error_code: Option<&'a str>,
    response_body: Option<&'a str>,
}

impl<'a> From<&'a RequestContext> for RequestPayload<'a> {
    fn from(context: &'a RequestContext) -> Self {
        Self {
            method: &context.method,
            url: &context.url,
            status_code: context.status_code,
            error_code: context.error_code.as_deref(),
            response_body: context.response_body.as_deref(),
        }
    }
}
