use std::{future::Future, ops::Deref, sync::Arc};

use reqwest::{
    Certificate, Client, ClientBuilder, RequestBuilder, StatusCode, Url, header::HeaderMap,
    redirect::Policy,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{Error, RequestContext, Result};

/// HTTP 客户端校验服务端证书时信任的根证书，默认使用平台校验器。
///
/// Android 上须调用 [`Self::from_pem_bundle`] 提供系统根证书：`rustls-platform-verifier` 的
/// Android 实现会在工作线程上同步下载并用 Java 解析吊销列表，遇到大体积 CRL 时单次握手耗时可达
/// 数十秒，阻塞所有工作线程后连超时也无法触发。
#[derive(Clone, Default)]
pub struct TrustRoots {
    certificates: Option<Arc<[Certificate]>>,
}

impl TrustRoots {
    /// 只信任 `pem` 中的根证书。
    ///
    /// # Errors
    ///
    /// `pem` 无法解析或不含任何证书时返回错误。
    pub fn from_pem_bundle(pem: &[u8]) -> Result<Self> {
        let certificates = Certificate::from_pem_bundle(pem)?;
        if certificates.is_empty() {
            return Err(Error::invalid_input("根证书列表为空".into()));
        }
        Ok(Self {
            certificates: Some(certificates.into()),
        })
    }

    /// 创建仅配置证书校验的 HTTP 客户端构建器。
    pub fn tls_client_builder(&self) -> ClientBuilder {
        match &self.certificates {
            Some(certificates) => Client::builder().tls_certs_only(certificates.iter().cloned()),
            None => Client::builder(),
        }
    }

    /// 创建按校园服务协议配置的 HTTP 客户端构建器。
    ///
    /// 默认启用 Cookie 存储，禁用自动重定向、自动重试与隐式超时，供协议层直接读取认证跳转头与原始状态码。
    pub fn client_builder(&self) -> ClientBuilder {
        self.tls_client_builder()
            .cookie_store(true)
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
    }

    /// 创建按校园服务协议配置的 HTTP 客户端。
    ///
    /// # Errors
    ///
    /// HTTP 客户端初始化失败时返回错误。
    pub fn client(&self) -> Result<Client> {
        Ok(self.client_builder().build()?)
    }
}

pub(crate) fn client() -> Result<Client> {
    TrustRoots::default().client()
}

/// 创建使用平台校验器并按校园服务协议配置的 HTTP 客户端构建器，详见 [`TrustRoots::client_builder`]。
pub fn client_builder() -> ClientBuilder {
    TrustRoots::default().client_builder()
}

pub(crate) struct Response {
    pub headers: HeaderMap,
    pub url: Url,
    pub context: Arc<RequestContext>,
}

pub(crate) async fn send(builder: RequestBuilder) -> Result<Response> {
    send_with_text_decoder(builder, reqwest::Response::text).await
}

pub(crate) async fn send_with_text_decoder<DecodeFuture>(
    builder: RequestBuilder,
    decode_body: impl FnOnce(reqwest::Response) -> DecodeFuture,
) -> Result<Response>
where
    DecodeFuture: Future<Output = reqwest::Result<String>>,
{
    let (client, request) = builder.build_split();
    let request = request?;
    let mut url = request.url().clone();
    url.set_query(None);
    url.set_fragment(None);
    let _ = url.set_username("");
    let _ = url.set_password(None);
    let mut context = RequestContext {
        method: request.method().to_string(),
        url: url.to_string(),
        status_code: None,
        error_code: None,
        response_body: None,
    };
    let response = client
        .execute(request)
        .await
        .map_err(|error| Error::from(error).with_request(Arc::new(context.clone())))?;
    context.status_code = Some(response.status().as_u16());
    let headers = response.headers().clone();
    let url = response.url().clone();
    let body = decode_body(response)
        .await
        .map_err(|error| Error::from(error).with_request(Arc::new(context.clone())))?;
    context.error_code = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| value.get("code").map(value_code));
    context.response_body = Some(body);
    Ok(Response {
        headers,
        url,
        context: Arc::new(context),
    })
}

impl Response {
    pub fn body(&self) -> &str {
        self.context.response_body.as_deref().unwrap_or_default()
    }

    pub fn require_success(self) -> Result<Self> {
        let status = StatusCode::from_u16(self.context.status_code.unwrap_or(0))
            .map_err(|_| Error::decode_message("HTTP 状态无效".into()))?;
        if status.is_success() {
            return Ok(self);
        }
        let detail = serde_json::from_str::<Value>(self.body())
            .ok()
            .and_then(|value| response_message(&value))
            .unwrap_or_else(|| {
                status
                    .canonical_reason()
                    .unwrap_or("服务器返回错误状态")
                    .to_owned()
            });
        Err(Error::http(detail).with_request(self.context))
    }

    pub fn allow_redirect(self) -> Result<Self> {
        if self
            .context
            .status_code
            .is_some_and(|status| (300..400).contains(&status))
        {
            return Ok(self);
        }
        self.require_success()
    }

    pub fn decode<T: DeserializeOwned>(self) -> Result<T> {
        serde_json::from_str(self.body())
            .map_err(|error| Error::from(error).with_request(self.context))
    }
}

pub(crate) struct JsonResponse {
    value: Value,
    pub context: Arc<RequestContext>,
}

impl Deref for JsonResponse {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.value
    }
}

impl JsonResponse {
    pub fn decode<T: DeserializeOwned>(self) -> Result<T> {
        serde_json::from_value(self.value)
            .map_err(|error| Error::from(error).with_request(self.context))
    }
}

pub(crate) async fn json(request: RequestBuilder) -> Result<JsonResponse> {
    let response = send(request).await?.require_success()?;
    let context = response.context.clone();
    Ok(JsonResponse {
        value: response.decode()?,
        context,
    })
}

pub(crate) fn data<T: DeserializeOwned>(response: JsonResponse, status_key: &str) -> Result<T> {
    check_status(&response, status_key)?;
    decode_field(response, "data")
}

pub(crate) fn check_status(response: &JsonResponse, status_key: &str) -> Result<()> {
    check_value_status(&response.value, status_key).map_err(|error| {
        let mut context = (*response.context).clone();
        context.error_code = response.value.get(status_key).map(value_code);
        error.with_request(Arc::new(context))
    })
}

fn check_value_status(value: &Value, status_key: &str) -> Result<()> {
    let status = value
        .get(status_key)
        .ok_or_else(|| Error::decode_message(format!("服务器响应缺少 {status_key}")))?;
    let status = status
        .as_i64()
        .ok_or_else(|| Error::decode_message(format!("{status_key} 必须为整数")))?;
    if status != 0 {
        return Err(Error::remote(message(value)));
    }
    Ok(())
}

pub(crate) fn ecard_data<T: DeserializeOwned>(response: JsonResponse) -> Result<T> {
    match response.get("success").and_then(Value::as_bool) {
        Some(true) => decode_field(response, "resultData"),
        Some(false) => Err(Error::remote(message(&response)).with_request(response.context)),
        None => Err(
            Error::decode_message("校园卡响应缺少有效的 success 字段".into())
                .with_request(response.context),
        ),
    }
}

fn decode_field<T: DeserializeOwned>(mut response: JsonResponse, key: &str) -> Result<T> {
    let field = response.value.get_mut(key).ok_or_else(|| {
        Error::decode_message(format!("服务器响应缺少 {key}"))
            .with_request(response.context.clone())
    })?;
    serde_json::from_value(field.take())
        .map_err(|error| Error::from(error).with_request(response.context))
}

pub(crate) fn message(value: &Value) -> String {
    response_message(value).unwrap_or_else(|| "服务器未提供错误消息".into())
}

fn response_message(value: &Value) -> Option<String> {
    ["message", "msg", "error"].into_iter().find_map(|key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    })
}

fn value_code(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

pub(crate) fn parse_money(value: &Value, field: &str) -> Result<crate::Decimal> {
    crate::money::deserialize_decimal(value)
        .map_err(|_| Error::decode_message(format!("{field} 必须是可精确表示的十进制金额")))
}

pub(crate) fn parse_number(value: &Value, field: &str) -> Result<f64> {
    let number = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()));
    number
        .filter(|number| number.is_finite())
        .ok_or_else(|| Error::decode_message(format!("{field} 必须为有限数值")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::error::Error as _;

    fn response(status: u16, body: &str) -> Response {
        Response {
            headers: HeaderMap::new(),
            url: "https://example.test/login".parse().unwrap(),
            context: Arc::new(RequestContext {
                method: "POST".into(),
                url: "https://example.test/login".into(),
                status_code: Some(status),
                error_code: None,
                response_body: Some(body.into()),
            }),
        }
    }

    #[test]
    fn failures_preserve_category_response_and_source() {
        assert!(response(302, "").allow_redirect().is_ok());
        let body = r#"{"status":401,"error":"Unauthorized","message":"Bad credentials"}"#;
        let error = response(401, body).require_success().err().unwrap();
        assert!(error.is_http());
        assert_eq!(error.to_string(), "Bad credentials (HTTP 401)");
        assert_eq!(error.request().unwrap().status_code, Some(401));
        assert_eq!(
            error.request().unwrap().response_body.as_deref(),
            Some(body)
        );
        let error = response(200, "invalid JSON").decode::<Value>().unwrap_err();
        assert!(error.is_decode());
        assert!(error.source().unwrap().is::<serde_json::Error>());
        let response = JsonResponse {
            value: json!({"result":42,"msg":"业务失败"}),
            context: response(200, r#"{"result":42,"msg":"业务失败"}"#).context,
        };
        let error = data::<Value>(response, "result").unwrap_err();
        assert!(error.is_remote());
        assert_eq!(error.request().unwrap().error_code.as_deref(), Some("42"));
    }

    #[test]
    fn recharge_preorder_failure_is_remote_unknown_even_with_http_200() {
        let message = "调用云平台生成预订单失败:云支付平台返回异常";
        let value =
            json!({"success": false, "message": message, "code": message, "resultData": null});
        let body = value.to_string();
        let mut context = response(200, &body).context;
        Arc::make_mut(&mut context).error_code = Some(message.into());
        let error = ecard_data::<Value>(JsonResponse { value, context }).unwrap_err();
        let Error::Remote(details) = &error else {
            panic!("expected remote error")
        };
        assert_eq!(details.kind(), crate::RemoteErrorKind::Unknown);
        assert_eq!(error.message(), message);
        let request = error.request().unwrap();
        assert_eq!(request.status_code, Some(200));
        assert_eq!(request.error_code.as_deref(), Some(message));
        assert_eq!(request.response_body.as_deref(), Some(body.as_str()));

        let value = json!({"resultData": {"order": "incomplete"}});
        let error = ecard_data::<Value>(JsonResponse {
            context: response(200, &value.to_string()).context,
            value,
        })
        .unwrap_err();
        assert!(error.is_decode());
    }
}
