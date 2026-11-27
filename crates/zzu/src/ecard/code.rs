use std::fmt;

use futures_util::future::try_join;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{ECARD_URL, EcardClient};
use crate::{Decimal, Error, Result, http};

const DEFAULT_QR_REFRESH_SECONDS: u32 = 60;

/// 在线校园码（付款码与通行码）载荷及自动刷新参数。
///
/// `code` 为实时有效的支付与门禁凭据，切勿写入日志（[`fmt::Debug`] 实现已隐藏该字段）。
#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PaymentCode {
    /// 用于编码生成二维码图像的动态码原始字符串（含支付凭据，严禁写入日志）。
    pub code: String,
    /// 生成动态码时刻的校园卡账户余额（单位：元）。
    #[serde(deserialize_with = "crate::money::deserialize_decimal")]
    pub balance: Decimal,
    /// 服务端建议的动态码自动刷新间隔（单位：秒）。
    pub refresh_seconds: u32,
}

impl fmt::Debug for PaymentCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PaymentCode")
            .field("balance", &self.balance)
            .field("refresh_seconds", &self.refresh_seconds)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawOnlineQrcode {
    qrcode: String,
    #[serde(deserialize_with = "crate::money::deserialize_decimal")]
    balance: Decimal,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawVirtualCardConfig {
    #[serde(default)]
    disable_online: bool,
    #[serde(default)]
    refresh_second: Option<u32>,
}

impl EcardClient {
    /// 获取当前账号的在线校园码（动态付款码）及刷新周期。
    ///
    /// 首次调用时并发查询动态码与虚拟卡配置并缓存刷新秒数，后续刷新仅请求动态码接口。
    /// 返回的 [`PaymentCode::code`] 携带实时支付凭据，切勿写入日志。
    ///
    /// # Errors
    ///
    /// - 客户端尚未登录或底层 CAS 令牌不可用时返回 [`Error::Authentication`]。
    /// - 服务端关闭在线校园码功能时返回 [`Error::Remote`]。
    /// - 会话换新、网络传输、远端业务校验或响应字段解析失败时返回错误。
    pub async fn payment_code(&self) -> Result<PaymentCode> {
        let token = self.access_token().await?;
        let cached_refresh = self.lock_state().qr_refresh_seconds;
        let ((code, balance), refresh_seconds) = if let Some(refresh_seconds) = cached_refresh {
            (self.fetch_online_qrcode(&token).await?, refresh_seconds)
        } else {
            let (qrcode, refresh_seconds) = try_join(
                self.fetch_online_qrcode(&token),
                self.fetch_qr_refresh_seconds(&token),
            )
            .await?;
            self.lock_state().qr_refresh_seconds = Some(refresh_seconds);
            (qrcode, refresh_seconds)
        };
        Ok(PaymentCode {
            code,
            balance,
            refresh_seconds,
        })
    }

    async fn fetch_online_qrcode(&self, token: &str) -> Result<(String, Decimal)> {
        let response = http::json(
            self.http
                .post(format!("{ECARD_URL}/virtualCard/qrcodeOnline"))
                .header("Authorization", token)
                .json(&json!({})),
        )
        .await?;
        parse_online_qrcode(response)
    }

    async fn fetch_qr_refresh_seconds(&self, token: &str) -> Result<u32> {
        let response = http::json(
            self.http
                .post(format!("{ECARD_URL}/virtualCard/config"))
                .header("Authorization", token)
                .json(&json!({})),
        )
        .await?;
        parse_virtual_card_config(response)
    }
}

fn parse_online_qrcode(response: http::JsonResponse) -> Result<(String, Decimal)> {
    let context = response.context.clone();
    let raw: RawOnlineQrcode = http::ecard_data(response)?;
    let code = raw.qrcode.trim().to_owned();
    if code.is_empty() {
        return Err(Error::decode_message("在线校园码 qrcode 为空".into()).with_request(context));
    }
    Ok((code, raw.balance))
}

fn parse_virtual_card_config(response: http::JsonResponse) -> Result<u32> {
    let context = response.context.clone();
    let config: RawVirtualCardConfig = http::ecard_data(response)?;
    if config.disable_online {
        return Err(Error::remote("服务端未开放在线校园码".into()).with_request(context));
    }
    Ok(config
        .refresh_second
        .filter(|&seconds| seconds > 0)
        .unwrap_or(DEFAULT_QR_REFRESH_SECONDS))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::Value;

    use super::*;
    use crate::{RequestContext, auth::CasClient};

    fn ecard_json_response(path: &str, value: Value) -> http::JsonResponse {
        http::JsonResponse::new(
            value,
            Arc::new(RequestContext {
                method: "POST".into(),
                url: format!("{ECARD_URL}{path}"),
                status_code: Some(200),
                error_code: None,
                response_body: None,
            }),
        )
    }

    #[test]
    fn payment_code_parses_qrcode_and_config_while_redacting_debug() {
        let (code, balance) = parse_online_qrcode(ecard_json_response(
            "/virtualCard/qrcodeOnline",
            json!({
                "success": true,
                "message": "成功",
                "resultData": {
                    "qrcode": "51613F523E07503266E7B62241979759",
                    "balance": "12.34",
                    "idSerial": "202400000001",
                    "userName": "张三"
                },
                "code": ""
            }),
        ))
        .unwrap();
        let refresh_seconds = parse_virtual_card_config(ecard_json_response(
            "/virtualCard/config",
            json!({
                "success": true,
                "resultData": {
                    "disableOnline": false,
                    "refreshSecond": 45
                }
            }),
        ))
        .unwrap();

        let payment_code = PaymentCode {
            code,
            balance,
            refresh_seconds,
        };
        assert_eq!(payment_code.code, "51613F523E07503266E7B62241979759");
        assert_eq!(payment_code.balance.to_string(), "12.34");
        assert_eq!(payment_code.refresh_seconds, 45);
        assert!(!format!("{payment_code:?}").contains("51613F523E07503266E7B62241979759"));

        let serialized = serde_json::to_value(&payment_code).unwrap();
        assert_eq!(
            serialized,
            json!({
                "code": "51613F523E07503266E7B62241979759",
                "balance": "12.34",
                "refreshSeconds": 45,
            })
        );
        let roundtripped: PaymentCode = serde_json::from_value(serialized).unwrap();
        assert_eq!(roundtripped, payment_code);

        let fallback_seconds = parse_virtual_card_config(ecard_json_response(
            "/virtualCard/config",
            json!({
                "success": true,
                "resultData": {"disableOnline": false, "refreshSecond": 0}
            }),
        ))
        .unwrap();
        assert_eq!(fallback_seconds, DEFAULT_QR_REFRESH_SECONDS);
    }

    #[test]
    fn payment_code_rejects_disabled_empty_and_remote_failures() {
        let disabled = parse_virtual_card_config(ecard_json_response(
            "/virtualCard/config",
            json!({
                "success": true,
                "resultData": {"disableOnline": true, "refreshSecond": 60}
            }),
        ))
        .unwrap_err();
        assert!(disabled.is_remote());
        assert!(disabled.request().is_some());

        let empty_code = parse_online_qrcode(ecard_json_response(
            "/virtualCard/qrcodeOnline",
            json!({
                "success": true,
                "resultData": {"qrcode": "   ", "balance": "0.00"}
            }),
        ))
        .unwrap_err();
        assert!(empty_code.is_decode());
        assert!(empty_code.request().is_some());

        let remote_err = parse_online_qrcode(ecard_json_response(
            "/virtualCard/qrcodeOnline",
            json!({
                "success": false,
                "message": "卡片已挂失",
                "resultData": null
            }),
        ))
        .unwrap_err();
        assert!(remote_err.is_remote());
        assert_eq!(remote_err.message(), "卡片已挂失");
    }

    #[tokio::test]
    async fn unauthenticated_payment_code_is_rejected() {
        let cas = CasClient::new("", "", "test-device").unwrap();
        let ecard = EcardClient::new(cas).unwrap();
        assert!(
            ecard
                .payment_code()
                .await
                .unwrap_err()
                .is_not_authenticated()
        );
    }
}
