use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ECARD_URL, ELECTRIC_APP_ID, EcardClient, RoomId};
use crate::{Decimal, Error, Result, http};

/// 宿舍电控账户详情快照。
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct EnergyAccount {
    /// 本次查询对应的宿舍房间号（入参省略时为当前账号绑定的默认房间）。
    pub room: RoomId,
    /// 宿舍房间完整位置名称（如 `"主校区松园2号楼照明房间301房间"`；查询默认房间或命中默认房间缓存时返回，直接查询其他未缓存房间时为 `None`）。
    #[serde(default)]
    pub room_name: Option<String>,
    /// 当前剩余电量（单位：度 / kWh）。
    pub remaining_energy: f64,
    /// 电价单价（单位：元/度；服务端未返回时为 `None`）。
    #[serde(
        default,
        deserialize_with = "crate::money::deserialize_optional_decimal"
    )]
    pub unit_price: Option<Decimal>,
    /// 按金额计费房间的电费账户余额（单位：元；按电量计费的宿舍通常为 `None`）。
    #[serde(
        default,
        deserialize_with = "crate::money::deserialize_optional_decimal"
    )]
    pub electricity_balance: Option<Decimal>,
    /// 查询时刻关联的校园卡账户余额快照（单位：元；服务端未返回时为 `None`）。
    #[serde(
        default,
        deserialize_with = "crate::money::deserialize_optional_decimal"
    )]
    pub card_balance: Option<Decimal>,
    /// 电控表计运行状态描述（如 `"正常"`；服务端未返回时为 `None`）。
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Clone)]
pub(super) struct DefaultRoom {
    pub room: RoomId,
    pub name: Option<String>,
}

#[derive(Deserialize)]
struct RawUtilitiesConfig {
    location: RawConfigLocation,
}

#[derive(Deserialize)]
struct RawConfigLocation {
    room: RoomId,
    #[serde(default, rename = "bigAreaName")]
    big_area: String,
    #[serde(default, rename = "areaName")]
    area: String,
    #[serde(default, rename = "subAreaName")]
    sub_area: String,
    #[serde(default, rename = "buildingName")]
    building: String,
    #[serde(default, rename = "unitName")]
    unit: String,
    #[serde(default, rename = "levelName")]
    level: String,
    #[serde(default, rename = "roomName")]
    label: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEnergyAccount {
    #[serde(default)]
    balance: Value,
    #[serde(default)]
    utility_status_name: Option<String>,
    template_list: Vec<RawTemplateItem>,
}

#[derive(Deserialize)]
struct RawTemplateItem {
    code: String,
    #[serde(default)]
    value: Value,
}

impl EcardClient {
    /// 查询并缓存当前账号在电控系统中绑定的默认宿舍房间 ID。
    ///
    /// # Errors
    ///
    /// - 客户端尚未调用 [`Self::login`] 时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    /// - 会话刷新、网络请求、远端业务校验或房间号解析失败时返回错误。
    pub async fn default_room(&self) -> Result<RoomId> {
        Ok(self.resolve_default_room().await?.room)
    }

    /// 查询指定宿舍房间的电控账户详情（`room` 为 `None` 时取 [`Self::default_room`]）。
    ///
    /// 除剩余电量外，一并返回电价单价、按金额计费时的电费余额、校园卡余额快照与表计运行状态；
    /// 当 `room` 为 `None` 或与已缓存的默认房间一致时，同时附带无需额外请求的完整房间名称。
    ///
    /// # Errors
    ///
    /// - 客户端尚未登录时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    /// - 获取默认房间、请求电费账户或解析电量与金额字段失败时返回错误。
    pub async fn energy_account(&self, room: Option<&RoomId>) -> Result<EnergyAccount> {
        let (target_room, room_name, token) = if let Some(room) = room {
            let token = self.access_token().await?;
            let room_name = self
                .lock_state()
                .default_room
                .as_ref()
                .filter(|cached| cached.room == *room)
                .and_then(|cached| cached.name.clone());
            (room.clone(), room_name, token)
        } else {
            let default = self.resolve_default_room().await?;
            let token = self.access_token().await?;
            (default.room, default.name, token)
        };

        let response = http::json(
            self.http
                .post(format!("{ECARD_URL}/utilities/account"))
                .header("Authorization", token)
                .json(&target_room.location()),
        )
        .await?;
        parse_energy_account_response(response, target_room, room_name)
    }

    /// 查询指定宿舍房间的剩余电量（单位：度；`room` 为 `None` 时取 [`Self::default_room`]）。
    ///
    /// 若同时需要电价单价、房间名称或表计状态，请使用 [`Self::energy_account`]。
    ///
    /// # Errors
    ///
    /// - 客户端尚未登录时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    /// - 获取默认房间、请求电费账户或解析 `quantity` 电量字段失败时返回错误。
    pub async fn remaining_energy(&self, room: Option<&RoomId>) -> Result<f64> {
        Ok(self.energy_account(room).await?.remaining_energy)
    }

    async fn resolve_default_room(&self) -> Result<DefaultRoom> {
        let token = self.access_token().await?;
        if let Some(cached) = self.lock_state().default_room.clone() {
            return Ok(cached);
        }
        let response = http::json(
            self.http
                .post(format!("{ECARD_URL}/utilities/config"))
                .header("Authorization", token)
                .json(&json!({"utilityType": "electric", "appId": ELECTRIC_APP_ID})),
        )
        .await?;
        let resolved = parse_default_room_response(response)?;
        self.lock_state().default_room = Some(resolved.clone());
        Ok(resolved)
    }
}

fn parse_default_room_response(response: http::JsonResponse) -> Result<DefaultRoom> {
    let config: RawUtilitiesConfig = http::ecard_data(response)?;
    let location = config.location;
    let joined: String = [
        &location.big_area,
        &location.area,
        &location.sub_area,
        &location.building,
        &location.unit,
        &location.level,
        &location.label,
    ]
    .into_iter()
    .map(|segment| segment.trim())
    .filter(|segment| !segment.is_empty())
    .collect();
    let name = (!joined.is_empty()).then_some(joined);
    Ok(DefaultRoom {
        room: location.room,
        name,
    })
}

fn parse_energy_account_response(
    response: http::JsonResponse,
    room: RoomId,
    room_name: Option<String>,
) -> Result<EnergyAccount> {
    let context = response.context.clone();
    let raw: RawEnergyAccount = http::ecard_data(response)?;

    let quantity = template_value(&raw.template_list, "quantity").ok_or_else(|| {
        Error::decode_message("电费账户缺少 quantity".into()).with_request(context.clone())
    })?;
    let remaining_energy = http::parse_number(quantity, "quantity")
        .map_err(|error| error.with_request(context.clone()))?;
    let unit_price = parse_optional_money(template_value(&raw.template_list, "price"), "price")
        .map_err(|error| error.with_request(context.clone()))?;
    let electricity_balance =
        parse_optional_money(template_value(&raw.template_list, "balance"), "balance")
            .map_err(|error| error.with_request(context.clone()))?;
    let card_balance = match parse_optional_money(Some(&raw.balance), "balance")
        .map_err(|error| error.with_request(context.clone()))?
    {
        Some(amount) => Some(amount),
        None => parse_optional_money(
            template_value(&raw.template_list, "ykt_balance"),
            "ykt_balance",
        )
        .map_err(|error| error.with_request(context))?,
    };
    let status = raw.utility_status_name.and_then(|text| {
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    });

    Ok(EnergyAccount {
        room,
        room_name,
        remaining_energy,
        unit_price,
        electricity_balance,
        card_balance,
        status,
    })
}

fn template_value<'a>(items: &'a [RawTemplateItem], code: &str) -> Option<&'a Value> {
    items
        .iter()
        .find(|item| item.code == code)
        .map(|item| &item.value)
}

fn parse_optional_money(value: Option<&Value>, field: &str) -> Result<Option<Decimal>> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::String(text) if text.trim().is_empty() => Ok(None),
        other => http::parse_money(other, field).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

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
    fn default_room_extracts_validated_room_and_concatenates_display_name() {
        let resolved = parse_default_room_response(ecard_json_response(
            "/utilities/config",
            json!({
                "success": true,
                "resultData": {
                    "location": {
                        "bigArea": "",
                        "bigAreaName": "",
                        "area": "1",
                        "areaName": "主校区",
                        "building": "2",
                        "buildingName": "松园2号楼",
                        "unit": "",
                        "unitName": "",
                        "level": "3",
                        "levelName": "照明房间",
                        "room": "1-2--3-301",
                        "roomName": "301房间",
                        "subArea": "",
                        "subAreaName": ""
                    }
                }
            }),
        ))
        .unwrap();
        assert_eq!(resolved.room.as_ref(), "1-2--3-301");
        assert_eq!(
            resolved.name.as_deref(),
            Some("主校区松园2号楼照明房间301房间")
        );

        let unnamed = parse_default_room_response(ecard_json_response(
            "/utilities/config",
            json!({
                "success": true,
                "resultData": {
                    "location": {
                        "room": "1-2--3-302",
                        "areaName": "   "
                    }
                }
            }),
        ))
        .unwrap();
        assert_eq!(unnamed.room.as_ref(), "1-2--3-302");
        assert_eq!(unnamed.name, None);
    }

    #[test]
    fn energy_account_parses_templates_decimals_fallbacks_and_roundtrips() {
        let room: RoomId = "1-2--3-301".parse().unwrap();
        let account = parse_energy_account_response(
            ecard_json_response(
                "/utilities/account",
                json!({
                    "success": true,
                    "message": "成功",
                    "resultData": {
                        "balance": "12.34",
                        "utilityStatus": "1",
                        "utilityStatusName": " 正常 ",
                        "accStatus": "1",
                        "accStatusName": "正常",
                        "templateList": [
                            {"code": "ykt_balance", "value": "12.34"},
                            {"code": "price", "value": "0.57"},
                            {"code": "quantity", "value": "86.5"},
                            {"code": "balance", "value": null}
                        ],
                        "supportDetails": false
                    }
                }),
            ),
            room,
            Some("主校区松园2号楼照明房间301房间".into()),
        )
        .unwrap();

        assert_eq!(account.room.as_ref(), "1-2--3-301");
        assert_eq!(
            account.room_name.as_deref(),
            Some("主校区松园2号楼照明房间301房间")
        );
        assert!((account.remaining_energy - 86.5).abs() < f64::EPSILON);
        assert_eq!(account.unit_price.unwrap().to_string(), "0.57");
        assert_eq!(account.electricity_balance, None);
        assert_eq!(account.card_balance.unwrap().to_string(), "12.34");
        assert_eq!(account.status.as_deref(), Some("正常"));

        let serialized = serde_json::to_value(&account).unwrap();
        assert_eq!(
            serialized,
            json!({
                "room": "1-2--3-301",
                "roomName": "主校区松园2号楼照明房间301房间",
                "remainingEnergy": 86.5,
                "unitPrice": "0.57",
                "electricityBalance": null,
                "cardBalance": "12.34",
                "status": "正常"
            })
        );
        let restored: EnergyAccount = serde_json::from_value(serialized).unwrap();
        assert_eq!(restored, account);

        // 顶层 balance 为空时回退读取 ykt_balance，且支持按金额计费的电费余额。
        let fallback_room: RoomId = "1-2--3-302".parse().unwrap();
        let fallback = parse_energy_account_response(
            ecard_json_response(
                "/utilities/account",
                json!({
                    "success": true,
                    "resultData": {
                        "balance": "",
                        "utilityStatusName": "  ",
                        "templateList": [
                            {"code": "ykt_balance", "value": "30.00"},
                            {"code": "quantity", "value": 42.0},
                            {"code": "balance", "value": "23.94"}
                        ]
                    }
                }),
            ),
            fallback_room,
            None,
        )
        .unwrap();
        assert_eq!(fallback.unit_price, None);
        assert_eq!(fallback.electricity_balance.unwrap().to_string(), "23.94");
        assert_eq!(fallback.card_balance.unwrap().to_string(), "30.00");
        assert_eq!(fallback.status, None);
    }

    #[test]
    fn energy_account_surfaces_remote_and_decode_errors_with_context() {
        let room: RoomId = "1-2--3-301".parse().unwrap();

        let remote_err = parse_energy_account_response(
            ecard_json_response(
                "/utilities/account",
                json!({"success": false, "message": "房间不存在", "resultData": null}),
            ),
            room.clone(),
            None,
        )
        .unwrap_err();
        assert!(remote_err.is_remote());
        assert_eq!(remote_err.message(), "房间不存在");
        assert!(remote_err.request().is_some());

        for malformed in [
            json!({"success": true, "resultData": {"templateList": []}}),
            json!({"success": true, "resultData": {"templateList": [{"code": "quantity", "value": "N/A"}]}}),
            json!({"success": true, "resultData": {"templateList": [{"code": "quantity", "value": "10.0"}, {"code": "price", "value": "NaN"}]}}),
        ] {
            let error = parse_energy_account_response(
                ecard_json_response("/utilities/account", malformed),
                room.clone(),
                None,
            )
            .unwrap_err();
            assert!(error.is_decode());
            assert!(error.request().is_some());
        }
    }

    #[tokio::test]
    async fn unauthenticated_energy_queries_are_rejected() {
        let cas = CasClient::new("", "", "test-device").unwrap();
        let ecard = EcardClient::new(cas).unwrap();
        assert!(
            ecard
                .energy_account(None)
                .await
                .unwrap_err()
                .is_not_authenticated()
        );
        assert!(
            ecard
                .remaining_energy(None)
                .await
                .unwrap_err()
                .is_not_authenticated()
        );
    }
}
