//! 错误分类、请求诊断上下文与统一 `Result` 类型。

pub use zzu_core::{
    AuthenticationError, AuthenticationErrorKind, CryptoError, CryptoErrorKind, DecodeError,
    DecodeErrorKind, Error, ErrorDetails, HttpError, HttpErrorKind, InvalidInputError,
    InvalidInputErrorKind, NotFoundError, NotFoundErrorKind, RemoteError, RemoteErrorKind,
    RequestContext, Result, TransportError, TransportErrorKind,
};
