use std::fmt;

use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{StatusCode, header::CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::CasClient;
use crate::{Error, Result, http};

const PORTRAIT_URL: &str =
    "https://authx-service.s.zzu.edu.cn/personal/api/v1/personal/me/portrait";

/// 用户在统一身份认证个人中心的头像图像数据。
///
/// Rust 调用方可直接读取原始图像字节 [`Self::bytes`]；JSON 序列化与反序列化时，
/// 图像内容自动按标准 Base64（RFC 4648）编码为 `data` 字符串字段。
#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Portrait {
    /// 图像 MIME 媒体类型（如 `"image/png"`、`"image/jpeg"`，已剥离附加参数并转为小写）。
    pub media_type: String,
    /// 原始图像二进制字节流（JSON 中编码为标准 Base64 字符串 `data`）。
    #[serde(rename = "data", with = "base64_bytes")]
    pub bytes: Vec<u8>,
}

impl fmt::Debug for Portrait {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Portrait")
            .field("media_type", &self.media_type)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

mod base64_bytes {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        STANDARD
            .decode(encoded.trim())
            .map_err(serde::de::Error::custom)
    }
}

impl CasClient {
    /// 查询当前账号在统一身份认证个人中心的头像图像。
    ///
    /// 若当前账号未设置头像（服务端返回 `404`、`204` 或空内容），返回 `Ok(None)`。
    ///
    /// # Errors
    ///
    /// - 客户端尚未登录或令牌不可用时返回 [`Error::Authentication`]。
    /// - 网络传输失败、服务端返回非成功状态码或业务错误，或响应负载不是可识别的图像格式时返回错误。
    pub async fn portrait(&self) -> Result<Option<Portrait>> {
        let token = self.user_token().await?;
        let response = http::bytes(
            self.inner
                .http
                .get(PORTRAIT_URL)
                .header("X-Id-Token", token),
        )
        .await?;
        parse_portrait_response(response)
    }
}

fn parse_portrait_response(response: http::BytesResponse) -> Result<Option<Portrait>> {
    if matches!(
        response.status,
        StatusCode::NOT_FOUND | StatusCode::NO_CONTENT
    ) {
        return Ok(None);
    }
    let response = response.require_success()?;
    if response.bytes.is_empty() {
        return Ok(None);
    }

    let declared_type = response
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.split(';').next())
        .map(|mime| mime.trim().to_ascii_lowercase())
        .filter(|mime| !mime.is_empty());

    if declared_type.as_deref() == Some("application/json") {
        return parse_json_portrait_payload(&response.bytes, &response.context);
    }

    if let Some(mime) = declared_type
        .as_deref()
        .filter(|mime| mime.starts_with("image/"))
    {
        return Ok(Some(Portrait {
            media_type: mime.to_owned(),
            bytes: response.bytes,
        }));
    }

    if let Some(sniffed) = sniff_image_media_type(&response.bytes) {
        return Ok(Some(Portrait {
            media_type: sniffed.into(),
            bytes: response.bytes,
        }));
    }

    if let Ok(text) = std::str::from_utf8(&response.bytes) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if trimmed.starts_with('{') {
            return parse_json_portrait_payload(trimmed.as_bytes(), &response.context);
        }
        if let Ok(decoded) = STANDARD.decode(trimmed)
            && let Some(sniffed) = sniff_image_media_type(&decoded)
        {
            return Ok(Some(Portrait {
                media_type: sniffed.into(),
                bytes: decoded,
            }));
        }
    }

    Err(
        Error::decode_message("个人中心头像响应不是受支持的图像格式".into())
            .with_request(response.context),
    )
}

fn parse_json_portrait_payload(
    bytes: &[u8],
    context: &std::sync::Arc<crate::RequestContext>,
) -> Result<Option<Portrait>> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| Error::from(error).with_request(context.clone()))?;
    if value.get("acknowleged").and_then(Value::as_bool) == Some(false) {
        return Err(http::remote_error(&value, context.clone(), "code"));
    }
    if let Some(code) = value.get("code").and_then(Value::as_i64)
        && code != 0
    {
        return Err(http::remote_error(&value, context.clone(), "code"));
    }
    if value.get("data").is_some_and(Value::is_null) {
        return Ok(None);
    }
    Err(
        Error::decode_message("个人中心头像 JSON 响应不包含有效图像".into())
            .with_request(context.clone()),
    )
}

fn sniff_image_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use reqwest::header::{HeaderMap, HeaderValue};
    use serde_json::json;

    use super::*;
    use crate::RequestContext;

    const SAMPLE_PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";

    fn portrait_response(
        status: u16,
        content_type: Option<&str>,
        body: Vec<u8>,
    ) -> http::BytesResponse {
        let mut headers = HeaderMap::new();
        if let Some(ct) = content_type {
            headers.insert(CONTENT_TYPE, HeaderValue::from_str(ct).unwrap());
        }
        let response_body = if status >= 400 {
            std::str::from_utf8(&body).ok().map(str::to_owned)
        } else {
            None
        };
        http::BytesResponse {
            status: StatusCode::from_u16(status).unwrap(),
            headers,
            bytes: body,
            context: Arc::new(RequestContext {
                method: "GET".into(),
                url: PORTRAIT_URL.into(),
                status_code: Some(status),
                error_code: None,
                response_body,
            }),
        }
    }

    #[test]
    fn portrait_parses_binary_sniffed_and_base64_images_and_roundtrips_json() {
        let png = parse_portrait_response(portrait_response(
            200,
            Some("Image/PNG; charset=binary"),
            SAMPLE_PNG.to_vec(),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(png.media_type, "image/png");
        assert_eq!(png.bytes, SAMPLE_PNG);
        assert!(format!("{png:?}").contains("byte_len: 16"));

        let serialized = serde_json::to_value(&png).unwrap();
        assert_eq!(
            serialized,
            json!({
                "mediaType": "image/png",
                "data": STANDARD.encode(SAMPLE_PNG),
            })
        );
        let roundtripped: Portrait = serde_json::from_value(serialized).unwrap();
        assert_eq!(roundtripped, png);

        let sniffed_jpeg = parse_portrait_response(portrait_response(
            200,
            Some("application/octet-stream"),
            vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10],
        ))
        .unwrap()
        .unwrap();
        assert_eq!(sniffed_jpeg.media_type, "image/jpeg");

        let base64_png = parse_portrait_response(portrait_response(
            200,
            Some("text/plain;charset=UTF-8"),
            STANDARD.encode(SAMPLE_PNG).into_bytes(),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(base64_png, png);
    }

    #[test]
    fn portrait_handles_absent_avatars_and_surfaces_http_remote_and_decode_errors() {
        assert_eq!(
            parse_portrait_response(portrait_response(404, None, Vec::new())).unwrap(),
            None
        );
        assert_eq!(
            parse_portrait_response(portrait_response(204, None, Vec::new())).unwrap(),
            None
        );
        assert_eq!(
            parse_portrait_response(portrait_response(200, Some("image/png"), Vec::new())).unwrap(),
            None
        );
        assert_eq!(
            parse_portrait_response(portrait_response(
                200,
                Some("application/json"),
                br#"{"acknowleged":true,"code":0,"data":null}"#.to_vec(),
            ))
            .unwrap(),
            None
        );

        let forbidden = parse_portrait_response(portrait_response(
            403,
            Some("application/json"),
            br#"{"status":403,"message":"Access Denied"}"#.to_vec(),
        ))
        .unwrap_err();
        assert!(forbidden.is_http());
        assert_eq!(forbidden.message(), "Access Denied");

        let remote_err = parse_portrait_response(portrait_response(
            200,
            Some("application/json"),
            r#"{"acknowleged":false,"code":500,"message":"文件服务异常"}"#
                .as_bytes()
                .to_vec(),
        ))
        .unwrap_err();
        assert!(remote_err.is_remote());
        assert_eq!(remote_err.message(), "文件服务异常");
        assert_eq!(
            remote_err.request().unwrap().error_code.as_deref(),
            Some("500")
        );

        let html_err = parse_portrait_response(portrait_response(
            200,
            Some("text/html"),
            b"<html>login</html>".to_vec(),
        ))
        .unwrap_err();
        assert!(html_err.is_decode());
        assert!(html_err.request().is_some());
    }

    #[tokio::test]
    async fn unauthenticated_portrait_is_rejected() {
        let cas = CasClient::new("", "", "test-device").unwrap();
        assert!(cas.portrait().await.unwrap_err().is_not_authenticated());
    }
}
