//! 为 `zzu` 与 `libzzu` 提供共享协议、会话状态机与数据模型的核心实现。
//!
//! 所有网络操作均不设隐式超时，时限由调用方按需控制（如使用 `tokio::time::timeout` 包裹）。

#![forbid(unsafe_code)]

pub mod auth;
pub mod eas;
pub mod ecard;
pub mod network;

mod crypto;
mod error;
mod http;
mod money;

pub use error::{
    AuthenticationError, AuthenticationErrorKind, CryptoError, CryptoErrorKind, DecodeError,
    DecodeErrorKind, Error, ErrorDetails, HttpError, HttpErrorKind, InvalidInputError,
    InvalidInputErrorKind, NotFoundError, NotFoundErrorKind, RemoteError, RemoteErrorKind,
    RequestContext, Result, TransportError, TransportErrorKind,
};
pub use http::{TrustRoots, client_builder as http_client_builder};
pub use reqwest::{Client as HttpClient, ClientBuilder as HttpClientBuilder};
pub use rust_decimal::Decimal;
