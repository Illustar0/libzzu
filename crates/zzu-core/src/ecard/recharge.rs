use std::{collections::BTreeMap, fmt, net::IpAddr, sync::Arc};

use jiff::{Timestamp, tz::TimeZone};
use reqwest::Url;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::{ECARD_URL, EcardClient};
use crate::{Error, Result, http};

const CASHIER_URL: &str = "https://payment.v.zzu.edu.cn/WapCashDesk";

/// 校园卡充值面额与网页收银台配置。
#[derive(Clone, Debug)]
pub struct RechargeConfig {
    /// 服务端预设的可选充值面额列表（单位：元）。
    pub amounts: Vec<u32>,
    /// 服务端是否开放网页收银台充值。
    pub is_browser_payment_enabled: bool,
    pay_code: String,
}

/// 已创建的校园卡网页充值订单。
///
/// 支持序列化保存并原样恢复，便于跳转外部收银台后在新进程中继续支付。
/// 序列化结果包含支付凭据，切勿写入日志（[`fmt::Debug`] 实现已隐藏敏感字段）。
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RechargeOrder {
    amount: u32,
    payment: WapPayment,
    checkout_url: Url,
}

impl RechargeOrder {
    /// 返回学校收银台订单流水号。
    #[must_use]
    pub fn id(&self) -> &str {
        &self.payment.order_info.order_id
    }

    /// 返回订单充值金额（单位：元）。
    #[must_use]
    pub fn amount(&self) -> u32 {
        self.amount
    }

    /// 返回未预选支付渠道的学校网页收银台地址。
    #[must_use]
    pub fn checkout_url(&self) -> &Url {
        &self.checkout_url
    }

    /// 生成预选指定支付渠道 `channel` 的学校网页收银台地址。
    ///
    /// # Errors
    ///
    /// - 当 `channel.id` 为空字符串时返回 [`Error::InvalidInput`]。
    /// - 当订单参数序列化或收银台 URL 校验失败时返回 [`Error::Decode`]。
    pub fn checkout_url_for(&self, channel: &PaymentChannel) -> Result<Url> {
        validate_channel(channel)?;
        self.payment.checkout_url(Some(channel))
    }
}

impl fmt::Debug for RechargeOrder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RechargeOrder")
            .field("id", &self.id())
            .field("amount", &self.amount)
            .finish_non_exhaustive()
    }
}

/// 学校网页收银台支持的支付渠道（如支付宝、微信支付）。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct PaymentChannel {
    /// 支付渠道编码。
    #[serde(rename = "paywayid")]
    pub id: String,
    /// 支付渠道显示名称。
    #[serde(rename = "paywayname")]
    pub name: String,
    /// 渠道关联的收款账户标识。
    #[serde(rename = "payAccountid", default)]
    pub account_id: Option<String>,
}

/// 向收银台预下单时上报的客户端设备平台。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PaymentDevice {
    /// Android 平台（默认值）。
    #[default]
    Android,
    /// iOS 平台。
    Ios,
}

/// 预下单时上报的付款端环境信息，默认为 [`PaymentDevice::Android`] 与空 IP。
#[derive(Clone, Copy, Debug, Default)]
pub struct PaymentContext {
    /// 付款端设备平台。
    pub device: PaymentDevice,
    /// 付款端 IP 地址；为 `None` 时按空字符串上报。
    pub payer_ip: Option<IpAddr>,
}

/// 收银台预下单后返回的第三方支付拉起动作。
#[derive(Clone)]
#[non_exhaustive]
pub enum PaymentAction {
    /// 通过 HTTP `GET` 跳转至第三方支付页面（如微信 H5 支付）。
    Redirect {
        /// 第三方支付目标 HTTPS 地址。
        url: Url,
        /// 发起跳转请求时应携带的 `Referer` 请求头地址。
        referrer: Option<Url>,
    },
    /// 通过提交 HTML `POST` 表单进入第三方支付网关（如支付宝收银台）。
    SubmitForm {
        /// 表单提交目标 HTTPS 地址。
        url: Url,
        /// 按原始顺序排列的表单键值对；可能包含同名字段，提交时须保留重复项与顺序。
        fields: Vec<(String, String)>,
    },
}

impl fmt::Debug for PaymentAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, url, count) = match self {
            Self::Redirect { url, .. } => ("Redirect", url, 0),
            Self::SubmitForm { url, fields } => ("SubmitForm", url, fields.len()),
        };
        formatter
            .debug_struct(kind)
            .field("origin", &url.origin().ascii_serialization())
            .field("field_count", &count)
            .finish_non_exhaustive()
    }
}

impl EcardClient {
    /// 查询校园卡充值可选面额与网页收银台开放状态。
    ///
    /// # Errors
    ///
    /// - 当客户端尚未调用 [`Self::login`] 或底层 CAS 令牌不可用时返回 [`Error::Authentication`]。
    /// - 当网络传输失败、远端返回业务错误或面额字段解析失败时返回相应错误。
    pub async fn recharge_config(&self) -> Result<RechargeConfig> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Config {
            amt_group_list: Vec<Amount>,
            wap_pay_enable: bool,
            wap_pay_code: String,
        }
        #[derive(Deserialize)]
        struct Amount {
            amt: String,
        }
        let token = self.access_token().await?;
        let response = http::json(
            self.http
                .post(format!("{ECARD_URL}/recharge/config"))
                .header("Authorization", token),
        )
        .await?;
        let context = response.context.clone();
        let config: Config = http::ecard_data(response)?;
        let amounts = config
            .amt_group_list
            .into_iter()
            .map(|amount| amount.amt.parse::<u32>().map_err(Error::decode))
            .collect::<Result<_>>()
            .map_err(|error| error.with_request(context))?;
        Ok(RechargeConfig {
            amounts,
            is_browser_payment_enabled: config.wap_pay_enable,
            pay_code: config.wap_pay_code,
        })
    }

    /// 创建指定整元金额 `amount` 的校园卡网页充值订单。
    ///
    /// # Errors
    ///
    /// - 当 `amount == 0` 时返回 [`Error::InvalidInput`]。
    /// - 当校园卡服务未开放网页收银台充值时返回 [`Error::Remote`]。
    /// - 当客户端尚未登录、网络请求失败，或返回的订单流水号与请求金额不一致时返回相应错误。
    pub async fn create_recharge_order(&mut self, amount: u32) -> Result<RechargeOrder> {
        #[derive(Serialize)]
        struct Transaction {
            datetime: String,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Recharge<'a> {
            pay_code: &'a str,
            amt: String,
            return_url: String,
            timestamp: i64,
        }
        if amount == 0 {
            return Err(Error::invalid_input("充值金额必须大于 0 元".into()));
        }
        let config = self.recharge_config().await?;
        if !config.is_browser_payment_enabled {
            return Err(Error::remote("校园卡服务未开放网页充值".into()));
        }
        if config.pay_code.is_empty() {
            return Err(Error::decode_message("充值配置缺少支付代码".into()));
        }
        let token = self.access_token().await?;
        let now = Timestamp::now();
        let transaction = Transaction {
            datetime: now
                .to_zoned(TimeZone::get("Asia/Shanghai").map_err(Error::decode)?)
                .strftime("%F %T")
                .to_string(),
        };
        let response = http::json(
            self.http
                .post(format!("{ECARD_URL}/consume/transaction"))
                .header("Authorization", &token)
                .json(&transaction),
        )
        .await?;
        let transaction_id: String = http::ecard_data(response)?;
        if transaction_id.is_empty() {
            return Err(Error::decode_message(
                "充值请求的 transactionid 为空".into(),
            ));
        }
        let recharge = Recharge {
            pay_code: &config.pay_code,
            amt: amount.to_string(),
            return_url: format!(
                "https://ecard.v.zzu.edu.cn/#/pages_plugins/recharge/webSuccess?money={amount}"
            ),
            timestamp: Timestamp::now().as_millisecond(),
        };
        let response = http::json(
            self.http
                .post(format!("{ECARD_URL}/recharge/pay"))
                .header("Authorization", token)
                .header("transactionid", transaction_id)
                .json(&recharge),
        )
        .await?;
        let context = response.context.clone();
        let recharge: CreatedRecharge = http::ecard_data(response)?;
        recharge
            .into_order(amount)
            .map_err(|error| error.with_request(context))
    }

    /// 查询充值订单 `order` 在学校收银台可用的支付渠道列表。
    ///
    /// 本方法直接访问学校收银台接口，无需校园卡客户端处于已登录状态。
    ///
    /// # Errors
    ///
    /// 当网络传输失败、收银台返回非成功业务状态或渠道列表解析失败时返回相应错误。
    pub async fn payment_channels(&self, order: &RechargeOrder) -> Result<Vec<PaymentChannel>> {
        #[derive(Deserialize)]
        struct Channels {
            payways: Vec<PaymentChannel>,
        }
        let info = &order.payment.order_info;
        let response = http::json(
            self.http
                .post(format!("{CASHIER_URL}/payWays"))
                .json(&json!({
                    "partnerid": info.partner_id,
                    "merchantno": info.merchant_no,
                    "channelno": info.channel_no,
                })),
        )
        .await?;
        let response: Channels = cashier_data(response)?;
        Ok(response.payways)
    }

    /// 向学校收银台提交预支付请求，提取拉起第三方支付所需的跳转链接或 `POST` 表单。
    ///
    /// `context` 传 `None` 时默认按 [`PaymentDevice::Android`] 与空 IP 上报。本方法无需校园卡客户端处于已登录状态。
    ///
    /// # Errors
    ///
    /// - 当 `channel.id` 为空字符串时返回 [`Error::InvalidInput`]。
    /// - 当网络传输失败、收银台返回业务拒绝，或预支付响应中的跳转地址与表单不符合无凭据 HTTPS 规范时返回相应错误。
    pub async fn prepare_payment(
        &self,
        order: &RechargeOrder,
        channel: &PaymentChannel,
        context: Option<&PaymentContext>,
    ) -> Result<PaymentAction> {
        #[derive(Serialize)]
        struct Prepay<'a> {
            #[serde(rename = "merchantno")]
            merchant_no: &'a str,
            #[serde(rename = "journo")]
            order_id: &'a str,
            #[serde(rename = "channelno")]
            channel_no: &'a str,
            #[serde(rename = "paywayid")]
            payment_channel_id: &'a str,
            #[serde(rename = "accountid", skip_serializing_if = "Option::is_none")]
            account_id: Option<&'a str>,
            #[serde(rename = "senceno")]
            scene: String,
            device: &'a str,
            #[serde(rename = "ip")]
            payer_ip: String,
        }
        #[derive(Deserialize)]
        struct Prepared {
            paymsg: Value,
        }
        validate_channel(channel)?;
        let info = &order.payment.order_info;
        let context = context.copied().unwrap_or_default();
        let request = Prepay {
            merchant_no: &info.merchant_no,
            order_id: &info.order_id,
            channel_no: &info.channel_no,
            payment_channel_id: &channel.id,
            account_id: channel.account_id.as_deref().filter(|id| !id.is_empty()),
            scene: json!({"h5_info": {
                "type": "Wap",
                "wap_url": format!("{CASHIER_URL}/e-pay/result.html?hostInfo=null"),
                "wap_name": "wap收银台",
            }})
            .to_string(),
            device: match context.device {
                PaymentDevice::Android => "android",
                PaymentDevice::Ios => "ios",
            },
            payer_ip: context
                .payer_ip
                .map_or_else(String::new, |payer_ip| payer_ip.to_string()),
        };
        let response = http::json(
            self.http
                .post(format!("{CASHIER_URL}/prepayOrder"))
                .json(&request),
        )
        .await?;
        let context = response.context.clone();
        let prepared: Prepared = cashier_data(response)?;
        payment_action(prepared.paymsg).map_err(|error| error.with_request(context))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatedRecharge {
    partner_journo: String,
    wap_pay_info: WapPayment,
}

impl CreatedRecharge {
    fn into_order(self, amount: u32) -> Result<RechargeOrder> {
        let info = &self.wap_pay_info.order_info;
        if info.order_id != self.partner_journo
            || [
                &info.partner_id,
                &info.order_id,
                &info.merchant_no,
                &info.channel_no,
            ]
            .iter()
            .any(|field| field.is_empty())
            || info.amount_fen.parse::<u64>().ok() != Some(u64::from(amount) * 100)
        {
            return Err(Error::decode_message(
                "充值订单标识或金额与请求不一致".into(),
            ));
        }
        let checkout_url = self.wap_pay_info.checkout_url(None)?;
        Ok(RechargeOrder {
            amount,
            payment: self.wap_pay_info,
            checkout_url,
        })
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WapPayment {
    order_info: OrderInfo,
    #[serde(default, rename = "callPaywayid")]
    call_payway_id: String,
    #[serde(default, rename = "callAccountid")]
    call_account_id: String,
    #[serde(default)]
    extend: Option<Value>,
}

impl WapPayment {
    fn checkout_url(&self, channel: Option<&PaymentChannel>) -> Result<Url> {
        let mut info = serde_json::to_value(&self.order_info)?;
        if let Some(extend) = &self.extend {
            info["extend"] = extend.clone();
        }
        let selected_id = channel.map_or(self.call_payway_id.as_str(), |channel| &channel.id);
        let account_id = channel.map_or(self.call_account_id.as_str(), |channel| {
            channel.account_id.as_deref().unwrap_or_default()
        });
        let mut url = web_url(&format!("{CASHIER_URL}/e-pay/payways.html"))?;
        url.query_pairs_mut()
            .append_pair("orderInfo", &serde_json::to_string(&info)?)
            .append_pair("callAccountid", account_id);
        if !selected_id.is_empty() {
            url.query_pairs_mut()
                .append_pair("callPaywayid", selected_id);
        }
        Ok(url)
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct OrderInfo {
    #[serde(rename = "partnerid")]
    partner_id: String,
    #[serde(rename = "journo")]
    order_id: String,
    #[serde(rename = "merchantno")]
    merchant_no: String,
    #[serde(rename = "channelno")]
    channel_no: String,
    #[serde(rename = "txamt")]
    amount_fen: String,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

fn validate_channel(channel: &PaymentChannel) -> Result<()> {
    if channel.id.is_empty() {
        return Err(Error::invalid_input("支付渠道 ID 不能为空".into()));
    }
    Ok(())
}

fn cashier_data<T: DeserializeOwned>(mut response: http::JsonResponse) -> Result<T> {
    let code = response
        .get("returncode")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::decode_message("收银台响应缺少 returncode".into())
                .with_request(response.context.clone())
        })?;
    let code = code.to_owned();
    Arc::make_mut(&mut response.context).error_code = Some(code.clone());
    if code != "SUCCESS" {
        let message = response
            .get("returnmsg")
            .and_then(Value::as_str)
            .unwrap_or("收银台未提供错误消息");
        return Err(Error::remote(message.into()).with_request(response.context));
    }
    response.decode()
}

fn web_url(text: &str) -> Result<Url> {
    let url = Url::parse(text).map_err(Error::decode)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Error::decode_message(
            "支付地址必须是不含用户凭据的 HTTPS URL".into(),
        ));
    }
    Ok(url)
}

fn payment_action(value: Value) -> Result<PaymentAction> {
    #[derive(Deserialize)]
    struct Message {
        request_type: String,
        htmlpost: Option<String>,
        mweb_url: Option<String>,
    }
    let message: Message = match value {
        Value::String(text) => serde_json::from_str(&text)?,
        value => serde_json::from_value(value)?,
    };
    match message.request_type.as_str() {
        "2" => payment_form(
            message
                .htmlpost
                .as_deref()
                .ok_or_else(|| Error::decode_message("预支付响应缺少 htmlpost".into()))?,
        ),
        "1" => {
            let target = message.mweb_url.as_deref().ok_or_else(|| {
                Error::decode_message("无法识别预支付跳转结果，请使用学校收银台".into())
            })?;
            Ok(PaymentAction::Redirect {
                url: web_url(target)?,
                referrer: Some(web_url("https://payment.v.zzu.edu.cn/")?),
            })
        }
        _ => Err(Error::decode_message("无法识别收银台返回的支付动作".into())),
    }
}

fn payment_form(text: &str) -> Result<PaymentAction> {
    let document = Html::parse_document(text);
    let forms = Selector::parse("form").expect("valid form selector");
    let inputs = Selector::parse("input[name]:not([disabled])").expect("valid input selector");
    let mut matches = document.select(&forms);
    let form = matches
        .next()
        .ok_or_else(|| Error::decode_message("支付响应缺少表单".into()))?;
    if matches.next().is_some()
        || !form
            .value()
            .attr("method")
            .is_some_and(|method| method.eq_ignore_ascii_case("post"))
    {
        return Err(Error::decode_message(
            "支付响应必须包含唯一的 POST 表单".into(),
        ));
    }
    let url = web_url(
        form.value()
            .attr("action")
            .ok_or_else(|| Error::decode_message("支付表单缺少 action".into()))?,
    )?;
    let fields = form
        .select(&inputs)
        .filter_map(|input| {
            let input = input.value();
            match input
                .attr("type")
                .unwrap_or("text")
                .to_ascii_lowercase()
                .as_str()
            {
                "submit" | "button" | "reset" | "file" => return None,
                "checkbox" | "radio" if input.attr("checked").is_none() => return None,
                _ => {}
            }
            Some((
                input.attr("name")?.to_owned(),
                input.attr("value").unwrap_or_default().to_owned(),
            ))
        })
        .collect();
    Ok(PaymentAction::SubmitForm { url, fields })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_preserves_credentials_and_selected_channel_without_debug_leaks() {
        let payload = json!({
            "partnerJourno": "school-order",
            "wapPayInfo": {
                "orderInfo": {
                    "partnerid": "partner", "journo": "school-order",
                    "merchantno": "merchant", "channelno": "103", "txamt": "1000",
                    "goodsname": "校园卡充值", "payResultUrl": "https://example.test/result",
                },
                "callPaywayid": "", "callAccountid": "",
                "extend": {"source": "browser", "accessToken": "test-payment-secret"},
            },
        });
        let created: CreatedRecharge = serde_json::from_value(payload.clone()).unwrap();
        let order = created.into_order(10).unwrap();
        assert_eq!(order.id(), "school-order");
        assert_eq!(order.amount(), 10);
        assert_eq!(
            order.checkout_url().path(),
            "/WapCashDesk/e-pay/payways.html"
        );
        let query: BTreeMap<_, _> = order.checkout_url().query_pairs().into_owned().collect();
        let info: Value = serde_json::from_str(&query["orderInfo"]).unwrap();
        assert_eq!(info["extend"]["accessToken"], "test-payment-secret");
        assert_eq!(info["goodsname"], "校园卡充值");
        let channel: PaymentChannel = serde_json::from_value(json!({
            "paywayid": "0106", "paywayname": "支付宝", "payAccountid": null,
        }))
        .unwrap();
        let selected = order.checkout_url_for(&channel).unwrap();
        assert!(
            selected
                .query_pairs()
                .any(|(key, value)| key == "callPaywayid" && value == "0106")
        );
        assert!(!format!("{order:?}").contains("test-payment-secret"));
        let restored: RechargeOrder =
            serde_json::from_value(serde_json::to_value(&order).unwrap()).unwrap();
        assert_eq!(restored.id(), order.id());
        assert_eq!(restored.amount(), order.amount());
        assert_eq!(
            restored.checkout_url_for(&channel).unwrap(),
            order.checkout_url_for(&channel).unwrap()
        );
        let created: CreatedRecharge = serde_json::from_value(payload).unwrap();
        assert!(created.into_order(20).unwrap_err().is_decode());
    }

    #[test]
    fn alipay_form_preserves_signed_values_without_executing_scripts() {
        let action = payment_action(json!({
            "request_type": "2",
            "htmlpost": r#"<form method="post" action="https://openapi.alipay.com/gateway.do?sign=test-signature">
                <input type="hidden" name="biz_content" value="{&quot;subject&quot;:&quot;A&amp;B&quot;}">
                <input name="duplicate" value="first"><input name="duplicate" value="second">
                <input name="ignored" disabled value="hidden"><input type="submit" name="submit" value="pay">
                </form><script>document.forms[0].submit()</script>"#,
        })).unwrap();
        assert!(!format!("{action:?}").contains("test-signature"));
        let PaymentAction::SubmitForm { url, fields } = action else {
            panic!("expected form")
        };
        assert_eq!(url.host_str(), Some("openapi.alipay.com"));
        assert_eq!(url.query(), Some("sign=test-signature"));
        assert_eq!(
            fields,
            [
                ("biz_content".into(), r#"{"subject":"A&B"}"#.into()),
                ("duplicate".into(), "first".into()),
                ("duplicate".into(), "second".into()),
            ]
        );
        assert!(
            payment_form(r#"<form method="post" action="javascript:alert(1)"></form>"#).is_err()
        );
    }

    #[test]
    fn wechat_redirect_retains_merchant_referrer_and_rejects_unknown_actions() {
        let action = payment_action(Value::String(json!({
            "request_type": "1",
            "mweb_url": "https://wx.tenpay.com/cgi-bin/mmpayweb-bin/checkmweb?prepay_id=test-secret",
        }).to_string())).unwrap();
        assert!(!format!("{action:?}").contains("test-secret"));
        let PaymentAction::Redirect { url, referrer } = action else {
            panic!("expected redirect")
        };
        assert_eq!(url.host_str(), Some("wx.tenpay.com"));
        assert_eq!(referrer.unwrap().as_str(), "https://payment.v.zzu.edu.cn/");
        assert!(
            payment_action(json!({"request_type": "unknown"}))
                .unwrap_err()
                .is_decode()
        );
        assert!(
            payment_action(json!({"request_type": "2"}))
                .unwrap_err()
                .is_decode()
        );
        assert!(
            payment_action(json!({"request_type": "1", "request_content": "unknown"}))
                .unwrap_err()
                .is_decode()
        );
    }
}
