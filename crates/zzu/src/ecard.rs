//! 校园卡会话认证、余额查询、宿舍层级检索以及电费查询与充值。

use std::{
    collections::BTreeMap,
    fmt,
    future::Future,
    str::FromStr,
    sync::{Mutex, MutexGuard},
};

use jiff::{Timestamp, tz::TimeZone};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;

use crate::{Error, Result, auth::CasClient, crypto, http};

mod recharge;

pub use recharge::{
    PaymentAction, PaymentChannel, PaymentContext, PaymentDevice, RechargeConfig, RechargeOrder,
};

const ECARD_URL: &str = "https://ecard.v.zzu.edu.cn/server";
const SESSION_MAX_AGE_SECONDS: i64 = 45 * 60;

/// 经过格式校验的宿舍房间标识符（格式为 `area-building--level-room`，四段均非空且不含 `-`）。
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
#[serde(try_from = "String", into = "String")]
pub struct RoomId(String);

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
    /// 构造电费账户查询与扣款接口所需的房间位置参数（`building` 与 `level` 须保留上级前缀，如 `"1-2"`、`"1-2--3"`）。
    fn location(&self) -> LocationRequest<'_> {
        let (building, _) = self.0.split_once("--").expect("validated room");
        let (area, _) = building.split_once('-').expect("validated building");
        let (level, _) = self.0.rsplit_once('-').expect("validated floor");
        LocationRequest {
            area,
            building,
            level,
            room: &self.0,
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
    default_room: Option<RoomId>,
}

/// 校园卡与宿舍电控服务客户端。
///
/// 共享 [`CasClient`] 的统一身份认证会话。内部仅在读写会话与默认房间缓存时短暂持锁，发起网络请求前即释放状态锁，
/// 因此查询与收银台预支付方法均以 `&self` 并发执行；会话变更（[`Self::login`]、[`Self::logout`]）与扣款、下单
/// （[`Self::recharge_energy`]、[`Self::create_recharge_order`]）仍要求 `&mut self`。
///
/// 客户端不启动后台刷新任务：每次请求前按需检查校园卡 `access_token`，满 45 分钟或底层 CAS 令牌变更时自动换取新令牌；
/// 令牌换新单独互斥，多个并发请求等待并复用同一次换新结果。
pub struct EcardClient {
    http: Client,
    cas: CasClient,
    state: Mutex<EcardState>,
    refresh_lock: AsyncMutex<()>,
}

impl EcardClient {
    /// 基于共享的 [`CasClient`] 与默认 HTTP 配置创建校园卡客户端。
    ///
    /// # Errors
    ///
    /// 当默认 HTTP 客户端初始化失败时返回错误。
    pub fn new(cas: CasClient) -> Result<Self> {
        Ok(Self::with_http_client(cas, http::client()?))
    }

    /// 基于共享的 [`CasClient`] 与自定义 [`reqwest::Client`] 创建校园卡客户端。
    ///
    /// 传入的 `client` 必须禁用自动重定向（例如由 [`crate::http_client_builder`] 构建），以便捕获单点登录跳转中的 `tid` 参数。
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
    /// - 当底层 [`CasClient`] 尚未登录或令牌不可用时返回 [`Error::Authentication`]。
    /// - 当单点登录跳转响应缺少 `Location` 或 `tid`、令牌交换失败或网络异常时返回相应错误。
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
        self.access_token_with(|token| fetch_session(&self.http, token))
            .await
    }

    async fn access_token_with<F>(&self, fetch: impl FnOnce(String) -> F) -> Result<String>
    where
        F: Future<Output = Result<Session>>,
    {
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
        let session = fetch(cas_token).await?;
        let token = session.access_token.clone();
        self.lock_state().session = Some(session);
        Ok(token)
    }

    /// 查询并缓存当前账号在电控系统中绑定的默认宿舍房间 ID。
    ///
    /// # Errors
    ///
    /// - 当客户端尚未调用 [`Self::login`] 时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    /// - 当会话刷新、网络请求、远端业务校验或房间号解析失败时返回相应错误。
    pub async fn default_room(&self) -> Result<RoomId> {
        #[derive(Deserialize)]
        struct Config {
            location: Location,
        }
        #[derive(Deserialize)]
        struct Location {
            room: RoomId,
        }
        let token = self.access_token().await?;
        let room = self.lock_state().default_room.clone();
        if let Some(room) = room {
            return Ok(room);
        }
        let response = http::json(
            self.http
                .post(format!("{ECARD_URL}/utilities/config"))
                .header("Authorization", token)
                .json(&json!({"utilityType":"electric"})),
        )
        .await?;
        let config: Config = http::ecard_data(response)?;
        self.lock_state().default_room = Some(config.location.room.clone());
        Ok(config.location.room)
    }

    /// 查询当前账号的校园卡余额（单位：元）。
    ///
    /// # Errors
    ///
    /// - 当客户端尚未登录或底层 CAS 会话未认证时返回 [`Error::Authentication`]。
    /// - 当门户信息卡片查询失败或余额字段不是合法有限数值时返回相应错误。
    pub async fn balance(&self) -> Result<crate::Decimal> {
        self.access_token().await?;
        let cards = self.cas.info_cards().await?;
        let card = cards
            .get(1)
            .ok_or_else(|| Error::decode_message("个人信息缺少余额卡片".into()))?;
        http::parse_money(&card.amount, "balance")
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
    /// - 当 `parent` 不符合上述四种格式（例如包含空段、多余连字符或传入完整四段房间号）时返回 [`Error::InvalidInput`]。
    /// - 当客户端尚未登录、网络请求失败或远端返回错误时返回相应错误。
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

    /// 查询指定宿舍房间（传入 `None` 时使用 [`Self::default_room`]）的剩余电量（单位：度）。
    ///
    /// # Errors
    ///
    /// - 当客户端尚未登录时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    /// - 当获取默认房间、请求电费账户或解析 `quantity` 电量字段失败时返回相应错误。
    pub async fn remaining_energy(&self, room: Option<&RoomId>) -> Result<f64> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Account {
            template_list: Vec<Template>,
        }
        #[derive(Deserialize)]
        struct Template {
            code: String,
            value: Value,
        }
        let room = match room {
            Some(room) => room.clone(),
            None => self.default_room().await?,
        };
        let token = self.access_token().await?;
        let response = http::json(
            self.http
                .post(format!("{ECARD_URL}/utilities/account"))
                .header("Authorization", token)
                .json(&room.location()),
        )
        .await?;
        let account: Account = http::ecard_data(response)?;
        let quantity = account
            .template_list
            .iter()
            .find(|template| template.code == "quantity")
            .ok_or_else(|| Error::decode_message("电费账户缺少 quantity".into()))?;
        http::parse_number(&quantity.value, "quantity")
    }

    /// 使用校园卡支付密码从卡余额扣款，为指定宿舍房间充值电费。
    ///
    /// `amount` 为正整数充值金额（单位：元）。提交前会申请一次性交易号与服务端临时 SM2 公钥，
    /// 用公钥加密支付参数与本地随机生成的 SM4 密钥；服务端返回加密响应时再用该 SM4 密钥解密。
    ///
    /// # Errors
    ///
    /// - 当 `amount == 0` 或 `payment_password` 为空字符串时返回 [`Error::InvalidInput`]。
    /// - 当客户端尚未登录、公钥解析或加解密失败、网络异常或远端拒绝扣款时返回相应错误。
    /// - 若支付响应缺失 `success` 确认字段导致扣款结果未知，返回 [`Error::Decode`]；加密响应解密失败时返回的
    ///   [`Error::Crypto`] 同样意味着结果未知。两种情况下调用方都应先核对余额再决定是否重试。
    pub async fn recharge_energy(
        &mut self,
        payment_password: &str,
        amount: u32,
        room: &RoomId,
    ) -> Result<()> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Encryption {
            id: Value,
            public_key: String,
        }
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
            app_id: &'a str,
        }
        if amount == 0 || payment_password.is_empty() {
            return Err(Error::invalid_input(
                "充值金额必须大于 0，支付密码不能为空".into(),
            ));
        }
        let token = self.access_token().await?;
        let request = self
            .http
            .post(format!("{ECARD_URL}/auth/getEncrypt"))
            .header("Authorization", &token)
            .json(&json!({"protocolVersion":2}));
        let response = http::json(request).await?;
        let encryption: Encryption = http::ecard_data(response)?;
        let payment = Payment {
            location: room.location(),
            pay_code: "06",
            password: payment_password,
            amt: amount.to_string(),
            timestamp: Timestamp::now().as_millisecond(),
            customfield: BTreeMap::new(),
            app_id: "electric",
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
        let context = response.context.clone();
        let result = decrypt_response(&response, &sm4_key)
            .map_err(|error| error.with_request(context.clone()))?;
        match result.get("success").and_then(Value::as_bool) {
            Some(true) => Ok(()),
            Some(false) => Err(Error::remote(http::message(&result)).with_request(context)),
            None => Err(Error::decode_message(
                "充值响应缺少 success，支付结果未知，请查询余额后再决定是否重试".into(),
            )
            .with_request(context)),
        }
    }

    /// 申请 `/utilities/pay`、`/recharge/pay` 等扣款接口请求头 `transactionId` 所需的一次性交易号。
    async fn transaction_id(&self, token: &str) -> Result<String> {
        let datetime = Timestamp::now()
            .to_zoned(TimeZone::get("Asia/Shanghai").map_err(Error::decode)?)
            .strftime("%F %T")
            .to_string();
        let response = http::json(
            self.http
                .post(format!("{ECARD_URL}/consume/transaction"))
                .header("Authorization", token)
                .json(&json!({"datetime":datetime})),
        )
        .await?;
        let transaction_id: String = http::ecard_data(response)?;
        if transaction_id.is_empty() {
            return Err(Error::decode_message("扣款交易号为空".into()));
        }
        Ok(transaction_id)
    }

    /// 清除本地缓存的校园卡会话与默认房间信息（不影响底层共享的 [`CasClient`]）。
    pub fn logout(&mut self) {
        *self.lock_state() = EcardState::default();
    }
}

/// 还原校园卡加密响应：与前端一致，`success` 不为真且带非空 `data` 时，用请求时生成的 SM4 密钥解密 `data` 得到实际响应。
fn decrypt_response(response: &Value, sm4_key: &str) -> Result<Value> {
    let is_success = response.get("success").and_then(Value::as_bool) == Some(true);
    match response.get("data").and_then(Value::as_str) {
        Some(data) if !is_success && !data.is_empty() => Ok(serde_json::from_slice(
            &crypto::sm4_decrypt(data, sm4_key)?,
        )?),
        _ => Ok(response.clone()),
    }
}

async fn fetch_session(http: &Client, cas_token: String) -> Result<Session> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TokenData {
        access_token: String,
        refresh_token: String,
    }
    let request = http.get(format!("{ECARD_URL}/auth/host/open")).query(&[
        ("host", "11"),
        ("org", "2"),
        ("token", cas_token.as_str()),
    ]);
    let response = http::send(request).await?.allow_redirect()?;
    let location = response
        .headers
        .get("location")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            Error::decode_message("校园卡响应缺少 Location".into())
                .with_request(response.context.clone())
        })?;
    let redirect = response.url.join(location).map_err(Error::decode)?;
    let tid = redirect
        .query_pairs()
        .find(|(key, _)| key == "tid")
        .map(|(_, value)| value.into_owned())
        .filter(|tid| !tid.is_empty())
        .ok_or_else(|| Error::decode_message("校园卡重定向缺少 tid".into()))?;
    let response = http::json(
        http.post(format!("{ECARD_URL}/auth/getToken"))
            .json(&json!({"tid":tid})),
    )
    .await?;
    let tokens: TokenData = http::ecard_data(response)?;
    if tokens.access_token.is_empty() || tokens.refresh_token.is_empty() {
        return Err(Error::decode_message("校园卡 Token 为空".into()));
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
        }
    }
}

/// 构造 `/utilities/location` 目录检索请求：与 [`RoomId::location`] 不同，该接口要求 `area`、`building`、`level` 传入拆分后的单段编码。
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    use super::*;
    use crate::auth::Tokens;

    async fn client_with_expired_session() -> EcardClient {
        let cas = CasClient::new("", "").unwrap();
        let token = format!(
            "e30.{}.c2ln",
            URL_SAFE_NO_PAD.encode(json!({"exp": Timestamp::now().as_second() + 3600}).to_string())
        );
        cas.set_tokens(Tokens {
            user_token: token.clone(),
            refresh_token: token.clone(),
        })
        .await
        .unwrap();
        cas.login().await.unwrap();
        let mut client = EcardClient::new(cas).unwrap();
        *client.state.get_mut().unwrap() = EcardState {
            session: Some(Session {
                access_token: "expired".into(),
                cas_token: token,
                obtained_at: Timestamp::now().as_second() - SESSION_MAX_AGE_SECONDS,
            }),
            default_room: Some("1-2--3-301".parse().unwrap()),
        };
        client
    }

    #[tokio::test]
    async fn concurrent_requests_share_one_refresh_and_logout_clears_state() {
        let mut client = client_with_expired_session().await;
        let refreshes = AtomicUsize::new(0);
        let refresh_count = &refreshes;
        let fetch = move |cas_token| async move {
            refresh_count.fetch_add(1, Ordering::Relaxed);
            tokio::task::yield_now().await;
            Ok(Session {
                access_token: "fresh".into(),
                cas_token,
                obtained_at: Timestamp::now().as_second(),
            })
        };

        let (first, second) = tokio::join!(
            client.access_token_with(fetch),
            client.access_token_with(fetch)
        );

        assert_eq!(first.unwrap(), "fresh");
        assert_eq!(second.unwrap(), "fresh");
        assert_eq!(refreshes.load(Ordering::Relaxed), 1);
        assert!(client.is_logged_in());
        client.logout();
        assert!(!client.is_logged_in());
        assert!(client.state.get_mut().unwrap().default_room.is_none());
        assert!(
            client
                .default_room()
                .await
                .unwrap_err()
                .is_not_authenticated()
        );
    }

    #[tokio::test]
    async fn cancelled_refresh_leaves_the_next_request_able_to_refresh() {
        let client = client_with_expired_session().await;
        {
            let request = client.access_token_with(|_| std::future::pending::<Result<Session>>());
            tokio::pin!(request);
            tokio::select! {
                biased;
                result = &mut request => panic!("refresh unexpectedly finished: {result:?}"),
                () = tokio::task::yield_now() => {}
            }
        }
        let token = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.access_token_with(|cas_token| async move {
                Ok(Session {
                    access_token: "fresh".into(),
                    cas_token,
                    obtained_at: Timestamp::now().as_second(),
                })
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(token, "fresh");
    }

    #[test]
    fn room_and_location_ids_are_validated_before_requests() {
        let room: RoomId = "1-2--3-301".parse().unwrap();
        assert_eq!(room.location().level, "1-2--3");
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
}
