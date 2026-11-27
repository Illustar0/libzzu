#[cfg(any(feature = "auth", feature = "network"))]
use std::future::Future;
use std::sync::Arc;
#[cfg(any(feature = "auth", feature = "network", test))]
use std::{fmt, ops::Deref};

#[cfg(any(feature = "auth", feature = "network"))]
use reqwest::RequestBuilder;
use reqwest::{Certificate, Client, ClientBuilder, redirect::Policy};
#[cfg(any(feature = "auth", feature = "network", test))]
use reqwest::{StatusCode, Url, header::HeaderMap};
#[cfg(any(feature = "auth", feature = "network", test))]
use serde::de::DeserializeOwned;
#[cfg(feature = "auth")]
use serde::{Deserialize, Deserializer};
#[cfg(any(feature = "auth", feature = "network", test))]
use serde_json::Value;

#[cfg(any(feature = "auth", feature = "network", test))]
use crate::RequestContext;
use crate::{Error, Result};

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

#[cfg(feature = "auth")]
pub(crate) fn client() -> Result<Client> {
    TrustRoots::default().client()
}

/// 创建使用平台校验器并按校园服务协议配置的 HTTP 客户端构建器，详见 [`TrustRoots::client_builder`]。
pub fn client_builder() -> ClientBuilder {
    TrustRoots::default().client_builder()
}

#[cfg(any(feature = "auth", feature = "network", test))]
pub(crate) struct Response {
    #[cfg_attr(not(feature = "auth"), expect(dead_code))]
    pub headers: HeaderMap,
    #[cfg_attr(
        not(any(feature = "ecard", feature = "mail", feature = "network")),
        expect(dead_code)
    )]
    pub url: Url,
    pub context: Arc<RequestContext>,
}

#[cfg(feature = "auth")]
pub(crate) struct BytesResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub bytes: Vec<u8>,
    pub context: Arc<RequestContext>,
}

#[cfg(any(feature = "auth", feature = "network"))]
fn sanitized_request_url(url: &Url) -> String {
    let mut sanitized = url.clone();
    sanitized.set_query(None);
    sanitized.set_fragment(None);
    let _ = sanitized.set_username("");
    let _ = sanitized.set_password(None);
    sanitized.to_string()
}

#[cfg(any(feature = "auth", feature = "network"))]
pub(crate) async fn send(builder: RequestBuilder) -> Result<Response> {
    send_with_text_decoder(builder, reqwest::Response::text).await
}

#[cfg(feature = "auth")]
pub(crate) async fn bytes(builder: RequestBuilder) -> Result<BytesResponse> {
    let (client, request) = builder.build_split();
    let request = request?;
    let mut context = RequestContext {
        method: request.method().to_string(),
        url: sanitized_request_url(request.url()),
        status_code: None,
        error_code: None,
        response_body: None,
    };
    let response = client
        .execute(request)
        .await
        .map_err(|error| Error::from(error).with_request(Arc::new(context.clone())))?;
    let status = response.status();
    context.status_code = Some(status.as_u16());
    let headers = response.headers().clone();
    let body = response
        .bytes()
        .await
        .map_err(|error| Error::from(error).with_request(Arc::new(context.clone())))?
        .to_vec();
    let is_image = headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|ct| ct.trim_start().to_ascii_lowercase().starts_with("image/"));
    if (!status.is_success() || !is_image)
        && let Ok(text) = std::str::from_utf8(&body)
    {
        if !status.is_success() {
            context.error_code = serde_json::from_str::<Value>(text)
                .ok()
                .and_then(|value| scalar_error_code(&value, "code"));
        }
        context.response_body = Some(text.to_owned());
    }
    Ok(BytesResponse {
        status,
        headers,
        bytes: body,
        context: Arc::new(context),
    })
}

#[cfg(any(feature = "auth", feature = "network"))]
pub(crate) async fn send_with_text_decoder<DecodeFuture>(
    builder: RequestBuilder,
    decode_body: impl FnOnce(reqwest::Response) -> DecodeFuture,
) -> Result<Response>
where
    DecodeFuture: Future<Output = reqwest::Result<String>>,
{
    let (client, request) = builder.build_split();
    let request = request?;
    let mut context = RequestContext {
        method: request.method().to_string(),
        url: sanitized_request_url(request.url()),
        status_code: None,
        error_code: None,
        response_body: None,
    };
    let response = client
        .execute(request)
        .await
        .map_err(|error| Error::from(error).with_request(Arc::new(context.clone())))?;
    let status = response.status();
    context.status_code = Some(status.as_u16());
    let headers = response.headers().clone();
    let url = response.url().clone();
    let body = decode_body(response)
        .await
        .map_err(|error| Error::from(error).with_request(Arc::new(context.clone())))?;
    if !status.is_success() {
        context.error_code = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|value| scalar_error_code(&value, "code"));
    }
    context.response_body = Some(body);
    Ok(Response {
        headers,
        url,
        context: Arc::new(context),
    })
}

#[cfg(any(feature = "auth", feature = "network", test))]
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

    #[cfg(any(feature = "auth", test))]
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
}

#[cfg(feature = "auth")]
impl BytesResponse {
    pub fn require_success(self) -> Result<Self> {
        if self.status.is_success() {
            return Ok(self);
        }
        let detail = self
            .context
            .response_body
            .as_deref()
            .and_then(|body| serde_json::from_str::<Value>(body).ok())
            .and_then(|value| response_message(&value))
            .unwrap_or_else(|| {
                self.status
                    .canonical_reason()
                    .unwrap_or("服务器返回错误状态")
                    .to_owned()
            });
        Err(Error::http(detail).with_request(self.context))
    }
}

#[cfg(any(feature = "auth", feature = "network", test))]
pub(crate) struct JsonResponse {
    pub value: Value,
    pub context: Arc<RequestContext>,
}

#[cfg(any(feature = "auth", feature = "network", test))]
impl Deref for JsonResponse {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.value
    }
}

#[cfg(any(feature = "auth", feature = "network", test))]
impl JsonResponse {
    #[cfg(all(test, feature = "auth"))]
    pub(crate) fn new(value: Value, context: Arc<RequestContext>) -> Self {
        Self { value, context }
    }

    #[cfg(feature = "ecard")]
    pub fn map_value(self, transform: impl FnOnce(&Value) -> Result<Value>) -> Result<Self> {
        let value =
            transform(&self.value).map_err(|error| error.with_request(self.context.clone()))?;
        Ok(Self {
            value,
            context: self.context,
        })
    }

    #[cfg(any(feature = "ecard", feature = "network"))]
    pub fn decode<T: DeserializeOwned>(self) -> Result<T> {
        deserialize_value(self.value, None).map_err(|error| error.with_request(self.context))
    }
}

#[cfg(any(feature = "auth", feature = "network"))]
pub(crate) async fn json(request: RequestBuilder) -> Result<JsonResponse> {
    parse_json_response(send(request).await?.require_success()?)
}

#[cfg(any(feature = "auth", feature = "network", test))]
fn parse_json_response(response: Response) -> Result<JsonResponse> {
    let value = serde_json::from_str(response.body())
        .map_err(|error| Error::from(error).with_request(response.context.clone()))?;
    Ok(JsonResponse {
        value,
        context: response.context,
    })
}

#[cfg(any(feature = "auth", test))]
pub(crate) fn data<T: DeserializeOwned>(response: JsonResponse, status_key: &str) -> Result<T> {
    check_status(&response, status_key)?;
    decode_field(response, "data")
}

#[cfg(any(feature = "auth", test))]
pub(crate) fn check_status(response: &JsonResponse, status_key: &str) -> Result<()> {
    let status = response
        .value
        .get(status_key)
        .ok_or_else(|| {
            Error::decode_message(format!("服务器响应缺少 {status_key}"))
                .with_path(status_key)
                .with_request(response.context.clone())
        })?
        .as_i64()
        .ok_or_else(|| {
            Error::decode_message(format!("{status_key} 必须为整数"))
                .with_path(status_key)
                .with_request(response.context.clone())
        })?;
    if status != 0 {
        return Err(remote_error(
            &response.value,
            response.context.clone(),
            status_key,
        ));
    }
    Ok(())
}

#[cfg(feature = "ecard")]
pub(crate) fn ecard_data<T: DeserializeOwned>(response: JsonResponse) -> Result<T> {
    match response.get("success").and_then(Value::as_bool) {
        Some(true) => decode_field(response, "resultData"),
        Some(false) => Err(remote_error(&response.value, response.context, "code")),
        None => Err(
            Error::decode_message("校园卡响应缺少有效的 success 字段".into())
                .with_path("success")
                .with_request(response.context),
        ),
    }
}

#[cfg(any(feature = "auth", test))]
pub(crate) fn remote_error(
    value: &Value,
    mut context: Arc<RequestContext>,
    code_key: &str,
) -> Error {
    if let Some(code) = scalar_error_code(value, code_key) {
        Arc::make_mut(&mut context).error_code = Some(code);
    }
    Error::remote(message(value)).with_request(context)
}

#[cfg(any(feature = "auth", test))]
fn decode_field<T: DeserializeOwned>(mut response: JsonResponse, key: &str) -> Result<T> {
    let field = response.value.get_mut(key).ok_or_else(|| {
        Error::decode_message(format!("服务器响应缺少 {key}"))
            .with_path(key)
            .with_request(response.context.clone())
    })?;
    deserialize_value(field.take(), Some(key)).map_err(|error| error.with_request(response.context))
}

#[cfg(any(feature = "auth", feature = "network", test))]
#[derive(Debug)]
struct PathDecodeError {
    path: String,
    source: serde_json::Error,
}

#[cfg(any(feature = "auth", feature = "network", test))]
impl fmt::Display for PathDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path, self.source)
    }
}

#[cfg(any(feature = "auth", feature = "network", test))]
impl std::error::Error for PathDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.source()
    }
}

#[cfg(any(feature = "network", test))]
pub(crate) fn deserialize_str<T: DeserializeOwned>(text: &str) -> Result<T> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    serde_path_to_error::deserialize(&mut deserializer).map_err(|err| into_path_error(err, None))
}

#[cfg(any(feature = "auth", feature = "ecard", feature = "network", test))]
pub(crate) fn deserialize_value<T: DeserializeOwned>(
    value: Value,
    root: Option<&str>,
) -> Result<T> {
    serde_path_to_error::deserialize(value).map_err(|err| into_path_error(err, root))
}

#[cfg(feature = "mail")]
pub(crate) fn deserialize_value_ref<T: DeserializeOwned>(
    value: &Value,
    root: Option<&str>,
) -> Result<T> {
    serde_path_to_error::deserialize(value).map_err(|err| into_path_error(err, root))
}

#[cfg(any(feature = "auth", feature = "network", test))]
fn into_path_error(
    err: serde_path_to_error::Error<serde_json::Error>,
    root: Option<&str>,
) -> Error {
    let has_segments = err.path().iter().next().is_some();
    let path = match (root.filter(|name| !name.is_empty()), has_segments) {
        (None, false) => return Error::from(err.into_inner()),
        (None, true) => err.path().to_string(),
        (Some(root), false) => root.to_owned(),
        (Some(root), true) => {
            let relative = err.path().to_string();
            if relative.starts_with('[') {
                format!("{root}{relative}")
            } else {
                format!("{root}.{relative}")
            }
        }
    };
    let source = PathDecodeError {
        path: path.clone(),
        source: err.into_inner(),
    };
    Error::decode(source).with_path(path)
}

/// 将显式 JSON `null` 反序列化为 `T::default()`，避免上游空值标量字段中断整段解码。
#[cfg(feature = "auth")]
pub(crate) fn deserialize_null_default<'de, D, T>(
    deserializer: D,
) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[cfg(any(feature = "auth", test))]
pub(crate) fn message(value: &Value) -> String {
    response_message(value).unwrap_or_else(|| "服务器未提供错误消息".into())
}

#[cfg(any(feature = "auth", feature = "network", test))]
fn response_message(value: &Value) -> Option<String> {
    ["message", "msg", "error"].into_iter().find_map(|key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    })
}

#[cfg(any(feature = "auth", feature = "network", test))]
fn scalar_error_code(value: &Value, key: &str) -> Option<String> {
    match value.get(key)? {
        Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        }
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

#[cfg(feature = "ecard")]
pub(crate) fn parse_money(value: &Value, field: &str) -> Result<crate::Decimal> {
    crate::money::deserialize_decimal(value).map_err(|error| {
        Error::decode_with_source(format!("{field} 必须是可精确表示的十进制金额"), error)
    })
}

#[cfg(feature = "auth")]
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
        let error = parse_json_response(response(200, "invalid JSON"))
            .err()
            .unwrap();
        assert!(error.is_decode());
        assert!(error.source().unwrap().is::<serde_json::Error>());
        let response = JsonResponse {
            value: json!({"result":42,"msg":"业务失败"}),
            context: response(200, r#"{"result":42,"msg":"业务失败"}"#).context,
        };
        let error = data::<Value>(response, "result").unwrap_err();
        assert!(error.is_remote());
        assert_eq!(error.request().unwrap().error_code.as_deref(), Some("42"));

        // 成功信封在后续字段解码失败时不应把业务成功码误记为 error_code。
        let ok_envelope_bad_data = JsonResponse {
            value: json!({"code": 0, "data": "not-an-int"}),
            context: self::response(200, r#"{"code":0,"data":"not-an-int"}"#).context,
        };
        let error = data::<i32>(ok_envelope_bad_data, "code").unwrap_err();
        assert!(error.is_decode());
        assert_eq!(error.request().unwrap().error_code, None);

        // 非标量、null 与空白字符串不应被提取为 error_code。
        for empty_or_non_scalar in [
            json!({"code": ""}),
            json!({"code": "   "}),
            json!({"code": null}),
            json!({"code": true}),
            json!({"code": {"nested": 1}}),
        ] {
            assert_eq!(scalar_error_code(&empty_or_non_scalar, "code"), None);
        }
        assert_eq!(
            scalar_error_code(&json!({"code": " valid_error_1004 "}), "code").as_deref(),
            Some("valid_error_1004")
        );
        assert_eq!(
            scalar_error_code(&json!({"code": 401}), "code").as_deref(),
            Some("401")
        );
    }

    #[test]
    fn decode_failures_include_json_field_paths_in_error_source() {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct GradeItem {
            #[expect(dead_code)]
            sub_grades: Vec<SubItem>,
        }
        #[derive(serde::Deserialize)]
        struct SubItem {
            #[expect(dead_code)]
            id: i64,
        }
        let nested_bad = JsonResponse {
            value: json!({
                "result": 0,
                "data": [
                    {"subGrades": [{"id": 1}]},
                    {"subGrades": [{"id": null}]}
                ]
            }),
            context: response(200, "{}").context,
        };
        let err = data::<Vec<GradeItem>>(nested_bad, "result").err().unwrap();
        let Error::Decode(decode_err) = &err else {
            panic!("expected decode error, got {err:?}");
        };
        assert_eq!(decode_err.path(), Some("data[1].subGrades[0].id"));
        let path_err = err.source().unwrap().to_string();
        assert!(
            path_err.starts_with("data[1].subGrades[0].id: invalid type: null, expected i64"),
            "unexpected path error: {path_err}"
        );

        // 缺少外层字段时同样记录字段路径。
        let missing_data = JsonResponse {
            value: json!({"result": 0}),
            context: response(200, "{}").context,
        };
        let err = data::<Vec<GradeItem>>(missing_data, "result")
            .err()
            .unwrap();
        let Error::Decode(decode_err) = &err else {
            panic!("expected decode error, got {err:?}");
        };
        assert_eq!(decode_err.path(), Some("data"));

        // 顶层无路径错误即使底层消息含冒号与敏感字面量，path() 仍为 None；有字段路径时仅暴露路径名。
        let root_scalar = deserialize_str::<i64>(r#""secret:token""#).unwrap_err();
        let Error::Decode(root_decode) = &root_scalar else {
            panic!("expected decode error, got {root_scalar:?}");
        };
        assert_eq!(root_decode.path(), None);
        assert!(root_scalar.source().unwrap().to_string().contains(':'));

        let field_with_secret = deserialize_str::<SubItem>(r#"{"id":"secret:token"}"#)
            .err()
            .unwrap();
        let Error::Decode(field_decode) = &field_with_secret else {
            panic!("expected decode error, got {field_with_secret:?}");
        };
        assert_eq!(field_decode.path(), Some("id"));
    }
}
