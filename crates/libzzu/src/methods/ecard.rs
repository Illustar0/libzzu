use std::{
    ffi::{c_char, c_void},
    net::IpAddr,
};

use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use zzu::ecard::{
    EcardClient, PaymentAction, PaymentChannel, PaymentContext, PaymentDevice, RechargeConfig,
    RechargeOrder, RoomId, TransactionKind, TransactionQuery,
};

use super::{Guarded, ZzuCasClient, done, parse, reply};
use crate::{
    buffer::ZzuBuffer,
    client::{Client, Service, ZzuCall, ZzuCallback, call, call_blocking, compute},
    error::{FfiError, FfiResult, ZzuStatus},
    runtime::{bytes, create, discard},
};

/// 校园卡与电控客户端不透明句柄，线程安全，须通过 `zzu_ecard_free` 释放。
pub struct ZzuEcardClient(Client<Guarded<EcardClient>>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Locations {
    parent: String,
}

#[derive(Serialize)]
struct Location {
    id: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OptionalRoom {
    room: Option<String>,
}

fn parse_optional_room(params: &[u8]) -> FfiResult<Option<RoomId>> {
    let OptionalRoom { room } = parse(params)?;
    Ok(room.as_deref().map(str::parse::<RoomId>).transpose()?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Transactions {
    start_date: Date,
    end_date: Date,
    #[serde(default)]
    kind: Option<TransactionKind>,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    page_size: Option<u32>,
}

impl Transactions {
    fn into_request(self) -> (TransactionQuery, u32, Option<u32>) {
        let query = TransactionQuery {
            start_date: self.start_date,
            end_date: self.end_date,
            kind: self.kind,
        };
        (query, self.page.unwrap_or(1), self.page_size)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RechargeEnergy {
    payment_password: String,
    amount: u32,
    room: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateOrder {
    amount: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RechargeConfigReply {
    amounts: Vec<u32>,
    is_browser_payment_enabled: bool,
}

impl From<RechargeConfig> for RechargeConfigReply {
    fn from(config: RechargeConfig) -> Self {
        Self {
            amounts: config.amounts,
            is_browser_payment_enabled: config.is_browser_payment_enabled,
        }
    }
}

/// 在订单的不透明状态之外补上 `id`，封装层把整个对象原样传回即可。
#[derive(Serialize)]
struct OrderReply<'a> {
    id: &'a str,
    #[serde(flatten)]
    order: &'a RechargeOrder,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WithOrder {
    order: RechargeOrder,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Channel {
    id: String,
    name: String,
    #[serde(default)]
    account_id: Option<String>,
}

impl From<PaymentChannel> for Channel {
    fn from(channel: PaymentChannel) -> Self {
        Self {
            id: channel.id,
            name: channel.name,
            account_id: channel.account_id,
        }
    }
}

impl From<Channel> for PaymentChannel {
    fn from(channel: Channel) -> Self {
        Self {
            id: channel.id,
            name: channel.name,
            account_id: channel.account_id,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckoutUrl {
    order: RechargeOrder,
    channel: Channel,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Device {
    Android,
    Ios,
}

impl From<Device> for PaymentDevice {
    fn from(device: Device) -> Self {
        match device {
            Device::Android => Self::Android,
            Device::Ios => Self::Ios,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Context {
    device: Option<Device>,
    payer_ip: Option<IpAddr>,
}

impl From<Context> for PaymentContext {
    fn from(context: Context) -> Self {
        Self {
            device: context.device.map(Into::into).unwrap_or_default(),
            payer_ip: context.payer_ip,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparePayment {
    order: RechargeOrder,
    channel: Channel,
    context: Option<Context>,
}

#[derive(Serialize)]
struct FormField {
    name: String,
    value: String,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PaymentActionReply {
    Redirect {
        url: String,
        referrer: Option<String>,
    },
    SubmitForm {
        url: String,
        fields: Vec<FormField>,
    },
}

impl TryFrom<PaymentAction> for PaymentActionReply {
    type Error = FfiError;

    fn try_from(action: PaymentAction) -> FfiResult<Self> {
        match action {
            PaymentAction::Redirect { url, referrer } => Ok(Self::Redirect {
                url: url.into(),
                referrer: referrer.map(Into::into),
            }),
            PaymentAction::SubmitForm { url, fields } => Ok(Self::SubmitForm {
                url: url.into(),
                fields: fields
                    .into_iter()
                    .map(|(name, value)| FormField { name, value })
                    .collect(),
            }),
            _ => Err(FfiError::UnsupportedPaymentAction),
        }
    }
}

impl Service for Guarded<EcardClient> {
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
            "default_room" => reply(&self.read().await?.default_room().await?),
            "balance" => reply(&self.read().await?.balance().await?),
            "payment_code" => reply(&self.read().await?.payment_code().await?),
            "transactions" => {
                let (query, page, page_size) = parse::<Transactions>(params)?.into_request();
                let items = self
                    .read()
                    .await?
                    .transactions(&query, page, page_size)
                    .await?;
                reply(&items)
            }
            "locations" => {
                let Locations { parent } = parse(params)?;
                let locations = self.read().await?.locations(&parent).await?;
                let locations: Vec<_> = locations
                    .into_iter()
                    .map(|(id, name)| Location { id, name })
                    .collect();
                reply(&locations)
            }
            "energy_account" => {
                let room = parse_optional_room(params)?;
                reply(&self.read().await?.energy_account(room.as_ref()).await?)
            }
            "remaining_energy" => {
                let room = parse_optional_room(params)?;
                reply(&self.read().await?.remaining_energy(room.as_ref()).await?)
            }
            "recharge_energy" => {
                let RechargeEnergy {
                    payment_password,
                    amount,
                    room,
                } = parse(params)?;
                let room = room.parse::<RoomId>()?;
                self.write()
                    .await?
                    .recharge_energy(&payment_password, amount, &room)
                    .await?;
                done()
            }
            "recharge_config" => {
                let config = self.read().await?.recharge_config().await?;
                reply(&RechargeConfigReply::from(config))
            }
            "create_recharge_order" => {
                let CreateOrder { amount } = parse(params)?;
                let order = self.write().await?.create_recharge_order(amount).await?;
                reply(&OrderReply {
                    id: order.id(),
                    order: &order,
                })
            }
            "payment_channels" => {
                let WithOrder { order } = parse(params)?;
                let channels = self.read().await?.payment_channels(&order).await?;
                let channels: Vec<_> = channels.into_iter().map(Channel::from).collect();
                reply(&channels)
            }
            "prepare_payment" => {
                let PreparePayment {
                    order,
                    channel,
                    context,
                } = parse(params)?;
                let context = context.map(PaymentContext::from);
                let action = self
                    .read()
                    .await?
                    .prepare_payment(&order, &channel.into(), context.as_ref())
                    .await?;
                reply(&PaymentActionReply::try_from(action)?)
            }
            _ => Err(FfiError::UnknownMethod(method.into())),
        }
    }
}

/// 校验宿舍房间号（格式为 `area-building--level-room`；当前线程同步完成，不发起网络请求，无需客户端）。
///
/// `room` 为 UTF-8 房间号文本（无需 `NUL` 结尾）。返回 `ZZU_OK` 时 `*out_payload` 为 JSON 字符串形式的规范房间号，否则为错误 JSON。
///
/// # Safety
///
/// - `room` 在 `room_len` 非零时必须指向调用期间可读的 `room_len` 字节。
/// - `out_payload` 必须非空、对齐且独占可写（`NULL` 时返回 `ZZU_INVALID_INPUT`）；写入的缓冲区须通过 `zzu_buffer_free` 释放。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_room_id_parse(
    room: *const u8,
    room_len: usize,
    out_payload: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented pointer contracts.
    unsafe {
        compute(out_payload, || {
            let room = std::str::from_utf8(bytes(room, room_len)?)
                .map_err(|_| FfiError::InvalidInput("房间号必须为 UTF-8".into()))?;
            reply(&room.parse::<RoomId>()?)
        })
    }
}

/// 拼接指定支付渠道的收银台 URL（当前线程同步完成，不发起网络请求，无需客户端）。
///
/// `params` 为 UTF-8 JSON 对象 `{"order": ..., "channel": ...}`，其中 `order` 原样传入 `create_recharge_order` 的返回值。
/// 返回 `ZZU_OK` 时 `*out_payload` 为 JSON 字符串形式的 URL，否则为错误 JSON。
///
/// # Safety
///
/// - `params` 在 `params_len` 非零时必须指向调用期间可读的 `params_len` 字节。
/// - `out_payload` 必须非空、对齐且独占可写（`NULL` 时返回 `ZZU_INVALID_INPUT`）；写入的缓冲区须通过 `zzu_buffer_free` 释放。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_recharge_order_checkout_url_for(
    params: *const u8,
    params_len: usize,
    out_payload: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented pointer contracts.
    unsafe {
        compute(out_payload, || {
            let CheckoutUrl { order, channel } = parse(bytes(params, params_len)?)?;
            reply(order.checkout_url_for(&channel.into())?.as_str())
        })
    }
}

/// 使用 `ZzuCasClient` 创建校园卡与电控客户端（本地完成，不发起网络请求）。
///
/// 新句柄与 `cas` 共享认证会话、运行时和时限；创建后释放 `cas` 不影响它。
///
/// # Safety
///
/// - `cas` 必须为 `NULL` 或指向尚未释放的有效句柄（`NULL` 时返回 `ZZU_INVALID_INPUT`）。
/// - `out_ecard` 必须非空、对齐且独占可写；成功时写入的句柄须通过 `zzu_ecard_free` 释放。
/// - `out_error` 可以为 `NULL`；若非空则必须对齐且独占可写，失败时写入的错误 JSON 须通过 `zzu_buffer_free` 释放。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_ecard_new(
    cas: *const ZzuCasClient,
    out_ecard: *mut *mut ZzuEcardClient,
    out_error: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented input and output pointer contracts.
    unsafe {
        create(out_ecard, out_error, || {
            let cas = cas
                .as_ref()
                .ok_or_else(|| FfiError::InvalidInput("CAS 句柄为空".into()))?;
            let http = cas.0.trust_roots().client()?;
            let ecard = EcardClient::with_http_client(cas.0.service().clone(), http);
            Ok(ZzuEcardClient(cas.0.sibling(Guarded::new(ecard))))
        })
    }
}

/// 释放校园卡客户端句柄（传入 `NULL` 时无操作），不会阻塞，也可以在回调中调用。
///
/// 在途调用不受影响，其回调仍会照常触发。
///
/// # Safety
///
/// `ecard` 必须为 `NULL`，或由 `zzu_ecard_new` 返回且尚未释放的句柄；释放后不得再使用。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_ecard_free(ecard: *mut ZzuEcardClient) {
    // SAFETY: The caller returns this owned handle exactly once.
    unsafe { discard(ecard) }
}

/// 异步调用校园卡方法（如 `"balance"`），调用与回调规则同 `zzu_cas_call`。
///
/// # Safety
///
/// `ecard` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call`。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_ecard_call(
    ecard: *const ZzuEcardClient,
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
            ecard.as_ref().map(|ecard| &ecard.0),
            method,
            params,
            params_len,
            callback,
            user_data,
            out_call,
        )
    }
}

/// 同步调用校园卡方法，规则同 `zzu_cas_call_blocking`。
///
/// # Safety
///
/// `ecard` 必须为 `NULL` 或指向尚未释放的有效句柄，其余参数的要求同 `zzu_cas_call_blocking`。
// SAFETY: The symbol belongs to this library's zzu_ namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn zzu_ecard_call_blocking(
    ecard: *const ZzuEcardClient,
    method: *const c_char,
    params: *const u8,
    params_len: usize,
    out_payload: *mut *mut ZzuBuffer,
) -> ZzuStatus {
    // SAFETY: The caller upholds the documented pointer contracts.
    unsafe {
        call_blocking(
            ecard.as_ref().map(|ecard| &ecard.0),
            method,
            params,
            params_len,
            out_payload,
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn payment_form_preserves_duplicate_fields_and_order() {
        let action = PaymentAction::SubmitForm {
            url: "https://example.test/pay".parse().unwrap(),
            fields: vec![
                ("签名".into(), "a+b&c=1".into()),
                ("签名".into(), "second".into()),
            ],
        };
        assert_eq!(
            serde_json::to_value(PaymentActionReply::try_from(action).unwrap()).unwrap(),
            json!({
                "kind": "submit_form",
                "url": "https://example.test/pay",
                "fields": [
                    {"name": "签名", "value": "a+b&c=1"},
                    {"name": "签名", "value": "second"},
                ],
            })
        );
    }
}
