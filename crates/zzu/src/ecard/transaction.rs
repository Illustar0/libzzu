use jiff::{
    Zoned,
    civil::{Date, DateTime},
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::{ECARD_URL, EcardClient};
use crate::{Decimal, Error, Result, http};

/// 校园卡账单流水分页查询的默认每页条数。
pub const DEFAULT_TRANSACTION_PAGE_SIZE: u32 = 30;

/// 校园卡交易流水的业务类别。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TransactionKind {
    /// 消费扣款（如食堂餐饮、商超购物、宿舍电控购电）。
    Consumption,
    /// 充值入账（如微信、支付宝或银行卡转账充值）。
    Recharge,
    /// 补助发放。
    Subsidy,
}

impl TransactionKind {
    const fn upstream_code(self) -> &'static str {
        match self {
            Self::Consumption => "1",
            Self::Recharge => "2",
            Self::Subsidy => "3",
        }
    }
}

/// 校园卡账单流水筛选条件（起止日期均含当日）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionQuery {
    /// 查询起始公历日期（含当日）。
    pub start_date: Date,
    /// 查询截止公历日期（含当日）。
    pub end_date: Date,
    /// 交易类别筛选；为 `None` 时查询全部类别（消费、充值与补助）。
    pub kind: Option<TransactionKind>,
}

impl TransactionQuery {
    /// 创建查询指定闭区间 `[start_date, end_date]` 内全部类别流水的筛选条件。
    #[must_use]
    pub const fn new(start_date: Date, end_date: Date) -> Self {
        Self {
            start_date,
            end_date,
            kind: None,
        }
    }
}

/// 校园卡单笔交易账单记录。
///
/// 反序列化时同时兼容上游接口原始字段（`journo`、北京时间 `YYYY-MM-DD HH:MM:SS` 的 `date`、
/// `amt`、字符串 `"0"`/`"1"` 的 `isRefund`）与本库序列化的标准 `camelCase` / RFC 9557 JSON 格式。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transaction {
    /// 校园卡交易流水号（映射自上游 `journo`）。
    #[serde(alias = "journo")]
    pub id: String,
    /// 交易发生时间（`Asia/Shanghai` 时区）。
    #[serde(alias = "date", deserialize_with = "deserialize_transaction_time")]
    pub time: Zoned,
    /// 交易金额（单位：元；消费通常为负数，充值与补助通常为正数）。
    #[serde(alias = "amt", deserialize_with = "crate::money::deserialize_decimal")]
    pub amount: Decimal,
    /// 交易商户或入账渠道名称（如食堂档口、主校区电控、微信支付转账充值）。
    #[serde(default)]
    pub merchant_name: String,
    /// 交易摘要说明（如购电房间描述或移动端交易备注）。
    #[serde(default)]
    pub summary: String,
    /// 是否为退款记录。
    #[serde(default, deserialize_with = "deserialize_refund_flag")]
    pub is_refund: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TradeListRequest<'a> {
    from_date: String,
    to_date: String,
    trade_type: &'a str,
    page_no: u32,
    page_size: u32,
}

impl EcardClient {
    /// 分页查询符合筛选条件 `query` 的校园卡交易账单流水。
    ///
    /// `page` 从 `1` 开始（内部自动换算为上游从 `0` 起算的 `pageNo`）；`page_size` 为 `None` 时
    /// 使用 [`DEFAULT_TRANSACTION_PAGE_SIZE`]（`30`）。上游接口不返回总条数，翻页至返回列表长度
    /// 小于 `page_size`（或为空列表）即表示已到达末页。
    ///
    /// 当 [`Self::recharge_energy`] 因超时或取消导致扣款状态未知时，可通过本方法核对最新电控扣款流水。
    ///
    /// # Errors
    ///
    /// - `page == 0`、`page_size == Some(0)` 或 `query.start_date > query.end_date` 时返回 [`Error::InvalidInput`]。
    /// - 客户端尚未登录或底层 CAS 令牌不可用时返回 [`Error::Authentication`]。
    /// - 会话换新、网络传输、远端业务校验或流水字段解析失败时返回错误。
    pub async fn transactions(
        &self,
        query: &TransactionQuery,
        page: u32,
        page_size: Option<u32>,
    ) -> Result<Vec<Transaction>> {
        let request_body = build_trade_list_request(query, page, page_size)?;
        let token = self.access_token().await?;
        let response = http::json(
            self.http
                .post(format!("{ECARD_URL}/user/tradeList"))
                .header("Authorization", token)
                .json(&request_body),
        )
        .await?;
        parse_transactions_response(response)
    }
}

fn build_trade_list_request(
    query: &TransactionQuery,
    page: u32,
    page_size: Option<u32>,
) -> Result<TradeListRequest<'static>> {
    if page == 0 {
        return Err(Error::invalid_input("页码 page 必须从 1 开始".into()));
    }
    let page_size = page_size.unwrap_or(DEFAULT_TRANSACTION_PAGE_SIZE);
    if page_size == 0 {
        return Err(Error::invalid_input("每页条数 page_size 必须大于 0".into()));
    }
    if query.start_date > query.end_date {
        return Err(Error::invalid_input(
            "起始日期 start_date 不能晚于截止日期 end_date".into(),
        ));
    }
    let trade_type = query.kind.map_or("1,2,3", TransactionKind::upstream_code);
    Ok(TradeListRequest {
        from_date: query.start_date.to_string(),
        to_date: query.end_date.to_string(),
        trade_type,
        page_no: page - 1,
        page_size,
    })
}

fn parse_transactions_response(response: http::JsonResponse) -> Result<Vec<Transaction>> {
    let items: Option<Vec<Transaction>> = http::ecard_data(response)?;
    Ok(items.unwrap_or_default())
}

fn deserialize_transaction_time<'de, D>(deserializer: D) -> std::result::Result<Zoned, D::Error>
where
    D: Deserializer<'de>,
{
    let text = String::deserialize(deserializer)?;
    let trimmed = text.trim();
    if let Ok(zoned) = trimmed.parse::<Zoned>() {
        return Ok(zoned);
    }
    DateTime::strptime("%Y-%m-%d %H:%M:%S", trimmed)
        .and_then(|datetime| datetime.in_tz("Asia/Shanghai"))
        .map_err(serde::de::Error::custom)
}

fn deserialize_refund_flag<'de, D>(deserializer: D) -> std::result::Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::Bool(flag) => Ok(flag),
        Value::Null => Ok(false),
        Value::Number(number) => match number.as_i64() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => Err(serde::de::Error::custom("isRefund 数字取值仅支持 0 或 1")),
        },
        Value::String(text) => match text.trim() {
            "" | "0" | "false" => Ok(false),
            "1" | "true" => Ok(true),
            _ => Err(serde::de::Error::custom(
                "isRefund 字符串取值仅支持 0/1 或布尔文本",
            )),
        },
        _ => Err(serde::de::Error::custom(
            "isRefund 必须为布尔值、0/1 或字符串",
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::{RequestContext, auth::CasClient};

    fn trade_list_response(value: Value) -> http::JsonResponse {
        http::JsonResponse::new(
            value,
            Arc::new(RequestContext {
                method: "POST".into(),
                url: format!("{ECARD_URL}/user/tradeList"),
                status_code: Some(200),
                error_code: None,
                response_body: None,
            }),
        )
    }

    #[test]
    fn trade_list_request_validates_bounds_and_maps_zero_based_page() {
        let start: Date = "2026-11-01".parse().unwrap();
        let end: Date = "2026-11-27".parse().unwrap();
        let query = TransactionQuery::new(start, end);

        let default_req = build_trade_list_request(&query, 1, None).unwrap();
        assert_eq!(
            serde_json::to_value(&default_req).unwrap(),
            json!({
                "fromDate": "2026-11-01",
                "toDate": "2026-11-27",
                "tradeType": "1,2,3",
                "pageNo": 0,
                "pageSize": 30,
            })
        );

        for (kind, expected_code) in [
            (TransactionKind::Consumption, "1"),
            (TransactionKind::Recharge, "2"),
            (TransactionKind::Subsidy, "3"),
        ] {
            let typed = TransactionQuery {
                kind: Some(kind),
                ..query.clone()
            };
            let req = build_trade_list_request(&typed, 3, Some(15)).unwrap();
            assert_eq!(req.trade_type, expected_code);
            assert_eq!(req.page_no, 2);
            assert_eq!(req.page_size, 15);
        }

        assert!(
            build_trade_list_request(&query, 0, None)
                .unwrap_err()
                .is_invalid_input()
        );
        assert!(
            build_trade_list_request(&query, 1, Some(0))
                .unwrap_err()
                .is_invalid_input()
        );
        let reversed = TransactionQuery::new(end, start);
        assert!(
            build_trade_list_request(&reversed, 1, None)
                .unwrap_err()
                .is_invalid_input()
        );
    }

    #[test]
    fn parse_transactions_normalizes_upstream_records_and_roundtrips() {
        let response = trade_list_response(json!({
            "success": true,
            "message": "成功",
            "resultData": [
                {
                    "summary": "缴电费_主校区某园1号楼照明房间101房间",
                    "merchantName": "主校区电控",
                    "date": "2026-11-25 16:02:37",
                    "amt": "-60.00",
                    "isRefund": "0",
                    "operation": false,
                    "journo": "1442787548706373664"
                },
                {
                    "summary": "移动端交易",
                    "merchantName": "微信支付转账充值",
                    "date": "2026-11-25 14:18:22",
                    "amt": "50.00",
                    "isRefund": "1",
                    "journo": "1442761632294895629"
                }
            ],
            "code": ""
        }));

        let transactions = parse_transactions_response(response).unwrap();
        assert_eq!(transactions.len(), 2);
        assert_eq!(transactions[0].id, "1442787548706373664");
        assert_eq!(
            transactions[0].time.to_string(),
            "2026-11-25T16:02:37+08:00[Asia/Shanghai]"
        );
        assert_eq!(transactions[0].amount.to_string(), "-60.00");
        assert_eq!(transactions[0].merchant_name, "主校区电控");
        assert!(!transactions[0].is_refund);
        assert!(transactions[1].is_refund);

        let serialized = serde_json::to_value(&transactions[0]).unwrap();
        assert_eq!(
            serialized,
            json!({
                "id": "1442787548706373664",
                "time": "2026-11-25T16:02:37+08:00[Asia/Shanghai]",
                "amount": "-60.00",
                "merchantName": "主校区电控",
                "summary": "缴电费_主校区某园1号楼照明房间101房间",
                "isRefund": false,
            })
        );
        let roundtripped: Transaction = serde_json::from_value(serialized).unwrap();
        assert_eq!(roundtripped, transactions[0]);

        let null_data = trade_list_response(json!({
            "success": true,
            "message": "成功",
            "resultData": null,
        }));
        assert_eq!(parse_transactions_response(null_data).unwrap(), Vec::new());
    }

    #[test]
    fn parse_transactions_surfaces_remote_and_decode_errors_with_context() {
        let remote_failure = trade_list_response(json!({
            "success": false,
            "message": "查询区间超出限制",
            "resultData": null,
            "code": "valid_error_1004"
        }));
        let error = parse_transactions_response(remote_failure).unwrap_err();
        assert!(error.is_remote());
        assert_eq!(error.message(), "查询区间超出限制");
        assert!(error.request().is_some());

        for bad_item in [
            json!({"journo": "1", "date": "bad-date", "amt": "-1.00", "isRefund": "0"}),
            json!({"journo": "1", "date": "2026-11-25 10:00:00", "amt": "NaN", "isRefund": "0"}),
            json!({"journo": "1", "date": "2026-11-25 10:00:00", "amt": "-1.00", "isRefund": "2"}),
        ] {
            let malformed = trade_list_response(json!({
                "success": true,
                "resultData": [bad_item],
            }));
            let error = parse_transactions_response(malformed).unwrap_err();
            assert!(error.is_decode());
            assert!(error.request().is_some());
        }
    }

    #[tokio::test]
    async fn transactions_checks_inputs_before_authentication() {
        let cas = CasClient::new("", "", "test-device").unwrap();
        let ecard = EcardClient::new(cas).unwrap();
        let start: Date = "2026-11-10".parse().unwrap();
        let end: Date = "2026-11-01".parse().unwrap();
        assert!(
            ecard
                .transactions(&TransactionQuery::new(start, end), 1, None)
                .await
                .unwrap_err()
                .is_invalid_input()
        );
        assert!(
            ecard
                .transactions(&TransactionQuery::new(end, start), 1, None)
                .await
                .unwrap_err()
                .is_not_authenticated()
        );
    }
}
