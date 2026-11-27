use std::{error::Error as StdError, fmt, sync::Arc};

use serde::{Deserialize, Serialize};

/// 统一错误类型为 [`Error`] 的 `Result` 别名。
pub type Result<T> = std::result::Result<T, Error>;

/// HTTP 请求失败时的诊断上下文。
///
/// 为避免泄露敏感凭据，`url` 已预先剥离查询参数、片段与用户信息；
/// [`fmt::Debug`] 实现仅输出响应体字节长度，不打印完整正文。
#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    /// HTTP 请求方法（如 `"GET"`、`"POST"`）。
    pub method: String,
    /// 已脱敏的请求目标 URL（不含查询参数、片段与认证凭据）。
    pub url: String,
    /// 收到的 HTTP 状态码；若请求在建立连接或等待响应阶段失败则为 `None`。
    #[serde(default)]
    pub status_code: Option<u16>,
    /// 从响应 JSON 中提取的业务错误码文本（如 `code`、`result` 或 `returncode` 字段）。
    #[serde(default)]
    pub error_code: Option<String>,
    /// 原始响应体文本；在读取响应体前即失败时为 `None`。
    #[serde(default)]
    pub response_body: Option<String>,
}

impl fmt::Debug for RequestContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestContext")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("status_code", &self.status_code)
            .field("error_code", &self.error_code)
            .field(
                "response_body_bytes",
                &self.response_body.as_ref().map(String::len),
            )
            .finish()
    }
}

/// 网络传输层错误的细分类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TransportErrorKind {
    /// 连接或读写操作超时。
    Timeout,
    /// 无法建立网络连接（如连接被拒绝、DNS 或代理握手失败）。
    Connection,
    /// 未归类的其他传输层故障。
    Unknown,
}

/// 非 2xx HTTP 状态错误的细分类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HttpErrorKind {
    /// 未归类的 HTTP 状态错误。
    Unknown,
}

/// 响应或协议数据解码错误的细分类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DecodeErrorKind {
    /// 未归类的解码或格式解析错误。
    Unknown,
}

/// 调用方输入参数校验错误的细分类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InvalidInputErrorKind {
    /// 未归类的输入参数错误。
    Unknown,
}

/// 身份认证与多因素认证（MFA）错误的细分类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthenticationErrorKind {
    /// 账号或密码错误（服务端确认凭据无效）。
    InvalidCredentials,
    /// 当前客户端尚未登录或已登出。
    NotAuthenticated,
    /// 令牌已过期或即将过期，无法继续使用。
    TokenExpired,
    /// 登录流程需要完成多因素认证（如短信验证码）后才能继续。
    MfaRequired,
    /// 多因素认证状态不满足当前操作前提（如未发送短信即校验验证码，或当前会话不支持短信验证）。
    InvalidMfaState,
    /// 多因素认证校验未通过（如短信验证码错误）。
    MfaVerificationFailed,
    /// 未归类的其他认证失败。
    Unknown,
}

/// 远端服务业务错误的细分类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RemoteErrorKind {
    /// 未归类的远端业务错误。
    Unknown,
}

/// 资源不存在错误的细分类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NotFoundErrorKind {
    /// 未归类的资源未找到错误。
    Unknown,
}

/// 密钥解析或加解密错误的细分类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CryptoErrorKind {
    /// 未归类的密码学处理错误。
    Unknown,
}

/// 网络传输错误详情。
pub type TransportError = ErrorDetails<TransportErrorKind>;
/// 非 2xx HTTP 状态错误详情。
pub type HttpError = ErrorDetails<HttpErrorKind>;
/// 响应或协议数据解码错误详情。
pub type DecodeError = ErrorDetails<DecodeErrorKind>;
/// 输入参数校验错误详情。
pub type InvalidInputError = ErrorDetails<InvalidInputErrorKind>;
/// 身份认证错误详情。
pub type AuthenticationError = ErrorDetails<AuthenticationErrorKind>;
/// 远端服务业务错误详情。
pub type RemoteError = ErrorDetails<RemoteErrorKind>;
/// 资源不存在错误详情。
pub type NotFoundError = ErrorDetails<NotFoundErrorKind>;
/// 密码学处理错误详情。
pub type CryptoError = ErrorDetails<CryptoErrorKind>;

/// 通用错误详情，包含细分类型、错误消息、可选的请求上下文与底层错误源。
#[derive(Debug)]
pub struct ErrorDetails<K> {
    kind: K,
    context: ErrorContext,
}

impl<K> ErrorDetails<K> {
    fn new(kind: K, message: impl Into<String>) -> Self {
        Self {
            kind,
            context: ErrorContext {
                message: message.into(),
                path: None,
                request: None,
                source: None,
            },
        }
    }

    /// 返回该错误对应的细分类型标识。
    #[must_use]
    pub fn kind(&self) -> K
    where
        K: Copy,
    {
        self.kind
    }

    /// 返回不含 HTTP 状态码后缀的错误主消息。
    #[must_use]
    pub fn message(&self) -> &str {
        &self.context.message
    }

    /// 返回触发该错误时的 HTTP 请求诊断上下文（若存在）。
    #[must_use]
    pub fn request(&self) -> Option<&RequestContext> {
        self.context.request.as_deref()
    }

    fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.context.source = Some(Box::new(source));
        self
    }
}

impl<K> fmt::Display for ErrorDetails<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.context.fmt(formatter)
    }
}

impl<K: fmt::Debug> StdError for ErrorDetails<K> {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.context.source()
    }
}

impl DecodeError {
    /// 返回解析或校验失败对应的响应 JSON 字段路径（如 `"data.items[2].firstUsedTime"`）。
    ///
    /// 该路径仅包含字段名与数组下标，不含原始响应字面量，可安全用于日志与终端诊断；
    /// 若错误发生在整段报文层级（如非法 JSON、HTML 解析失败或非 JSON 协议错误）则返回 `None`。
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        self.context.path.as_deref()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}{}", status_suffix(.request.as_deref()))]
struct ErrorContext {
    message: String,
    path: Option<String>,
    request: Option<Arc<RequestContext>>,
    #[source]
    source: Option<Box<dyn StdError + Send + Sync>>,
}

/// 校园服务客户端统一错误类型。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// 网络连接、超时或底层 HTTP 传输失败。
    #[error(transparent)]
    Transport(TransportError),
    /// 服务端返回非成功的 HTTP 状态码。
    #[error(transparent)]
    Http(HttpError),
    /// 响应正文、HTML、JSON、`Base64URL` 或协议字段解析失败。
    #[error(transparent)]
    Decode(DecodeError),
    /// 调用方传入的参数格式或取值不符合要求。
    #[error(transparent)]
    InvalidInput(InvalidInputError),
    /// 登录失败、未登录、令牌失效或多因素认证未通过。
    #[error(transparent)]
    Authentication(AuthenticationError),
    /// 远端服务正常响应但返回了业务级拒绝或失败状态。
    #[error(transparent)]
    Remote(RemoteError),
    /// 请求的目标资源（如指定学期 ID）不存在。
    #[error(transparent)]
    NotFound(NotFoundError),
    /// RSA、SM2 或 SM4 密钥解析与加解密运算失败。
    #[error(transparent)]
    Crypto(CryptoError),
}

impl Error {
    fn context(&self) -> &ErrorContext {
        match self {
            Self::Transport(error) => &error.context,
            Self::Http(error) => &error.context,
            Self::Decode(error) => &error.context,
            Self::InvalidInput(error) => &error.context,
            Self::Authentication(error) => &error.context,
            Self::Remote(error) => &error.context,
            Self::NotFound(error) => &error.context,
            Self::Crypto(error) => &error.context,
        }
    }

    #[cfg(any(feature = "auth", feature = "network", test))]
    fn context_mut(&mut self) -> &mut ErrorContext {
        match self {
            Self::Transport(error) => &mut error.context,
            Self::Http(error) => &mut error.context,
            Self::Decode(error) => &mut error.context,
            Self::InvalidInput(error) => &mut error.context,
            Self::Authentication(error) => &mut error.context,
            Self::Remote(error) => &mut error.context,
            Self::NotFound(error) => &mut error.context,
            Self::Crypto(error) => &mut error.context,
        }
    }

    /// 返回不含 HTTP 状态码后缀的错误主消息。
    #[must_use]
    pub fn message(&self) -> &str {
        &self.context().message
    }

    /// 返回关联的 HTTP 请求诊断上下文（若该错误源自某次 HTTP 请求）。
    #[must_use]
    pub fn request(&self) -> Option<&RequestContext> {
        self.context().request.as_deref()
    }

    /// 判断是否为网络传输层错误（[`Self::Transport`]）。
    #[must_use]
    pub fn is_transport(&self) -> bool {
        matches!(self, Self::Transport(_))
    }

    /// 判断是否为非成功 HTTP 状态错误（[`Self::Http`]）。
    #[must_use]
    pub fn is_http(&self) -> bool {
        matches!(self, Self::Http(_))
    }

    /// 判断是否为响应或协议数据解码错误（[`Self::Decode`]）。
    #[must_use]
    pub fn is_decode(&self) -> bool {
        matches!(self, Self::Decode(_))
    }

    /// 判断是否为调用方输入参数校验错误（[`Self::InvalidInput`]）。
    #[must_use]
    pub fn is_invalid_input(&self) -> bool {
        matches!(self, Self::InvalidInput(_))
    }

    /// 判断是否为身份认证相关错误（[`Self::Authentication`]）。
    #[must_use]
    pub fn is_authentication(&self) -> bool {
        matches!(self, Self::Authentication(_))
    }

    /// 判断是否因尚未登录而失败（[`AuthenticationErrorKind::NotAuthenticated`]）。
    #[must_use]
    pub fn is_not_authenticated(&self) -> bool {
        matches!(self, Self::Authentication(error) if error.kind() == AuthenticationErrorKind::NotAuthenticated)
    }

    /// 判断是否需要完成多因素认证（[`AuthenticationErrorKind::MfaRequired`]）。
    #[must_use]
    pub fn is_mfa_required(&self) -> bool {
        matches!(self, Self::Authentication(error) if error.kind() == AuthenticationErrorKind::MfaRequired)
    }

    /// 判断是否为远端服务业务错误（[`Self::Remote`]）。
    #[must_use]
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Remote(_))
    }

    /// 判断是否为资源不存在错误（[`Self::NotFound`]）。
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound(_))
    }

    /// 判断是否为密码学处理错误（[`Self::Crypto`]）。
    #[must_use]
    pub fn is_crypto(&self) -> bool {
        matches!(self, Self::Crypto(_))
    }

    /// 判断是否为网络传输超时错误（[`TransportErrorKind::Timeout`]）。
    #[must_use]
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Transport(error) if error.kind() == TransportErrorKind::Timeout)
    }

    /// 返回错误大类的 `snake_case` 标识符（与 JSON 序列化中的 `code` 字段一致）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Transport(_) => "transport",
            Self::Http(_) => "http",
            Self::Decode(_) => "decode",
            Self::InvalidInput(_) => "invalid_input",
            Self::Authentication(_) => "authentication",
            Self::Remote(_) => "remote",
            Self::NotFound(_) => "not_found",
            Self::Crypto(_) => "crypto",
        }
    }

    pub(crate) fn invalid_input(message: String) -> Self {
        Self::InvalidInput(ErrorDetails::new(InvalidInputErrorKind::Unknown, message))
    }
    #[cfg(any(feature = "auth", feature = "network"))]
    pub(crate) fn not_authenticated() -> Self {
        Self::authentication(
            AuthenticationErrorKind::NotAuthenticated,
            "需要先登录".into(),
        )
    }
    #[cfg(any(feature = "auth", feature = "network"))]
    pub(crate) fn authentication(kind: AuthenticationErrorKind, message: String) -> Self {
        Self::Authentication(ErrorDetails::new(kind, message))
    }
    #[cfg(any(feature = "auth", feature = "network", test))]
    pub(crate) fn http(message: String) -> Self {
        Self::Http(ErrorDetails::new(HttpErrorKind::Unknown, message))
    }
    #[cfg(feature = "auth")]
    pub(crate) fn mfa_required() -> Self {
        Self::authentication(
            AuthenticationErrorKind::MfaRequired,
            "需要完成 MFA 验证".into(),
        )
    }
    #[cfg(feature = "auth")]
    pub(crate) fn invalid_mfa_state(message: String) -> Self {
        Self::authentication(AuthenticationErrorKind::InvalidMfaState, message)
    }
    #[cfg(any(feature = "auth", feature = "network", test))]
    pub(crate) fn remote(message: String) -> Self {
        Self::Remote(ErrorDetails::new(RemoteErrorKind::Unknown, message))
    }
    #[cfg(feature = "eas")]
    pub(crate) fn not_found(message: String) -> Self {
        Self::NotFound(ErrorDetails::new(NotFoundErrorKind::Unknown, message))
    }
    #[cfg(feature = "auth")]
    pub(crate) fn crypto(message: String) -> Self {
        Self::Crypto(ErrorDetails::new(CryptoErrorKind::Unknown, message))
    }
    #[cfg(any(feature = "auth", feature = "network", test))]
    pub(crate) fn decode_message(message: String) -> Self {
        Self::Decode(ErrorDetails::new(DecodeErrorKind::Unknown, message))
    }

    #[cfg(feature = "auth")]
    pub(crate) fn crypto_source(source: impl StdError + Send + Sync + 'static) -> Self {
        Self::crypto_with_source("加密处理失败", source)
    }
    #[cfg(feature = "auth")]
    pub(crate) fn crypto_with_source(
        message: impl Into<String>,
        source: impl StdError + Send + Sync + 'static,
    ) -> Self {
        Self::Crypto(ErrorDetails::new(CryptoErrorKind::Unknown, message).with_source(source))
    }
    #[cfg(any(feature = "auth", feature = "network"))]
    pub(crate) fn decode_with_source(
        message: impl Into<String>,
        source: impl StdError + Send + Sync + 'static,
    ) -> Self {
        Self::Decode(ErrorDetails::new(DecodeErrorKind::Unknown, message).with_source(source))
    }
    #[cfg(feature = "eas")]
    pub(crate) fn map_message(mut self, f: impl FnOnce(&str) -> String) -> Self {
        let updated = f(&self.context().message);
        self.context_mut().message = updated;
        self
    }
    #[cfg(any(feature = "auth", feature = "network", test))]
    pub(crate) fn with_path(mut self, path: impl Into<String>) -> Self {
        self.context_mut().path = Some(path.into());
        self
    }
    #[cfg(any(feature = "auth", feature = "network", test))]
    pub(crate) fn with_request(mut self, context: Arc<RequestContext>) -> Self {
        self.context_mut().request = Some(context);
        self
    }
    #[cfg(feature = "auth")]
    pub(crate) fn into_authentication(self, kind: AuthenticationErrorKind) -> Self {
        let context = match self {
            Self::Http(error) => error.context,
            Self::Remote(error) => error.context,
            error => return error,
        };
        Self::Authentication(ErrorDetails { kind, context })
    }
    pub(crate) fn decode(source: impl StdError + Send + Sync + 'static) -> Self {
        Self::Decode(
            ErrorDetails::new(DecodeErrorKind::Unknown, "响应解析失败").with_source(source),
        )
    }
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        if error.is_builder() {
            return Self::InvalidInput(
                ErrorDetails::new(InvalidInputErrorKind::Unknown, "请求参数无效")
                    .with_source(error.without_url()),
            );
        }
        let kind = if error.is_timeout() {
            TransportErrorKind::Timeout
        } else if error.is_connect() {
            TransportErrorKind::Connection
        } else {
            TransportErrorKind::Unknown
        };
        Self::Transport(ErrorDetails::new(kind, "网络请求失败").with_source(error.without_url()))
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::decode(error)
    }
}

fn status_suffix(context: Option<&RequestContext>) -> String {
    context
        .and_then(|context| context.status_code)
        .map(|status| format!(" (HTTP {status})"))
        .unwrap_or_default()
}

impl Serialize for Error {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("Error", 4)?;
        state.serialize_field("code", self.code())?;
        match self {
            Self::Transport(error) => state.serialize_field("kind", &error.kind())?,
            Self::Http(error) => state.serialize_field("kind", &error.kind())?,
            Self::Decode(error) => state.serialize_field("kind", &error.kind())?,
            Self::InvalidInput(error) => state.serialize_field("kind", &error.kind())?,
            Self::Authentication(error) => state.serialize_field("kind", &error.kind())?,
            Self::Remote(error) => state.serialize_field("kind", &error.kind())?,
            Self::NotFound(error) => state.serialize_field("kind", &error.kind())?,
            Self::Crypto(error) => state.serialize_field("kind", &error.kind())?,
        }
        state.serialize_field("message", &self.to_string())?;
        state.serialize_field("context", &self.request())?;
        state.end()
    }
}

#[cfg(feature = "schemars")]
#[cfg_attr(docsrs, doc(cfg(feature = "schemars")))]
impl schemars::JsonSchema for Error {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Error".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        concat!(module_path!(), "::Error").into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        /// 校园服务客户端统一错误类型。
        #[derive(schemars::JsonSchema)]
        #[serde(tag = "code", rename_all = "snake_case")]
        #[expect(dead_code)]
        enum ErrorWire {
            Transport {
                kind: TransportErrorKind,
                message: String,
                context: Option<RequestContext>,
            },
            Http {
                kind: HttpErrorKind,
                message: String,
                context: Option<RequestContext>,
            },
            Decode {
                kind: DecodeErrorKind,
                message: String,
                context: Option<RequestContext>,
            },
            InvalidInput {
                kind: InvalidInputErrorKind,
                message: String,
                context: Option<RequestContext>,
            },
            Authentication {
                kind: AuthenticationErrorKind,
                message: String,
                context: Option<RequestContext>,
            },
            Remote {
                kind: RemoteErrorKind,
                message: String,
                context: Option<RequestContext>,
            },
            NotFound {
                kind: NotFoundErrorKind,
                message: String,
                context: Option<RequestContext>,
            },
            Crypto {
                kind: CryptoErrorKind,
                message: String,
                context: Option<RequestContext>,
            },
        }

        ErrorWire::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn request_context_serializes_camel_case_roundtrips_and_redacts_debug_body() {
        let context = RequestContext {
            method: "POST".into(),
            url: "https://example.test/api".into(),
            status_code: Some(401),
            error_code: Some("E_AUTH".into()),
            response_body: Some("sensitive-response-body".into()),
        };
        assert!(!format!("{context:?}").contains("sensitive-response-body"));

        let encoded = serde_json::to_value(&context).unwrap();
        assert_eq!(
            encoded,
            json!({
                "method": "POST",
                "url": "https://example.test/api",
                "statusCode": 401,
                "errorCode": "E_AUTH",
                "responseBody": "sensitive-response-body",
            })
        );
        let restored: RequestContext = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(restored, context);

        let error = Error::remote("拒绝访问".into()).with_request(Arc::new(context));
        let encoded_error = serde_json::to_value(&error).unwrap();
        assert_eq!(encoded_error["code"], "remote");
        assert_eq!(encoded_error["context"], encoded);
    }
}
