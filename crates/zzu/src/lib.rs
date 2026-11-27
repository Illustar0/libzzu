//! 校园服务异步 Rust 客户端库，同时是 C ABI 层 `libzzu` 的协议、会话状态机与数据模型实现。
//!
//! 各业务域按 Cargo feature 组织（默认全部启用，可通过 `default-features = false` 按需裁剪）：
//! - `auth`：统一身份认证（CAS）会话、多因素认证、设备信任管理与单点登录跳转；
//! - `eas`（依赖 `auth`）：本科教务、课表、日历导出、成绩单、考试安排、培养方案与空教室查询；
//! - `ecard`（依赖 `auth`）：校园卡余额、在线校园码、交易流水、宿舍电控与充值；
//! - `mail`（依赖 `auth`）：校园邮箱网页入口、收件箱未读数与邮件摘要查询；
//! - `network`：校园网 Portal 自动探测认证、本机出口 IP 探测与自助服务设备管理；
//! - `schemars`（可选）：为所有公开可序列化数据模型实现 `schemars::JsonSchema`。
//!
//! 所有网络操作默认不设隐式超时，可通过 `tokio::time::timeout` 或 [`http_client_builder`] 按需配置。

#![forbid(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(feature = "auth")]
#[cfg_attr(docsrs, doc(cfg(feature = "auth")))]
pub mod auth;
#[cfg(feature = "eas")]
#[cfg_attr(docsrs, doc(cfg(feature = "eas")))]
pub mod eas;
#[cfg(feature = "ecard")]
#[cfg_attr(docsrs, doc(cfg(feature = "ecard")))]
pub mod ecard;
#[cfg(feature = "mail")]
#[cfg_attr(docsrs, doc(cfg(feature = "mail")))]
pub mod mail;
#[cfg(feature = "network")]
#[cfg_attr(docsrs, doc(cfg(feature = "network")))]
pub mod network;

#[cfg(feature = "auth")]
mod crypto;
mod error;
mod http;
#[cfg(feature = "ecard")]
mod money;
#[cfg(all(feature = "schemars", any(feature = "auth", feature = "network")))]
mod schema;

pub use error::{
    AuthenticationError, AuthenticationErrorKind, CryptoError, CryptoErrorKind, DecodeError,
    DecodeErrorKind, Error, ErrorDetails, HttpError, HttpErrorKind, InvalidInputError,
    InvalidInputErrorKind, NotFoundError, NotFoundErrorKind, RemoteError, RemoteErrorKind,
    RequestContext, Result, TransportError, TransportErrorKind,
};
pub use http::{TrustRoots, client_builder as http_client_builder};
pub use reqwest::{Client as HttpClient, ClientBuilder as HttpClientBuilder};
#[cfg(feature = "ecard")]
#[cfg_attr(docsrs, doc(cfg(feature = "ecard")))]
pub use rust_decimal::Decimal;
