use jiff::{Timestamp, Zoned};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::CasClient;
use crate::{Error, Result, http};

const DEVICES_URL: &str =
    "https://authx-service.s.zzu.edu.cn/personal/api/v1/personal/user/accountUsedAgent";

/// 历史登录设备分页查询的默认每页条数。
pub const DEFAULT_DEVICE_PAGE_SIZE: u32 = 20;

/// 历史登录设备的单页查询结果。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevicePage {
    /// 当前页的历史登录设备列表。
    pub items: Vec<LoginDevice>,
    /// 当前页码（从 `1` 开始）。
    pub page: u32,
    /// 本次查询实际使用的每页条数。
    pub page_size: u32,
    /// 账号下的历史设备记录总数。
    pub total_items: u32,
    /// 按 `page_size` 计算的总页数。
    pub total_pages: u32,
}

/// 账号下曾登录过的单台终端设备及信任状态记录。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginDevice {
    /// 个人安全中心设备记录唯一标识。
    pub id: String,
    /// 设备标识（与 [`CasClient::device_id`] 及 [`CasClient::set_device_trust`] 的 `device_id` 一致）。
    pub device_id: String,
    /// 该设备是否已设为受信任设备（受信任设备登录时无须多因素认证）。
    pub is_trusted: bool,
    /// 终端设备形态类别（如 `"PC"`、`"MOBILE"`）。
    pub device_type: Option<String>,
    /// 操作系统标识（如 `"MAC_OS_X"`、`"ANDROID"`）。
    pub os: Option<String>,
    /// 浏览器或客户端标识。
    pub browser: Option<String>,
    /// 完整的客户端 `User-Agent` 字符串。
    pub user_agent: Option<String>,
    /// 累计登录次数。
    pub used_count: u32,
    /// 首次登录时间（`Asia/Shanghai` 时区）。
    pub first_used_time: Zoned,
    /// 最近一次登录时间（`Asia/Shanghai` 时区）。
    pub last_used_time: Zoned,
    /// 上一次登录时间（`Asia/Shanghai` 时区；未记录时为 `None`）。
    pub previous_used_time: Option<Zoned>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDevicePage {
    #[serde(default)]
    page_count: u32,
    #[serde(default)]
    record_count: u32,
    #[serde(default)]
    items: Option<Vec<RawLoginDevice>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLoginDevice {
    id: String,
    agent_id: String,
    #[serde(default)]
    agent_trust: bool,
    #[serde(default)]
    machine_type: Option<String>,
    #[serde(default)]
    agent_type: Option<String>,
    #[serde(default)]
    os: Option<String>,
    #[serde(default)]
    browser: Option<String>,
    #[serde(default)]
    user_agent: Option<String>,
    #[serde(default)]
    used_count: u32,
    first_used_time: String,
    last_used_time: String,
    #[serde(default)]
    prev_used_time: Option<String>,
}

impl CasClient {
    /// 分页查询当前账号下的历史登录设备及免 MFA 信任状态列表。
    ///
    /// `page` 从 `1` 开始（内部自动换算为上游从 `0` 起算的 `pageIndex`）；
    /// `page_size` 为 `None` 时使用 [`DEFAULT_DEVICE_PAGE_SIZE`]（`20`）。
    /// 返回条目的 [`LoginDevice::device_id`] 可直接传给 [`Self::set_device_trust`] 更新信任状态。
    ///
    /// # Errors
    ///
    /// - `page == 0` 或 `page_size == Some(0)` 时返回 [`Error::InvalidInput`]。
    /// - 客户端尚未登录或令牌不可用时返回 [`Error::Authentication`]。
    /// - 请求失败、服务端返回业务拒绝或响应字段解析失败时返回错误。
    pub async fn devices(&self, page: u32, page_size: Option<u32>) -> Result<DevicePage> {
        let (page_index, resolved_page_size) = validate_device_pagination(page, page_size)?;
        let token = self.user_token().await?;
        let request = self
            .inner
            .http
            .get(DEVICES_URL)
            .header("X-Id-Token", token)
            .query(&[("pageIndex", page_index), ("pageSize", resolved_page_size)]);
        let response = http::json(request).await?;
        parse_devices_response(response, page, resolved_page_size)
    }
}

fn validate_device_pagination(page: u32, page_size: Option<u32>) -> Result<(u32, u32)> {
    if page == 0 {
        return Err(Error::invalid_input("页码 page 必须从 1 开始".into()));
    }
    let page_size = page_size.unwrap_or(DEFAULT_DEVICE_PAGE_SIZE);
    if page_size == 0 {
        return Err(Error::invalid_input("每页条数 page_size 必须大于 0".into()));
    }
    Ok((page - 1, page_size))
}

fn parse_devices_response(
    response: http::JsonResponse,
    page: u32,
    page_size: u32,
) -> Result<DevicePage> {
    http::check_status(&response, "code")?;
    if response.get("acknowleged").and_then(Value::as_bool) == Some(false) {
        return Err(Error::remote(http::message(&response)).with_request(response.context));
    }
    let context = response.context.clone();
    let raw: RawDevicePage = http::data(response, "code")?;
    let mut items = Vec::new();
    for item in raw.items.unwrap_or_default() {
        items
            .push(convert_login_device(item).map_err(|error| error.with_request(context.clone()))?);
    }
    Ok(DevicePage {
        items,
        page,
        page_size,
        total_items: raw.record_count,
        total_pages: raw.page_count,
    })
}

fn convert_login_device(raw: RawLoginDevice) -> Result<LoginDevice> {
    let device_id = raw.agent_id.trim().to_owned();
    if device_id.is_empty() {
        return Err(Error::decode_message(
            "登录设备记录缺少有效的 agentId".into(),
        ));
    }
    let device_type =
        clean_optional_text(raw.machine_type).or_else(|| clean_optional_text(raw.agent_type));
    let first_used_time = parse_authx_time(&raw.first_used_time, "firstUsedTime")?;
    let last_used_time = parse_authx_time(&raw.last_used_time, "lastUsedTime")?;
    let previous_used_time = clean_optional_text(raw.prev_used_time)
        .as_deref()
        .map(|text| parse_authx_time(text, "prevUsedTime"))
        .transpose()?;
    Ok(LoginDevice {
        id: raw.id,
        device_id,
        is_trusted: raw.agent_trust,
        device_type,
        os: clean_optional_text(raw.os),
        browser: clean_optional_text(raw.browser),
        user_agent: clean_optional_text(raw.user_agent),
        used_count: raw.used_count,
        first_used_time,
        last_used_time,
        previous_used_time,
    })
}

fn clean_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn parse_authx_time(text: &str, field: &str) -> Result<Zoned> {
    let trimmed = text.trim();
    if let Ok(zoned) = trimmed.parse::<Zoned>() {
        return Ok(zoned);
    }
    trimmed
        .parse::<Timestamp>()
        .and_then(|timestamp| timestamp.in_tz("Asia/Shanghai"))
        .map_err(|error| Error::decode_with_source(format!("设备时间字段 {field} 格式无效"), error))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::RequestContext;

    fn authx_devices_response(value: Value) -> http::JsonResponse {
        http::JsonResponse::new(
            value,
            Arc::new(RequestContext {
                method: "GET".into(),
                url: DEVICES_URL.into(),
                status_code: Some(200),
                error_code: None,
                response_body: None,
            }),
        )
    }

    #[test]
    fn device_pagination_validates_bounds_and_maps_zero_based_index() {
        assert_eq!(validate_device_pagination(1, None).unwrap(), (0, 20));
        assert_eq!(validate_device_pagination(3, Some(5)).unwrap(), (2, 5));
        assert!(
            validate_device_pagination(0, None)
                .unwrap_err()
                .is_invalid_input()
        );
        assert!(
            validate_device_pagination(1, Some(0))
                .unwrap_err()
                .is_invalid_input()
        );
    }

    #[test]
    fn parse_devices_normalizes_authx_payload_and_roundtrips_camel_case() {
        let response = authx_devices_response(json!({
            "acknowleged": true,
            "code": 0,
            "message": null,
            "data": {
                "pageIndex": 0,
                "pageSize": 0,
                "pageCount": 18,
                "recordCount": 346,
                "currentItemCount": 2,
                "items": [
                    {
                        "id": "rec-1",
                        "agentId": "dev-1",
                        "agentTrust": true,
                        "agentType": "PC",
                        "machineType": "PC",
                        "os": "MAC_OS_X",
                        "browser": "Firefox 16/160.0",
                        "userAgent": "Mozilla/5.0",
                        "usedCount": 24,
                        "firstUsedTime": "2026-02-12T10:08:44.000+08:00",
                        "lastUsedTime": "2026-11-25T15:33:49.000+08:00",
                        "prevUsedTime": "2026-11-24T12:45:22.000+08:00"
                    },
                    {
                        "id": "rec-2",
                        "agentId": "zzu-tour",
                        "agentTrust": false,
                        "agentType": "MOBILE",
                        "machineType": "",
                        "os": "  ",
                        "browser": null,
                        "userAgent": "SWSuperApp/1.1.1()",
                        "usedCount": 1,
                        "firstUsedTime": "2026-11-26T22:00:00.000+08:00",
                        "lastUsedTime": "2026-11-26T22:00:00.000+08:00",
                        "prevUsedTime": null
                    }
                ]
            }
        }));

        let page = parse_devices_response(response, 2, 2).unwrap();
        assert_eq!(page.page, 2);
        assert_eq!(page.page_size, 2);
        assert_eq!(page.total_items, 346);
        assert_eq!(page.total_pages, 18);
        assert_eq!(page.items.len(), 2);

        assert_eq!(page.items[0].device_id, "dev-1");
        assert!(page.items[0].is_trusted);
        assert_eq!(page.items[0].device_type.as_deref(), Some("PC"));
        assert_eq!(
            page.items[0].first_used_time.to_string(),
            "2026-02-12T10:08:44+08:00[Asia/Shanghai]"
        );
        assert!(page.items[0].previous_used_time.is_some());

        assert_eq!(page.items[1].device_id, "zzu-tour");
        assert!(!page.items[1].is_trusted);
        assert_eq!(page.items[1].device_type.as_deref(), Some("MOBILE"));
        assert_eq!(page.items[1].os, None);
        assert_eq!(page.items[1].browser, None);
        assert_eq!(page.items[1].previous_used_time, None);

        let serialized = serde_json::to_value(&page).unwrap();
        assert_eq!(serialized["items"][0]["deviceId"], "dev-1");
        assert_eq!(serialized["items"][0]["isTrusted"], true);
        let roundtripped: DevicePage = serde_json::from_value(serialized).unwrap();
        assert_eq!(roundtripped, page);
    }

    #[test]
    fn parse_devices_surfaces_remote_and_decode_errors_with_context() {
        let unacknowledged = authx_devices_response(json!({
            "acknowleged": false,
            "code": 0,
            "message": "查询受限"
        }));
        let error = parse_devices_response(unacknowledged, 1, 20).unwrap_err();
        assert!(error.is_remote());
        assert_eq!(error.message(), "查询受限");
        assert!(error.request().is_some());

        let invalid_item = authx_devices_response(json!({
            "acknowleged": true,
            "code": 0,
            "data": {
                "pageCount": 1,
                "recordCount": 1,
                "items": [{
                    "id": "rec-1",
                    "agentId": "   ",
                    "firstUsedTime": "2026-11-26T22:00:00.000+08:00",
                    "lastUsedTime": "2026-11-26T22:00:00.000+08:00"
                }]
            }
        }));
        let error = parse_devices_response(invalid_item, 1, 20).unwrap_err();
        assert!(error.is_decode());
        assert!(error.request().is_some());

        let invalid_time = authx_devices_response(json!({
            "acknowleged": true,
            "code": 0,
            "data": {
                "pageCount": 1,
                "recordCount": 1,
                "items": [{
                    "id": "rec-1",
                    "agentId": "dev-1",
                    "firstUsedTime": "not-a-timestamp",
                    "lastUsedTime": "2026-11-26T22:00:00.000+08:00"
                }]
            }
        }));
        let error = parse_devices_response(invalid_time, 1, 20).unwrap_err();
        assert!(error.is_decode());
        assert!(std::error::Error::source(&error).is_some());
    }

    #[tokio::test]
    async fn devices_checks_pagination_before_authentication() {
        let cas = CasClient::new("", "", "test-device").unwrap();
        assert!(cas.devices(0, None).await.unwrap_err().is_invalid_input());
        assert!(
            cas.devices(1, None)
                .await
                .unwrap_err()
                .is_not_authenticated()
        );
    }
}
