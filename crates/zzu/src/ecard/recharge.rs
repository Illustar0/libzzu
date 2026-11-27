use std::{collections::BTreeMap, fmt, net::IpAddr, sync::Arc};

use jiff::Timestamp;
use reqwest::Url;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::{ECARD_URL, EcardClient};
use crate::{Error, Result, http};

const CASHIER_URL: &str = "https://payment.v.zzu.edu.cn/WapCashDesk";

/// 校园卡充值面额与网页收银台配置。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RechargeConfig {
    /// 服务端预设的可选充值面额列表（单位：元）。
    pub amounts: Vec<u32>,
    /// 服务端是否开放网页收银台充值。
    pub is_browser_payment_enabled: bool,
}

/// 已创建的校园卡网页充值订单。
///
/// 支持序列化保存并原样恢复，便于跳转外部收银台后在新进程中继续支付。
/// 序列化结果包含支付凭据，切勿写入日志（[`fmt::Debug`] 实现已隐藏敏感字段）。
#[derive(Clone, Eq, PartialEq, Deserialize)]
#[serde(try_from = "RawRechargeOrder")]
pub struct RechargeOrder {
    amount: u32,
    payment: WapPayment,
    checkout_url: Url,
}

impl Serialize for RechargeOrder {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("RechargeOrder", 4)?;
        state.serialize_field("id", self.id())?;
        state.serialize_field("amount", &self.amount)?;
        state.serialize_field("payment", &self.payment)?;
        state.serialize_field("checkoutUrl", &self.checkout_url)?;
        state.end()
    }
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
    /// - `channel.id` 为空字符串时返回 [`Error::InvalidInput`]。
    /// - 订单参数序列化或收银台 URL 校验失败时返回 [`Error::Decode`]。
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
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentChannel {
    /// 支付渠道编码。
    pub id: String,
    /// 支付渠道显示名称。
    pub name: String,
    /// 渠道关联的收款账户标识。
    #[serde(default)]
    pub account_id: Option<String>,
}

#[derive(Deserialize)]
struct RawPaymentChannel {
    #[serde(rename = "paywayid")]
    id: String,
    #[serde(rename = "paywayname")]
    name: String,
    #[serde(rename = "payAccountid", default)]
    account_id: Option<String>,
}

impl From<RawPaymentChannel> for PaymentChannel {
    fn from(raw: RawPaymentChannel) -> Self {
        Self {
            id: raw.id,
            name: raw.name,
            account_id: raw.account_id,
        }
    }
}

/// 向收银台预下单时上报的客户端设备平台。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentDevice {
    /// Android 平台（默认值）。
    #[default]
    Android,
    /// iOS 平台。
    Ios,
}

/// 预下单时上报的付款端环境信息，默认为 [`PaymentDevice::Android`] 与空 IP。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PaymentContext {
    /// 付款端设备平台。
    #[serde(default, deserialize_with = "deserialize_payment_device")]
    pub device: PaymentDevice,
    /// 付款端 IP 地址；为 `None` 时按空字符串上报。
    #[serde(default)]
    pub payer_ip: Option<IpAddr>,
}

fn deserialize_payment_device<'de, D>(
    deserializer: D,
) -> std::result::Result<PaymentDevice, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<PaymentDevice>::deserialize(deserializer)?.unwrap_or_default())
}

/// 收银台预下单后返回的第三方支付拉起动作。
#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", try_from = "RawPaymentAction")]
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
        #[serde(serialize_with = "serialize_form_fields")]
        fields: Vec<(String, String)>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RawPaymentAction {
    Redirect {
        url: String,
        #[serde(default)]
        referrer: Option<String>,
    },
    SubmitForm {
        url: String,
        fields: Vec<RawFormField>,
    },
}

#[derive(Deserialize, Serialize)]
struct RawFormField {
    name: String,
    value: String,
}

impl TryFrom<RawPaymentAction> for PaymentAction {
    type Error = Error;

    fn try_from(raw: RawPaymentAction) -> Result<Self> {
        match raw {
            RawPaymentAction::Redirect { url, referrer } => Ok(Self::Redirect {
                url: web_url(&url)?,
                referrer: referrer.as_deref().map(web_url).transpose()?,
            }),
            RawPaymentAction::SubmitForm { url, fields } => Ok(Self::SubmitForm {
                url: web_url(&url)?,
                fields: fields
                    .into_iter()
                    .map(|field| (field.name, field.value))
                    .collect(),
            }),
        }
    }
}

fn serialize_form_fields<S: serde::Serializer>(
    fields: &[(String, String)],
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let mut seq = serializer.serialize_seq(Some(fields.len()))?;
    for (name, value) in fields {
        seq.serialize_element(&RawFormField {
            name: name.clone(),
            value: value.clone(),
        })?;
    }
    seq.end()
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
    /// - 客户端尚未调用 [`Self::login`] 或底层 CAS 令牌不可用时返回 [`Error::Authentication`]。
    /// - 网络传输失败、远端返回业务错误或面额字段解析失败时返回错误。
    pub async fn recharge_config(&self) -> Result<RechargeConfig> {
        let (config, _) = self.fetch_recharge_config().await?;
        Ok(config)
    }

    async fn fetch_recharge_config(&self) -> Result<(RechargeConfig, String)> {
        let token = self.access_token().await?;
        let response = http::json(
            self.http
                .post(format!("{ECARD_URL}/recharge/config"))
                .header("Authorization", token),
        )
        .await?;
        parse_recharge_config_response(response)
    }

    /// 创建指定整元金额 `amount` 的校园卡网页充值订单。
    ///
    /// # Errors
    ///
    /// - `amount == 0` 时返回 [`Error::InvalidInput`]。
    /// - 校园卡服务未开放网页收银台充值时返回 [`Error::Remote`]。
    /// - 客户端尚未登录、网络请求失败，或返回的订单流水号与请求金额不一致时返回错误。
    pub async fn create_recharge_order(&mut self, amount: u32) -> Result<RechargeOrder> {
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
        let (config, pay_code) = self.fetch_recharge_config().await?;
        if !config.is_browser_payment_enabled {
            return Err(Error::remote("校园卡服务未开放网页充值".into()));
        }
        let token = self.access_token().await?;
        let transaction_id = self.transaction_id(&token).await?;
        let recharge = Recharge {
            pay_code: &pay_code,
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
    /// 直接访问学校收银台接口，无需校园卡客户端登录。
    ///
    /// # Errors
    ///
    /// 网络传输失败、收银台返回业务错误或渠道列表解析失败时返回错误。
    pub async fn payment_channels(&self, order: &RechargeOrder) -> Result<Vec<PaymentChannel>> {
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
        parse_payment_channels_response(response)
    }

    /// 向学校收银台提交预支付请求，提取拉起第三方支付所需的跳转链接或 `POST` 表单。
    ///
    /// `context` 传 `None` 时默认按 [`PaymentDevice::Android`] 与空 IP 上报。本方法无需校园卡客户端登录。
    ///
    /// # Errors
    ///
    /// - `channel.id` 为空字符串时返回 [`Error::InvalidInput`]。
    /// - 网络传输失败、收银台返回业务错误，或预支付响应中的跳转地址与表单不符合无凭据 HTTPS 规范时返回错误。
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
        validate_order_fields(Some(&self.partner_journo), amount, &self.wap_pay_info)?;
        let checkout_url = self.wap_pay_info.checkout_url(None)?;
        Ok(RechargeOrder {
            amount,
            payment: self.wap_pay_info,
            checkout_url,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRechargeOrder {
    #[serde(default)]
    id: Option<String>,
    amount: u32,
    payment: WapPayment,
    checkout_url: String,
}

impl TryFrom<RawRechargeOrder> for RechargeOrder {
    type Error = Error;

    fn try_from(raw: RawRechargeOrder) -> Result<Self> {
        validate_order_fields(raw.id.as_deref(), raw.amount, &raw.payment)?;
        let checkout_url = web_url(&raw.checkout_url)?;
        Ok(Self {
            amount: raw.amount,
            payment: raw.payment,
            checkout_url,
        })
    }
}

fn validate_order_fields(
    expected_id: Option<&str>,
    amount: u32,
    payment: &WapPayment,
) -> Result<()> {
    let info = &payment.order_info;
    if amount == 0
        || expected_id.is_some_and(|id| id != info.order_id)
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
    Ok(())
}

#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
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

#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
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

fn parse_recharge_config_response(
    response: http::JsonResponse,
) -> Result<(RechargeConfig, String)> {
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
    let context = response.context.clone();
    let config: Config = http::ecard_data(response)?;
    let pay_code = config.wap_pay_code.trim().to_owned();
    if config.wap_pay_enable && pay_code.is_empty() {
        return Err(Error::decode_message("充值配置缺少支付代码".into()).with_request(context));
    }
    let amounts = config
        .amt_group_list
        .into_iter()
        .map(|amount| amount.amt.parse::<u32>().map_err(Error::decode))
        .collect::<Result<_>>()
        .map_err(|error| error.with_request(context))?;
    Ok((
        RechargeConfig {
            amounts,
            is_browser_payment_enabled: config.wap_pay_enable,
        },
        pay_code,
    ))
}

fn parse_payment_channels_response(response: http::JsonResponse) -> Result<Vec<PaymentChannel>> {
    #[derive(Deserialize)]
    struct Channels {
        payways: Vec<RawPaymentChannel>,
    }
    let response: Channels = cashier_data(response)?;
    Ok(response
        .payways
        .into_iter()
        .map(PaymentChannel::from)
        .collect())
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
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .ok_or_else(|| {
            Error::decode_message("收银台响应缺少 returncode".into())
                .with_request(response.context.clone())
        })?;
    if code != "SUCCESS" {
        let code = code.to_owned();
        Arc::make_mut(&mut response.context).error_code = Some(code);
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
        let channels = parse_payment_channels_response(http::JsonResponse::new(
            json!({
                "returncode": "SUCCESS",
                "payways": [
                    {"paywayid": "0106", "paywayname": "支付宝", "payAccountid": null},
                    {"paywayid": "0201", "paywayname": "微信支付", "payAccountid": "acc-1"}
                ]
            }),
            Arc::new(crate::RequestContext {
                method: "POST".into(),
                url: format!("{CASHIER_URL}/payWays"),
                status_code: Some(200),
                error_code: None,
                response_body: None,
            }),
        ))
        .unwrap();
        let channel = &channels[0];
        let encoded_channel = serde_json::to_value(channel).unwrap();
        assert_eq!(
            encoded_channel,
            json!({"id": "0106", "name": "支付宝", "accountId": null})
        );
        let restored_channel: PaymentChannel = serde_json::from_value(encoded_channel).unwrap();
        assert_eq!(&restored_channel, channel);
        assert_eq!(channels[1].account_id.as_deref(), Some("acc-1"));
        assert!(
            serde_json::from_value::<PaymentChannel>(json!({
                "paywayid": "0106",
                "paywayname": "支付宝"
            }))
            .is_err()
        );
        let selected = order.checkout_url_for(channel).unwrap();
        assert!(
            selected
                .query_pairs()
                .any(|(key, value)| key == "callPaywayid" && value == "0106")
        );
        assert!(!format!("{order:?}").contains("test-payment-secret"));
        let serialized_order = serde_json::to_value(&order).unwrap();
        assert_eq!(serialized_order["id"], "school-order");
        assert_eq!(serialized_order["amount"], 10);
        let restored: RechargeOrder = serde_json::from_value(serialized_order.clone()).unwrap();
        assert_eq!(restored, order);
        assert_eq!(
            restored.checkout_url_for(channel).unwrap(),
            order.checkout_url_for(channel).unwrap()
        );

        let mut tampered_amount = serialized_order.clone();
        tampered_amount["amount"] = json!(20);
        assert!(serde_json::from_value::<RechargeOrder>(tampered_amount).is_err());

        let mut tampered_id = serialized_order.clone();
        tampered_id["id"] = json!("other-order");
        assert!(serde_json::from_value::<RechargeOrder>(tampered_id).is_err());

        let mut insecure_url = serialized_order;
        insecure_url["checkoutUrl"] = json!("http://payment.v.zzu.edu.cn/pay");
        assert!(serde_json::from_value::<RechargeOrder>(insecure_url).is_err());

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
        let PaymentAction::SubmitForm { url, fields } = &action else {
            panic!("expected form")
        };
        assert_eq!(url.host_str(), Some("openapi.alipay.com"));
        assert_eq!(url.query(), Some("sign=test-signature"));
        assert_eq!(
            fields.as_slice(),
            [
                ("biz_content".into(), r#"{"subject":"A&B"}"#.into()),
                ("duplicate".into(), "first".into()),
                ("duplicate".into(), "second".into()),
            ]
        );
        let encoded_action = serde_json::to_value(&action).unwrap();
        assert_eq!(encoded_action["kind"], "submit_form");
        assert_eq!(
            encoded_action["fields"],
            json!([
                {"name": "biz_content", "value": r#"{"subject":"A&B"}"#},
                {"name": "duplicate", "value": "first"},
                {"name": "duplicate", "value": "second"},
            ])
        );
        let restored_action: PaymentAction = serde_json::from_value(encoded_action).unwrap();
        assert_eq!(restored_action, action);
        assert!(
            serde_json::from_value::<PaymentAction>(json!({
                "kind": "redirect",
                "url": "http://insecure.example/pay"
            }))
            .is_err()
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
        let PaymentAction::Redirect { url, referrer } = &action else {
            panic!("expected redirect")
        };
        assert_eq!(url.host_str(), Some("wx.tenpay.com"));
        assert_eq!(
            referrer.as_ref().unwrap().as_str(),
            "https://payment.v.zzu.edu.cn/"
        );
        let encoded_redirect = serde_json::to_value(&action).unwrap();
        assert_eq!(encoded_redirect["kind"], "redirect");
        let restored_redirect: PaymentAction = serde_json::from_value(encoded_redirect).unwrap();
        assert_eq!(restored_redirect, action);

        let context: PaymentContext = serde_json::from_value(json!({
            "device": "ios",
            "payerIp": "10.1.2.3"
        }))
        .unwrap();
        assert_eq!(context.device, PaymentDevice::Ios);
        let null_device: PaymentContext = serde_json::from_value(json!({"device": null})).unwrap();
        assert_eq!(null_device, PaymentContext::default());
        assert!(serde_json::from_value::<PaymentContext>(json!({"extra": 1})).is_err());
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
            payment_action(json!({"request_type": "1"}))
                .unwrap_err()
                .is_decode()
        );
    }

    #[test]
    fn recharge_config_parses_amounts_validates_pay_code_and_roundtrips() {
        let config_context = Arc::new(crate::RequestContext {
            method: "POST".into(),
            url: format!("{ECARD_URL}/recharge/config"),
            status_code: Some(200),
            error_code: None,
            response_body: None,
        });
        let missing_code = http::JsonResponse::new(
            json!({
                "success": true,
                "resultData": {
                    "amtGroupList": [{"amt": "10"}],
                    "wapPayEnable": true,
                    "wapPayCode": ""
                }
            }),
            config_context.clone(),
        );
        let err = parse_recharge_config_response(missing_code).unwrap_err();
        assert!(err.is_decode());
        assert!(err.request().is_some());

        let disabled_empty_code = http::JsonResponse::new(
            json!({
                "success": true,
                "resultData": {
                    "amtGroupList": [{"amt": "10"}],
                    "wapPayEnable": false,
                    "wapPayCode": ""
                }
            }),
            config_context.clone(),
        );
        let (config, pay_code) = parse_recharge_config_response(disabled_empty_code).unwrap();
        assert_eq!(
            config,
            RechargeConfig {
                amounts: vec![10],
                is_browser_payment_enabled: false,
            }
        );
        assert_eq!(pay_code, "");
        assert!(!format!("{config:?}").contains("RequestContext"));
        assert!(!format!("{config:?}").contains("pay_code"));

        let enabled_code = http::JsonResponse::new(
            json!({
                "success": true,
                "resultData": {
                    "amtGroupList": [{"amt": "20"}, {"amt": "50"}],
                    "wapPayEnable": true,
                    "wapPayCode": " 01 "
                }
            }),
            config_context,
        );
        let (enabled, code) = parse_recharge_config_response(enabled_code).unwrap();
        assert_eq!(code, "01");
        let encoded = serde_json::to_value(&enabled).unwrap();
        assert_eq!(
            encoded,
            json!({
                "amounts": [20, 50],
                "isBrowserPaymentEnabled": true,
            })
        );
        let restored: RechargeConfig = serde_json::from_value(encoded).unwrap();
        assert_eq!(restored, enabled);
    }
}
