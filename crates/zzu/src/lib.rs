//! 校园服务异步 Rust 客户端库，同时是 C ABI 层 `libzzu` 的协议、会话状态机与数据模型实现。
//!
//! 围绕统一身份认证（[`auth::CasClient`]）组织各业务系统客户端，涵盖本科教务与课表（[`eas`]）、
//! 校园卡与宿舍电费（[`ecard`]）以及校园网 Portal 与自助服务（[`network`]）。
//! 所有网络操作默认不设隐式超时，可通过 `tokio::time::timeout` 或 [`http_client_builder`] 按需配置。

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
