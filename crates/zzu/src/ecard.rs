//! 校园卡会话认证、余额与在线校园码、账单流水查询、宿舍层级检索以及电费查询与充值。

use std::{
    collections::BTreeMap,
    fmt,
    str::FromStr,
    sync::{Mutex, MutexGuard},
};

use jiff::Timestamp;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;

use crate::{Error, Result, auth::CasClient, crypto, http};

mod code;
mod energy;
mod recharge;
mod transaction;

pub use code::PaymentCode;
pub use energy::EnergyAccount;
pub use recharge::{
    PaymentAction, PaymentChannel, PaymentContext, PaymentDevice, RechargeConfig, RechargeOrder,
};
pub use transaction::{
    DEFAULT_TRANSACTION_PAGE_SIZE, Transaction, TransactionKind, TransactionQuery,
};

const ECARD_URL: &str = "https://ecard.v.zzu.edu.cn/server";
const SESSION_MAX_AGE_SECONDS: i64 = 45 * 60;
/// 电费页面默认的 `appCode`，前端在配置、账户查询与扣款请求中作为 `appId` 发送。
const ELECTRIC_APP_ID: &str = "electric";

/// 宿舍房间号，格式为 `area-building--level-room`（四段均非空且不含 `-`）。
///
/// # Examples
///
/// ```
/// use zzu::ecard::RoomId;
///
/// let room: RoomId = "1-2--3-301".parse()?;
/// assert_eq!(room.as_ref(), "1-2--3-301");
/// assert!("invalid-room".parse::<RoomId>().is_err());
/// # Ok::<(), zzu::Error>(())
/// ```
#[derive(Clone, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(!try_from, !into)
)]
#[serde(try_from = "String", into = "String")]
pub struct RoomId(
    #[cfg_attr(
        feature = "schemars",
        schemars(regex(pattern = r"^[^-]+-[^-]+--[^-]+-[^-]+$"))
    )]
    String,
);

impl FromStr for RoomId {
    type Err = Error;

    fn from_str(room: &str) -> Result<Self> {
        let invalid = || Error::invalid_input("房间格式应为 area-building--level-room".into());
        let (building, room_part) = room.split_once("--").ok_or_else(invalid)?;
        let (area, building) = building.split_once('-').ok_or_else(invalid)?;
        let (level, number) = room_part.split_once('-').ok_or_else(invalid)?;
        if [area, building, level, number]
            .iter()
            .any(|part| part.is_empty() || part.contains('-'))
        {
            return Err(invalid());
        }
        Ok(Self(room.into()))
    }
}

impl TryFrom<String> for RoomId {
    type Error = Error;

    fn try_from(room: String) -> Result<Self> {
        room.parse()
    }
}

impl From<RoomId> for String {
    fn from(room: RoomId) -> Self {
        room.0
    }
}

impl AsRef<str> for RoomId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RoomId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl RoomId {
    /// 构造电费账户查询与扣款接口的房间位置参数：`area`、`building`、`level` 拆为单段编码
    /// （如 `"1-2--3-301"` 对应 `"1"`、`"2"`、`"3"`），`room` 保留完整房间号，并附带 `appId`。
    fn location(&self) -> LocationRequest<'_> {
        let (building_path, room_path) = self.0.split_once("--").expect("validated room");
        let (area, building) = building_path.split_once('-').expect("validated building");
        let (level, _) = room_path.split_once('-').expect("validated floor");
        LocationRequest {
            area,
            building,
            level,
            room: &self.0,
            app_id: Some(ELECTRIC_APP_ID),
            ..LocationRequest::default()
        }
    }
}

struct Session {
    access_token: String,
    cas_token: String,
    /// 使用墙钟秒数：单调时钟在设备休眠期间可能停走，会把过期会话判断为仍然有效。
    obtained_at: i64,
}

impl Session {
    fn is_current(&self, cas_token: &str) -> bool {
        self.cas_token == cas_token
            && Timestamp::now().as_second() - self.obtained_at < SESSION_MAX_AGE_SECONDS
    }
}

#[derive(Default)]
struct EcardState {
    session: Option<Session>,
    default_room: Option<energy::DefaultRoom>,
    qr_refresh_seconds: Option<u32>,
}

/// 校园卡与宿舍电控服务客户端。
///
/// 共享 [`CasClient`] 的统一身份认证会话。内部仅在读写会话与默认房间缓存时短暂持锁，发起网络请求前即释放状态锁；
/// 查询与收银台预支付方法均以 `&self` 并发执行，会话变更（[`Self::login`]、[`Self::logout`]）与扣款、下单
/// （[`Self::recharge_energy`]、[`Self::create_recharge_order`]）仍要求 `&mut self`。
///
/// 客户端不启动后台刷新任务：每次请求前按需检查校园卡 `access_token`，满 45 分钟或底层 CAS 令牌变更时自动换取新令牌。
/// 令牌换新单独互斥，多个并发请求等待并复用同一次换新结果。
pub struct EcardClient {
    http: Client,
    cas: CasClient,
    state: Mutex<EcardState>,
    refresh_lock: AsyncMutex<()>,
}

impl EcardClient {
    /// 使用共享的 [`CasClient`] 与默认 HTTP 配置创建校园卡客户端。
    ///
    /// # Errors
    ///
    /// 默认 HTTP 客户端初始化失败时返回错误。
    pub fn new(cas: CasClient) -> Result<Self> {
        Ok(Self::with_http_client(cas, http::client()?))
    }

    /// 使用共享的 [`CasClient`] 与自定义 [`reqwest::Client`] 创建校园卡客户端。
    ///
    /// `client` 必须禁用自动重定向（如通过 [`crate::http_client_builder`] 构建），以便捕获单点登录跳转中的 `tid` 参数。
    #[must_use]
    pub fn with_http_client(cas: CasClient, client: Client) -> Self {
        Self {
            http: client,
            cas,
            state: Mutex::new(EcardState::default()),
            refresh_lock: AsyncMutex::new(()),
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, EcardState> {
        self.state.lock().expect("ecard state poisoned")
    }

    /// 判断当前校园卡客户端是否已建立本地会话。
    #[must_use]
    pub fn is_logged_in(&self) -> bool {
        self.lock_state().session.is_some()
    }

    /// 使用共享的 [`CasClient`] 令牌换取校园卡服务 `access_token` 并建立会话。
    ///
    /// # Errors
    ///
    /// - 底层 [`CasClient`] 尚未登录或令牌不可用时返回 [`Error::Authentication`]。
    /// - 单点登录跳转响应缺少 `Location` 或 `tid`、令牌交换失败或网络异常时返回错误。
    pub async fn login(&mut self) -> Result<()> {
        self.logout();
        let cas_token = self.cas.user_token().await?;
        let session = fetch_session(&self.http, cas_token).await?;
        self.lock_state().session = Some(session);
        Ok(())
    }

    fn cached_access_token(&self, cas_token: &str) -> Result<Option<String>> {
        let state = self.lock_state();
        let session = state.session.as_ref().ok_or(Error::not_authenticated())?;
        Ok(session
            .is_current(cas_token)
            .then(|| session.access_token.clone()))
    }

    async fn access_token(&self) -> Result<String> {
        if !self.is_logged_in() {
            return Err(Error::not_authenticated());
        }
        let cas_token = self.cas.user_token().await?;
        if let Some(token) = self.cached_access_token(&cas_token)? {
            return Ok(token);
        }
        let _refresh = self.refresh_lock.lock().await;
        let cas_token = self.cas.user_token().await?;
        if let Some(token) = self.cached_access_token(&cas_token)? {
            return Ok(token);
        }
        let session = fetch_session(&self.http, cas_token).await?;
        let token = session.access_token.clone();
        self.lock_state().session = Some(session);
        Ok(token)
    }

    /// 查询当前账号的校园卡余额（单位：元）。
    ///
    /// # Errors
    ///
    /// - 客户端尚未登录或底层 CAS 令牌不可用时返回 [`Error::Authentication`]。
    /// - 获取临时公钥、SM2/SM4 加解密、网络请求、远端业务校验或余额字段解析失败时返回错误。
    pub async fn balance(&self) -> Result<crate::Decimal> {
        let token = self.access_token().await?;
        let encryption = self.encryption(&token).await?;
        let encrypt_id = encryption.id_header()?;
        let sm4_key = crypto::random_sm4_key();
        let encrypted_sm4_key = crypto::sm2_encrypt(&encryption.public_key, sm4_key.as_bytes())?;
        let response = http::json(
            self.http
                .post(format!("{ECARD_URL}/user/info"))
                .header("Authorization", token)
                .header("X-Encrypt-Id", encrypt_id)
                .header("X-Sm4-Key", encrypted_sm4_key)
                .header("Content-Type", "application/json"),
        )
        .await?;
        parse_balance_response(response, &sm4_key)
    }

    /// 按上级位置编码逐级检索电控位置目录，返回 `id -> 名称` 的有序映射。
    ///
    /// `parent` 支持以下四种层级格式（其中 `area`、`building`、`level` 各段均非空且不含 `-`）：
    /// - `""`（空字符串）：查询区域列表（`locationType = "bigArea"`）；
    /// - `"area"`（如 `"1"`）：查询指定区域下的楼栋列表（`locationType = "building"`）；
    /// - `"area-building"`（如 `"1-2"`）：查询指定楼栋下的单元/楼层列表（`locationType = "unit"`）；
    /// - `"area-building--level"`（如 `"1-2--3"`）：查询指定楼层下的房间列表（`locationType = "room"`，返回的键格式为 `area-building--level-room`，可解析为 [`RoomId`]）。
    ///
    /// # Errors
    ///
    /// - `parent` 不符合上述四种格式（如包含空段、多余连字符或传入完整四段房间号）时返回 [`Error::InvalidInput`]。
    /// - 客户端尚未登录、网络请求失败或远端返回错误时返回错误。
    pub async fn locations(&self, parent: &str) -> Result<BTreeMap<String, String>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Locations {
            #[serde(default)]
            location_list: Vec<Location>,
        }
        #[derive(Deserialize)]
        struct Location {
            id: String,
            name: String,
        }
        let location = location_query(parent)?;
        let token = self.access_token().await?;
        let request = self
            .http
            .post(format!("{ECARD_URL}/utilities/location"))
            .header("Authorization", token)
            .json(&location);
        let response = http::json(request).await?;
        let locations: Locations = http::ecard_data(response)?;
        Ok(locations
            .location_list
            .into_iter()
            .map(|location| (location.id, location.name))
            .collect())
    }

    /// 使用校园卡支付密码从卡余额扣款，为指定宿舍房间充值电费。
    ///
    /// `amount` 为正整数充值金额（单位：元）。提交前会申请一次性交易号与服务端临时 SM2 公钥，
    /// 用公钥加密支付参数与本地随机生成的 SM4 密钥；服务端返回加密响应时再用该 SM4 密钥解密。
    ///
    /// # Errors
    ///
    /// - `amount == 0` 或 `payment_password` 为空字符串时返回 [`Error::InvalidInput`]。
    /// - 客户端尚未登录、公钥解析或加解密失败、网络异常或远端拒绝扣款时返回错误。
    /// - 支付响应缺失 `success` 确认字段时返回 [`Error::Decode`]，加密响应解密失败时返回 [`Error::Crypto`]；
    ///   两者均代表扣款结果未知，调用方须先核对余额再决定是否重试。
    pub async fn recharge_energy(
        &mut self,
        payment_password: &str,
        amount: u32,
        room: &RoomId,
    ) -> Result<()> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Payment<'a> {
            #[serde(flatten)]
            location: LocationRequest<'a>,
            pay_code: &'a str,
            password: &'a str,
            amt: String,
            timestamp: i64,
            customfield: BTreeMap<String, String>,
        }
        if amount == 0 || payment_password.is_empty() {
            return Err(Error::invalid_input(
                "充值金额必须大于 0，支付密码不能为空".into(),
            ));
        }
        let token = self.access_token().await?;
        let encryption = self.encryption(&token).await?;
        let payment = Payment {
            location: room.location(),
            pay_code: "06",
            password: payment_password,
            amt: amount.to_string(),
            timestamp: Timestamp::now().as_millisecond(),
            customfield: BTreeMap::new(),
        };
        let sm4_key = crypto::random_sm4_key();
        let body = json!({
            "id": encryption.id,
            "params": crypto::sm2_encrypt(&encryption.public_key, &serde_json::to_vec(&payment)?)?,
            "sm4Key": crypto::sm2_encrypt(&encryption.public_key, sm4_key.as_bytes())?,
        });
        let transaction_id = self.transaction_id(&token).await?;
        let response = http::json(
            self.http
                .post(format!("{ECARD_URL}/utilities/pay"))
                .header("Authorization", token)
                .header("transactionId", transaction_id)
                .json(&body),
        )
        .await?;
        let response = response.map_value(|body| decrypt_response(body, &sm4_key))?;
        match response.get("success").and_then(Value::as_bool) {
            Some(true) => Ok(()),
            Some(false) => Err(http::remote_error(
                &response.value,
                response.context,
                "code",
            )),
            None => Err(Error::decode_message(
                "充值响应缺少 success，支付结果未知，请查询余额后再决定是否重试".into(),
            )
            .with_request(response.context)),
        }
    }

    /// 申请服务端临时 SM2 公钥与加密会话标识（`POST /auth/getEncrypt`）。
    async fn encryption(&self, token: &str) -> Result<Encryption> {
        let request = self
            .http
            .post(format!("{ECARD_URL}/auth/getEncrypt"))
            .header("Authorization", token)
            .json(&json!({"protocolVersion":2}));
        let response = http::json(request).await?;
        let context = response.context.clone();
        let encryption: Encryption = http::ecard_data(response)?;
        encryption
            .id_header()
            .map_err(|error| error.with_request(context))?;
        Ok(encryption)
    }

    /// 申请 `/utilities/pay`、`/recharge/pay` 等扣款接口请求头 `transactionId` 所需的一次性交易号。
    async fn transaction_id(&self, token: &str) -> Result<String> {
        let datetime = Timestamp::now()
            .in_tz("Asia/Shanghai")
            .map_err(Error::decode)?
            .strftime("%F %T")
            .to_string();
        let response = http::json(
            self.http
                .post(format!("{ECARD_URL}/consume/transaction"))
                .header("Authorization", token)
                .json(&json!({"datetime":datetime})),
        )
        .await?;
        parse_transaction_id_response(response)
    }

    /// 清除本地缓存的校园卡会话与默认房间信息（不影响底层共享的 [`CasClient`]）。
    pub fn logout(&mut self) {
        *self.lock_state() = EcardState::default();
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Encryption {
    id: Value,
    public_key: String,
}

impl Encryption {
    /// 将 `getEncrypt` 返回的 `id` 转为 `X-Encrypt-Id` 请求头值，避免对 JSON 字符串二次加引号。
    fn id_header(&self) -> Result<String> {
        match &self.id {
            Value::String(id) if !id.is_empty() => Ok(id.clone()),
            Value::Number(id) => Ok(id.to_string()),
            _ => Err(Error::decode_message("加密配置缺少有效的 id".into())),
        }
    }
}

/// 解密并解析 `/user/info` 响应中的校园卡余额（单位：元）。
fn parse_balance_response(response: http::JsonResponse, sm4_key: &str) -> Result<crate::Decimal> {
    #[derive(Deserialize)]
    struct UserInfo {
        user: Option<User>,
    }
    #[derive(Deserialize)]
    struct User {
        balance: Value,
    }
    let response = response.map_value(|body| decrypt_response(body, sm4_key))?;
    let context = response.context.clone();
    let info: UserInfo = http::ecard_data(response)?;
    let user = info.user.ok_or_else(|| {
        Error::decode_message("校园卡用户信息缺少 user".into()).with_request(context.clone())
    })?;
    http::parse_money(&user.balance, "balance").map_err(|error| error.with_request(context))
}

/// 还原校园卡加密响应：与前端一致，`success` 不为 `true` 且带非空 `data` 时，用请求时生成的 SM4 密钥解密 `data` 得到实际响应。
fn decrypt_response(response: &Value, sm4_key: &str) -> Result<Value> {
    let is_success = response.get("success").and_then(Value::as_bool) == Some(true);
    match response.get("data").and_then(Value::as_str) {
        Some(data) if !is_success && !data.is_empty() => Ok(serde_json::from_slice(
            &crypto::sm4_decrypt(data, sm4_key)?,
        )?),
        _ => Ok(response.clone()),
    }
}

fn parse_transaction_id_response(response: http::JsonResponse) -> Result<String> {
    let context = response.context.clone();
    let transaction_id: String = http::ecard_data(response)?;
    if transaction_id.is_empty() {
        return Err(Error::decode_message("扣款交易号为空".into()).with_request(context));
    }
    Ok(transaction_id)
}

async fn fetch_session(http: &Client, cas_token: String) -> Result<Session> {
    let request = http.get(format!("{ECARD_URL}/auth/host/open")).query(&[
        ("host", "11"),
        ("org", "2"),
        ("token", cas_token.as_str()),
    ]);
    let response = http::send(request).await?.allow_redirect()?;
    let tid = parse_session_redirect(&response)?;
    let response = http::json(
        http.post(format!("{ECARD_URL}/auth/getToken"))
            .json(&json!({"tid":tid})),
    )
    .await?;
    parse_session_tokens(response, cas_token)
}

fn parse_session_redirect(response: &http::Response) -> Result<String> {
    let location = response
        .headers
        .get("location")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            Error::decode_message("校园卡响应缺少 Location".into())
                .with_request(response.context.clone())
        })?;
    let redirect = response
        .url
        .join(location)
        .map_err(|error| Error::decode(error).with_request(response.context.clone()))?;
    redirect
        .query_pairs()
        .find(|(key, _)| key == "tid")
        .map(|(_, value)| value.into_owned())
        .filter(|tid| !tid.is_empty())
        .ok_or_else(|| {
            Error::decode_message("校园卡重定向缺少 tid".into())
                .with_request(response.context.clone())
        })
}

fn parse_session_tokens(response: http::JsonResponse, cas_token: String) -> Result<Session> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TokenData {
        access_token: String,
        refresh_token: String,
    }
    let context = response.context.clone();
    let tokens: TokenData = http::ecard_data(response)?;
    if tokens.access_token.is_empty() || tokens.refresh_token.is_empty() {
        return Err(Error::decode_message("校园卡 Token 为空".into()).with_request(context));
    }
    Ok(Session {
        access_token: tokens.access_token,
        cas_token,
        obtained_at: Timestamp::now().as_second(),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LocationRequest<'a> {
    utility_type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    location_type: Option<&'a str>,
    big_area: &'a str,
    area: &'a str,
    building: &'a str,
    unit: &'a str,
    level: &'a str,
    room: &'a str,
    sub_area: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_id: Option<&'a str>,
}

impl Default for LocationRequest<'_> {
    fn default() -> Self {
        Self {
            utility_type: "electric",
            location_type: None,
            big_area: "",
            area: "",
            building: "",
            unit: "",
            level: "",
            room: "",
            sub_area: "",
            app_id: None,
        }
    }
}

/// 构造 `/utilities/location` 目录检索请求：将 `parent` 拆为 `area`、`building`、`level` 单段编码，并按层级设置 `locationType`。
fn location_query(parent: &str) -> Result<LocationRequest<'_>> {
    let invalid = || {
        Error::invalid_input("位置格式应为空、area、area-building 或 area-building--level".into())
    };
    let (area, building, level, kind) = if parent.is_empty() {
        ("", "", "", "bigArea")
    } else if let Some((building, level)) = parent.split_once("--") {
        let (area, building_number) = building.split_once('-').ok_or_else(invalid)?;
        if [area, building_number, level]
            .iter()
            .any(|part| part.is_empty() || part.contains('-'))
        {
            return Err(invalid());
        }
        (area, building_number, level, "room")
    } else if let Some((area, building)) = parent.split_once('-') {
        if area.is_empty() || building.is_empty() || building.contains('-') {
            return Err(invalid());
        }
        (area, building, "", "unit")
    } else {
        (parent, "", "", "building")
    };
    Ok(LocationRequest {
        area,
        building,
        level,
        location_type: Some(kind),
        ..LocationRequest::default()
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use base64::{Engine, engine::general_purpose::STANDARD};
    use ecb::cipher::{BlockModeEncrypt, KeyInit, block_padding::Pkcs7};

    use super::*;
    use crate::RequestContext;

    fn user_info_response(value: Value) -> http::JsonResponse {
        http::JsonResponse::new(
            value,
            Arc::new(RequestContext {
                method: "POST".into(),
                url: format!("{ECARD_URL}/user/info"),
                status_code: Some(200),
                error_code: None,
                response_body: None,
            }),
        )
    }

    fn encrypt_payload(payload: &Value, sm4_key: &str) -> Value {
        let key_bytes: &[u8; 16] = sm4_key.as_bytes().try_into().unwrap();
        let plaintext = serde_json::to_vec(payload).unwrap();
        let ciphertext = ecb::Encryptor::<sm4::Sm4>::new(key_bytes.into())
            .encrypt_padded_vec::<Pkcs7>(&plaintext);
        json!({"data": STANDARD.encode(ciphertext)})
    }

    #[test]
    fn room_and_location_ids_are_validated_before_requests() {
        let room: RoomId = "1-2--3-301".parse().unwrap();
        assert_eq!(
            serde_json::to_value(room.location()).unwrap(),
            json!({
                "utilityType": "electric",
                "bigArea": "",
                "area": "1",
                "building": "2",
                "unit": "",
                "level": "3",
                "room": "1-2--3-301",
                "subArea": "",
                "appId": "electric",
            }),
        );
        for invalid in ["", "1-2-3-301", "1--3-301", "1-2--3-", "1-2--3-301-x"] {
            assert!(invalid.parse::<RoomId>().is_err());
        }
        for (parent, kind, area, building, level) in [
            ("", "bigArea", "", "", ""),
            ("1", "building", "1", "", ""),
            ("1-2", "unit", "1", "2", ""),
            ("1-2--3", "room", "1", "2", "3"),
        ] {
            let location = location_query(parent).unwrap();
            assert_eq!(location.location_type, Some(kind));
            assert_eq!(
                (location.area, location.building, location.level),
                (area, building, level),
            );
        }
        assert!(location_query("1-2-3").is_err());
    }

    #[test]
    fn encryption_id_header_formats_scalars_and_rejects_invalid_values() {
        let string_id = Encryption {
            id: json!("JebRGi53Oy4ZwgO4Q-YqH"),
            public_key: String::new(),
        };
        assert_eq!(string_id.id_header().unwrap(), "JebRGi53Oy4ZwgO4Q-YqH");

        let numeric_id = Encryption {
            id: json!(42),
            public_key: String::new(),
        };
        assert_eq!(numeric_id.id_header().unwrap(), "42");

        for invalid in [json!(""), json!(null), json!(true), json!({})] {
            let encryption = Encryption {
                id: invalid,
                public_key: String::new(),
            };
            assert!(encryption.id_header().unwrap_err().is_decode());
        }
    }

    #[test]
    fn balance_response_decrypts_sm4_payload_and_parses_decimal() {
        let key = crypto::random_sm4_key();
        let payload = json!({
            "success": true,
            "message": "成功",
            "resultData": {
                "balanceLessRecharge": false,
                "user": {
                    "balance": "10.46",
                    "mainblance": 1046,
                    "subblance": 0,
                    "accTypes": [{"typeCode": "0", "typeName": "主账户", "balance": "10.46"}],
                },
                "merchant": null,
                "merchantAdmin": null,
            },
            "code": "",
        });
        let encrypted = user_info_response(encrypt_payload(&payload, &key));
        assert_eq!(
            parse_balance_response(encrypted, &key).unwrap().to_string(),
            "10.46"
        );

        let plaintext = user_info_response(json!({
            "success": true,
            "resultData": {"user": {"balance": "0.00"}},
        }));
        assert_eq!(
            parse_balance_response(plaintext, &key).unwrap().to_string(),
            "0.00"
        );
    }

    #[test]
    fn balance_response_surfaces_remote_crypto_and_decode_errors_with_context() {
        let key = "AbCdEf012345_+-=";
        let other_key = "rx4EG_5ziW3YMjZt";

        let remote_failure = user_info_response(encrypt_payload(
            &json!({"success": false, "message": "会话已过期", "code": "401"}),
            key,
        ));
        let error = parse_balance_response(remote_failure, key).unwrap_err();
        assert!(error.is_remote());
        assert_eq!(error.message(), "会话已过期");
        assert_eq!(error.request().unwrap().error_code.as_deref(), Some("401"));

        let wrong_key_payload = user_info_response(encrypt_payload(
            &json!({"success": true, "resultData": {"user": {"balance": "10.46"}}}),
            key,
        ));
        let error = parse_balance_response(wrong_key_payload, other_key).unwrap_err();
        assert!(error.is_crypto());
        assert!(error.request().is_some());

        let missing_user = user_info_response(encrypt_payload(
            &json!({"success": true, "resultData": {"user": null}}),
            key,
        ));
        let error = parse_balance_response(missing_user, key).unwrap_err();
        assert!(error.is_decode());
        assert!(error.request().is_some());

        let invalid_balance = user_info_response(encrypt_payload(
            &json!({"success": true, "resultData": {"user": {"balance": "NaN"}}}),
            key,
        ));
        let error = parse_balance_response(invalid_balance, key).unwrap_err();
        assert!(error.is_decode());
        assert!(error.request().is_some());
    }

    #[test]
    fn session_and_transaction_parsers_attach_request_context_on_malformed_payloads() {
        use reqwest::header::{HeaderMap, HeaderValue, LOCATION};

        let mut headers = HeaderMap::new();
        headers.insert(
            LOCATION,
            HeaderValue::from_static("https://ecard.v.zzu.edu.cn/#/home"),
        );
        let redirect = http::Response {
            headers,
            url: format!("{ECARD_URL}/auth/host/open").parse().unwrap(),
            context: Arc::new(RequestContext {
                method: "GET".into(),
                url: format!("{ECARD_URL}/auth/host/open"),
                status_code: Some(302),
                error_code: None,
                response_body: Some(String::new()),
            }),
        };
        let err = parse_session_redirect(&redirect).unwrap_err();
        assert!(err.is_decode());
        assert_eq!(err.request().unwrap().status_code, Some(302));

        let empty_tokens = http::JsonResponse::new(
            json!({"success": true, "resultData": {"accessToken": "", "refreshToken": "r"}}),
            Arc::new(RequestContext {
                method: "POST".into(),
                url: format!("{ECARD_URL}/auth/getToken"),
                status_code: Some(200),
                error_code: None,
                response_body: None,
            }),
        );
        let err = parse_session_tokens(empty_tokens, "cas".into())
            .err()
            .unwrap();
        assert!(err.is_decode());
        assert!(err.request().is_some());

        let empty_tid = http::JsonResponse::new(
            json!({"success": true, "resultData": ""}),
            Arc::new(RequestContext {
                method: "POST".into(),
                url: format!("{ECARD_URL}/consume/transaction"),
                status_code: Some(200),
                error_code: None,
                response_body: None,
            }),
        );
        let err = parse_transaction_id_response(empty_tid).unwrap_err();
        assert!(err.is_decode());
        assert!(err.request().is_some());
    }
}
