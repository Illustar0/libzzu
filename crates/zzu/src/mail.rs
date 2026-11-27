//! 校园邮箱（Coremail）客户端，提供网页版入口地址、收件箱未读邮件数与收件箱邮件摘要查询。

use std::sync::{Arc, Mutex, MutexGuard};

use jiff::{Timestamp, ZonedDateTime, civil::DateTime};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;

use crate::{
    Error, Result,
    auth::{CasClient, fetch_email_login_redirect},
    http,
};

const MAIL_API_URL: &str = "https://mail.v.zzu.edu.cn/coremail/XT/jsp/mail.jsp";
const MAIL_JSON_API_URL: &str = "https://mail.v.zzu.edu.cn/coremail/s/json";
const SESSION_MAX_AGE_SECONDS: i64 = 20 * 60;
const INBOX_FOLDER_ID: i64 = 1;

/// 收件箱邮件摘要分页查询的默认每页条数。
pub const DEFAULT_MAIL_PAGE_SIZE: u32 = 20;

/// 收件箱邮件摘要的单页查询结果。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MailMessagePage {
    /// 当前页的邮件摘要列表（按接收时间倒序排列）。
    pub items: Vec<MailMessage>,
    /// 当前页码（从 `1` 开始）。
    pub page: u32,
    /// 本次查询实际使用的每页条数。
    pub page_size: u32,
    /// 收件箱中的邮件总封数。
    pub total_items: u32,
    /// 按 `page_size` 计算的总页数。
    pub total_pages: u32,
}

/// 收件箱单封邮件摘要元数据。
///
/// 仅承载列表预览所需的发件人、主题、摘要片段、时间与状态标记，不包含富文本正文与附件内容；
/// 阅读完整邮件或回复请通过 [`MailClient::webmail_url`] 跳转网页版完成。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MailMessage {
    /// Coremail 邮件唯一标识。
    pub id: String,
    /// 发件人显示文本（如 `"\"通知中心\" <notice@example.com>"`）。
    pub from: String,
    /// 收件人显示文本。
    pub to: String,
    /// 邮件主题（无主题邮件为空字符串）。
    pub subject: String,
    /// 纯文本正文摘要预览（无正文摘要时为 `None`）。
    #[serde(default)]
    pub summary: Option<String>,
    /// 发信头声明的发送时间（`Asia/Shanghai` 时区；缺失或格式无法识别时为 `None`）。
    #[serde(default)]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<crate::schema::ZonedDateTime>")
    )]
    pub sent_time: Option<ZonedDateTime>,
    /// 邮件到达服务器的时间（`Asia/Shanghai` 时区）。
    #[cfg_attr(feature = "schemars", schemars(with = "crate::schema::ZonedDateTime"))]
    pub received_time: ZonedDateTime,
    /// 邮件原始大小（单位：字节）。
    pub size: u64,
    /// 是否已读。
    pub is_read: bool,
    /// 是否带有附件。
    pub has_attachment: bool,
    /// 是否已加星标（红旗标记）。
    pub is_flagged: bool,
}

#[derive(Clone)]
struct Session {
    webmail_url: String,
    sid: String,
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

/// 校园邮箱客户端。
///
/// 共享 [`CasClient`] 的统一身份认证会话；调用 [`Self::login`] 换取邮箱会话后，
/// 即可获取网页版邮箱入口地址，并查询收件箱未读邮件数与邮件摘要列表。
pub struct MailClient {
    http: Client,
    cas: CasClient,
    session: Mutex<Option<Session>>,
    refresh_lock: AsyncMutex<()>,
}

impl MailClient {
    /// 使用共享的 [`CasClient`] 与默认 HTTP 配置创建校园邮箱客户端。
    ///
    /// # Errors
    ///
    /// 默认 HTTP 客户端初始化失败时返回错误。
    pub fn new(cas: CasClient) -> Result<Self> {
        Ok(Self::with_http_client(cas, http::client()?))
    }

    /// 使用共享的 [`CasClient`] 与自定义 [`reqwest::Client`] 创建校园邮箱客户端。
    ///
    /// `client` 必须禁用自动重定向（如通过 [`crate::http_client_builder`] 构建），
    /// 以便捕获单点登录跳转中的 `Location` 头与 `sid` 参数。
    #[must_use]
    pub fn with_http_client(cas: CasClient, client: Client) -> Self {
        Self {
            http: client,
            cas,
            session: Mutex::new(None),
            refresh_lock: AsyncMutex::new(()),
        }
    }

    fn lock_session(&self) -> MutexGuard<'_, Option<Session>> {
        self.session.lock().expect("mail session poisoned")
    }

    /// 判断当前校园邮箱客户端是否已建立本地会话。
    #[must_use]
    pub fn is_logged_in(&self) -> bool {
        self.lock_session().is_some()
    }

    /// 使用共享的 [`CasClient`] 令牌换取邮箱会话（`sid`）与网页版邮箱入口地址。
    ///
    /// # Errors
    ///
    /// - 底层 [`CasClient`] 尚未登录或令牌不可用时返回 [`Error::Authentication`]。
    /// - 单点登录跳转响应缺少 `Location` 或 `sid`，或网络传输失败时返回错误。
    pub async fn login(&mut self) -> Result<()> {
        self.logout();
        let cas_token = self.cas.user_token().await?;
        let session = fetch_session(&self.http, cas_token).await?;
        *self.lock_session() = Some(session);
        Ok(())
    }

    /// 清除本地缓存的邮箱会话状态，不影响底层 [`CasClient`]。
    pub fn logout(&mut self) {
        *self.lock_session() = None;
    }

    fn cached_session(&self, cas_token: &str) -> Result<Option<Session>> {
        let state = self.lock_session();
        let session = state.as_ref().ok_or(Error::not_authenticated())?;
        Ok(session.is_current(cas_token).then(|| session.clone()))
    }

    async fn session(&self) -> Result<Session> {
        if !self.is_logged_in() {
            return Err(Error::not_authenticated());
        }
        let cas_token = self.cas.user_token().await?;
        if let Some(session) = self.cached_session(&cas_token)? {
            return Ok(session);
        }
        let _refresh = self.refresh_lock.lock().await;
        let cas_token = self.cas.user_token().await?;
        if let Some(session) = self.cached_session(&cas_token)? {
            return Ok(session);
        }
        let session = fetch_session(&self.http, cas_token).await?;
        *self.lock_session() = Some(session.clone());
        Ok(session)
    }

    async fn refresh_session_if_unchanged(&self, stale_sid: &str) -> Result<Session> {
        let _refresh = self.refresh_lock.lock().await;
        let cas_token = self.cas.user_token().await?;
        if let Some(session) = self.cached_session(&cas_token)?
            && session.sid != stale_sid
        {
            return Ok(session);
        }
        let session = fetch_session(&self.http, cas_token).await?;
        *self.lock_session() = Some(session.clone());
        Ok(session)
    }

    /// 获取当前邮箱会话对应的网页版入口 URL（含 `sid` 查询参数）。
    ///
    /// 返回的 URL 携带邮箱会话凭据，切勿写入日志。
    /// 若缓存会话已超过 20 分钟经验有效期或底层 CAS 令牌已更新，会自动按需换取新会话。
    ///
    /// # Errors
    ///
    /// - 客户端尚未登录或底层 CAS 令牌不可用时返回 [`Error::Authentication`]。
    /// - 换取邮箱会话失败时返回错误。
    pub async fn webmail_url(&self) -> Result<String> {
        Ok(self.session().await?.webmail_url)
    }

    /// 查询收件箱（文件夹 ID 为 `1`）中的未读邮件数。
    ///
    /// 统计口径与 Coremail 导航栏未读提示一致，仅包含收件箱，不计入虚拟视图、草稿箱、
    /// 已发送、已删除、垃圾邮件与病毒文件夹。
    /// 若服务端报告会话失效（`FA_INVALID_SESSION`、`FA_INVALID_TEMP_SESSION` 或 `FA_SECURITY`），
    /// 会自动用现有 CAS 令牌换新一次会话并重试。
    ///
    /// # Errors
    ///
    /// - 客户端尚未登录或底层 CAS 令牌不可用时返回 [`Error::Authentication`]。
    /// - 网络传输失败、服务端返回业务错误或响应缺少收件箱统计字段时返回错误。
    pub async fn unread_count(&self) -> Result<u32> {
        let session = self.session().await?;
        match self.fetch_unread_count(&session.sid).await? {
            CoremailOutcome::Ok(count) => Ok(count),
            CoremailOutcome::SessionExpired(_) => {
                let refreshed = self.refresh_session_if_unchanged(&session.sid).await?;
                match self.fetch_unread_count(&refreshed.sid).await? {
                    CoremailOutcome::Ok(count) => Ok(count),
                    CoremailOutcome::SessionExpired(error) => Err(error),
                }
            }
        }
    }

    /// 分页查询收件箱（文件夹 ID 为 `1`）中的邮件摘要列表，按接收时间倒序排列。
    ///
    /// `page` 从 `1` 开始；`page_size` 为 `None` 时默认使用 [`DEFAULT_MAIL_PAGE_SIZE`]（`20`）。
    /// 若服务端报告邮箱会话失效，会自动用现有 CAS 令牌换新一次会话并重试。
    ///
    /// # Errors
    ///
    /// - `page == 0` 或 `page_size == Some(0)` 时返回 [`Error::InvalidInput`]。
    /// - 客户端尚未登录或底层 CAS 令牌不可用时返回 [`Error::Authentication`]。
    /// - 网络传输失败、服务端返回业务错误或邮件列表字段解析失败时返回错误。
    pub async fn messages(&self, page: u32, page_size: Option<u32>) -> Result<MailMessagePage> {
        let (start, resolved_page_size) = validate_mail_pagination(page, page_size)?;
        let session = self.session().await?;
        match self
            .fetch_messages(&session.sid, page, resolved_page_size, start)
            .await?
        {
            CoremailOutcome::Ok(result) => Ok(result),
            CoremailOutcome::SessionExpired(_) => {
                let refreshed = self.refresh_session_if_unchanged(&session.sid).await?;
                match self
                    .fetch_messages(&refreshed.sid, page, resolved_page_size, start)
                    .await?
                {
                    CoremailOutcome::Ok(result) => Ok(result),
                    CoremailOutcome::SessionExpired(error) => Err(error),
                }
            }
        }
    }

    async fn fetch_unread_count(&self, sid: &str) -> Result<CoremailOutcome<u32>> {
        let request = self
            .http
            .post(MAIL_API_URL)
            .query(&[("func", "getAllFolders"), ("sid", sid)])
            .header("Accept", "text/x-json")
            .form(&[("stats", "true")]);
        // Coremail 在会话失效等场景下会以 HTTP 500 返回 JSON 错误体，因此先解析报文再判定状态码。
        let response = http::send(request).await?;
        parse_unread_count_response(response)
    }

    async fn fetch_messages(
        &self,
        sid: &str,
        page: u32,
        page_size: u32,
        start: u64,
    ) -> Result<CoremailOutcome<MailMessagePage>> {
        let body = build_list_messages_body(start, page_size);
        let request = self
            .http
            .post(MAIL_JSON_API_URL)
            .query(&[("func", "mbox:listMessages"), ("sid", sid)])
            .header("Accept", "text/x-json")
            .json(&body);
        let response = http::send(request).await?;
        parse_messages_response(response, page, page_size)
    }
}

enum CoremailOutcome<T> {
    Ok(T),
    SessionExpired(Error),
}

fn validate_mail_pagination(page: u32, page_size: Option<u32>) -> Result<(u64, u32)> {
    if page == 0 {
        return Err(Error::invalid_input("页码 page 必须从 1 开始".into()));
    }
    let page_size = page_size.unwrap_or(DEFAULT_MAIL_PAGE_SIZE);
    if page_size == 0 {
        return Err(Error::invalid_input("每页条数 page_size 必须大于 0".into()));
    }
    let start = u64::from(page - 1) * u64::from(page_size);
    Ok((start, page_size))
}

fn build_list_messages_body(start: u64, page_size: u32) -> Value {
    json!({
        "fid": INBOX_FOLDER_ID,
        "start": start,
        "limit": page_size,
        "order": "receivedDate",
        "desc": true,
        "returnTotal": true,
        "summaryWindowSize": page_size,
    })
}

async fn fetch_session(http: &Client, cas_token: String) -> Result<Session> {
    let response = fetch_email_login_redirect(http, &cas_token).await?;
    parse_session_response(&response, cas_token, Timestamp::now().as_second())
}

fn parse_session_response(
    response: &http::Response,
    cas_token: String,
    obtained_at: i64,
) -> Result<Session> {
    let location = response
        .headers
        .get("location")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            Error::decode_message("邮箱单点登录响应缺少 Location".into())
                .with_request(response.context.clone())
        })?;
    let redirect = response
        .url
        .join(location)
        .map_err(|error| Error::decode(error).with_request(response.context.clone()))?;
    let sid = redirect
        .query_pairs()
        .find(|(key, _)| key == "sid")
        .map(|(_, value)| value.into_owned())
        .filter(|sid| !sid.is_empty())
        .ok_or_else(|| {
            Error::decode_message("邮箱重定向地址缺少 sid".into())
                .with_request(response.context.clone())
        })?;
    Ok(Session {
        webmail_url: redirect.into(),
        sid,
        cas_token,
        obtained_at,
    })
}

fn parse_coremail_envelope<T>(
    response: http::Response,
    extract: impl FnOnce(&Value, &Arc<crate::RequestContext>) -> Result<T>,
) -> Result<CoremailOutcome<T>> {
    let value: Value = match serde_json::from_str(response.body()) {
        Ok(value) => value,
        Err(decode_error) => {
            let response = response.require_success()?;
            return Err(Error::from(decode_error).with_request(response.context));
        }
    };

    if let Some(code) = non_empty_str(&value, "code") {
        if code == "S_OK" {
            return extract(&value, &response.context).map(CoremailOutcome::Ok);
        }
        return classify_coremail_error(&value, Some(code), &response.context);
    }

    if value.get("result").and_then(Value::as_str) == Some("error") {
        let error_code = non_empty_str(&value, "errorCode");
        return classify_coremail_error(&value, error_code, &response.context);
    }

    let response = response.require_success()?;
    Err(Error::decode_message("邮箱响应缺少有效的 code 字段".into()).with_request(response.context))
}

fn parse_unread_count_response(response: http::Response) -> Result<CoremailOutcome<u32>> {
    #[derive(Deserialize)]
    struct Folder {
        id: i64,
        stats: Option<FolderStats>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct FolderStats {
        unread_message_count: Option<u32>,
    }

    parse_coremail_envelope(response, |value, context| {
        let folders_value = value.get("var").ok_or_else(|| {
            Error::decode_message("邮箱文件夹响应缺少 var 字段".into())
                .with_request(context.clone())
        })?;
        let folders: Vec<Folder> = http::deserialize_value_ref(folders_value, Some("var"))
            .map_err(|error| error.with_request(context.clone()))?;
        let inbox = folders
            .into_iter()
            .find(|folder| folder.id == INBOX_FOLDER_ID)
            .ok_or_else(|| {
                Error::decode_message("邮箱文件夹列表缺少收件箱".into())
                    .with_request(context.clone())
            })?;
        inbox
            .stats
            .and_then(|stats| stats.unread_message_count)
            .ok_or_else(|| {
                Error::decode_message("收件箱缺少 unreadMessageCount 字段".into())
                    .with_request(context.clone())
            })
    })
}

fn parse_messages_response(
    response: http::Response,
    page: u32,
    page_size: u32,
) -> Result<CoremailOutcome<MailMessagePage>> {
    #[derive(Default, Deserialize)]
    struct RawFlags {
        #[serde(default)]
        read: bool,
        #[serde(default)]
        attached: bool,
        #[serde(default)]
        flagged: bool,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawMailMessage {
        id: String,
        #[serde(default)]
        from: Option<String>,
        #[serde(default)]
        to: Option<String>,
        #[serde(default)]
        subject: Option<String>,
        #[serde(default)]
        summary: Option<String>,
        #[serde(default)]
        sent_date: Option<String>,
        received_date: String,
        #[serde(default)]
        size: u64,
        #[serde(default)]
        flags: Option<RawFlags>,
    }

    parse_coremail_envelope(response, |value, context| {
        let total_u64 = value.get("total").and_then(Value::as_u64).ok_or_else(|| {
            Error::decode_message("邮件列表响应缺少有效的 total 字段".into())
                .with_request(context.clone())
        })?;
        let total_items = u32::try_from(total_u64).map_err(|_| {
            Error::decode_message("邮件总数超出 u32 范围".into()).with_request(context.clone())
        })?;
        let var_value = value.get("var").ok_or_else(|| {
            Error::decode_message("邮件列表响应缺少 var 字段".into()).with_request(context.clone())
        })?;
        let raw_items: Vec<RawMailMessage> = http::deserialize_value_ref(var_value, Some("var"))
            .map_err(|error| error.with_request(context.clone()))?;

        let mut items = Vec::with_capacity(raw_items.len());
        for raw in raw_items {
            let id = raw.id.trim().to_owned();
            if id.is_empty() {
                return Err(Error::decode_message("邮件记录缺少有效的 id".into())
                    .with_request(context.clone()));
            }
            let received_time = parse_coremail_time(&raw.received_date)
                .map_err(|error| error.with_request(context.clone()))?;
            let sent_time = raw
                .sent_date
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .and_then(|text| parse_coremail_time(text).ok());
            let flags = raw.flags.unwrap_or_default();
            items.push(MailMessage {
                id,
                from: raw.from.unwrap_or_default().trim().to_owned(),
                to: raw.to.unwrap_or_default().trim().to_owned(),
                subject: raw.subject.unwrap_or_default(),
                summary: clean_summary(raw.summary),
                sent_time,
                received_time,
                size: raw.size,
                is_read: flags.read,
                has_attachment: flags.attached,
                is_flagged: flags.flagged,
            });
        }

        Ok(MailMessagePage {
            items,
            page,
            page_size,
            total_items,
            total_pages: total_items.div_ceil(page_size),
        })
    })
}

fn parse_coremail_time(text: &str) -> Result<ZonedDateTime> {
    let trimmed = text.trim();
    if let Ok(zoned) = trimmed.parse::<ZonedDateTime>() {
        return Ok(zoned);
    }
    DateTime::strptime("%Y-%m-%d %H:%M:%S", trimmed)
        .and_then(|datetime| datetime.in_tz("Asia/Shanghai"))
        .map_err(|error| Error::decode_with_source(format!("无法解析邮件时间：{trimmed}"), error))
}

fn clean_summary(summary: Option<String>) -> Option<String> {
    summary.and_then(|text| {
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

fn classify_coremail_error<T>(
    value: &Value,
    code: Option<&str>,
    context: &Arc<crate::RequestContext>,
) -> Result<CoremailOutcome<T>> {
    let mut updated = (**context).clone();
    if let Some(code) = code {
        updated.error_code = Some(code.to_owned());
    }
    let error = Error::remote(coremail_error_message(value, code)).with_request(Arc::new(updated));
    if code.is_some_and(is_session_expired_code) {
        return Ok(CoremailOutcome::SessionExpired(error));
    }
    Err(error)
}

fn non_empty_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

fn is_session_expired_code(code: &str) -> bool {
    matches!(
        code,
        "FA_INVALID_SESSION" | "FA_INVALID_TEMP_SESSION" | "FA_SECURITY"
    )
}

fn coremail_error_message(value: &Value, code: Option<&str>) -> String {
    ["errorMsg", "summary", "message", "msg", "error"]
        .into_iter()
        .find_map(|key| non_empty_str(value, key).map(str::to_owned))
        .unwrap_or_else(|| match code {
            Some(code) => format!("邮箱服务返回错误：{code}"),
            None => "邮箱服务未提供错误消息".into(),
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use reqwest::header::{HeaderMap, HeaderValue, LOCATION};
    use serde_json::json;

    use super::*;
    use crate::{RequestContext, auth::EMAIL_URL};

    fn redirect_response(location: Option<&str>) -> http::Response {
        let mut headers = HeaderMap::new();
        if let Some(location) = location {
            headers.insert(LOCATION, HeaderValue::from_str(location).unwrap());
        }
        http::Response {
            headers,
            url: EMAIL_URL.parse().unwrap(),
            context: Arc::new(RequestContext {
                method: "GET".into(),
                url: EMAIL_URL.into(),
                status_code: Some(302),
                error_code: None,
                response_body: Some(String::new()),
            }),
        }
    }

    fn raw_mail_response(url: &str, status: u16, body: &str) -> http::Response {
        let error_code = (!(200..300).contains(&status))
            .then(|| serde_json::from_str::<Value>(body).ok())
            .flatten()
            .and_then(|value| value.get("code").and_then(Value::as_str).map(str::to_owned));
        http::Response {
            headers: HeaderMap::new(),
            url: url.parse().unwrap(),
            context: Arc::new(RequestContext {
                method: "POST".into(),
                url: url.into(),
                status_code: Some(status),
                error_code,
                response_body: Some(body.into()),
            }),
        }
    }

    fn folders_response(value: &Value) -> http::Response {
        raw_mail_response(MAIL_API_URL, 200, &value.to_string())
    }

    fn messages_response(value: &Value) -> http::Response {
        raw_mail_response(MAIL_JSON_API_URL, 200, &value.to_string())
    }

    #[test]
    fn parse_session_extracts_webmail_url_and_sid() {
        let target = "https://mail.v.zzu.edu.cn/coremail/main.jsp?sid=BAabcdef123456";
        let session =
            parse_session_response(&redirect_response(Some(target)), "cas-tok".into(), 1000)
                .unwrap();
        assert_eq!(session.webmail_url, target);
        assert_eq!(session.sid, "BAabcdef123456");
        assert_eq!(session.cas_token, "cas-tok");
        assert_eq!(session.obtained_at, 1000);

        for bad in [
            None,
            Some(""),
            Some("https://mail.v.zzu.edu.cn/coremail/main.jsp"),
            Some("https://mail.v.zzu.edu.cn/coremail/main.jsp?sid="),
        ] {
            let error = parse_session_response(&redirect_response(bad), "cas-tok".into(), 1000)
                .err()
                .unwrap();
            assert!(error.is_decode());
            assert!(error.request().is_some());
        }
    }

    #[test]
    fn session_freshness_checks_cas_token_and_ttl() {
        let now = Timestamp::now().as_second();
        let fresh = Session {
            webmail_url: "https://mail.v.zzu.edu.cn/coremail/main.jsp?sid=abc".into(),
            sid: "abc".into(),
            cas_token: "token-1".into(),
            obtained_at: now - 60,
        };
        assert!(fresh.is_current("token-1"));
        assert!(!fresh.is_current("token-2"));

        let expired = Session {
            obtained_at: now - SESSION_MAX_AGE_SECONDS,
            ..fresh
        };
        assert!(!expired.is_current("token-1"));
    }

    #[test]
    fn parse_unread_count_reads_only_inbox_and_ignores_other_folders() {
        let response = folders_response(&json!({
            "code": "S_OK",
            "var": [
                {"id": -5, "name": "待办邮件", "stats": {"messageCount": 3, "unreadMessageCount": 3}},
                {"id": 1, "name": "收件箱", "stats": {"messageCount": 25, "unreadMessageCount": 0}},
                {"id": 5, "name": "垃圾邮件", "stats": {"messageCount": 2, "unreadMessageCount": 2}}
            ]
        }));
        assert!(matches!(
            parse_unread_count_response(response).unwrap(),
            CoremailOutcome::Ok(0)
        ));

        let with_unread = folders_response(&json!({
            "code": "S_OK",
            "var": [
                {"id": 1, "name": "收件箱", "stats": {"unreadMessageCount": 7}},
                {"id": 5, "name": "垃圾邮件", "stats": {"unreadMessageCount": 4}}
            ]
        }));
        assert!(matches!(
            parse_unread_count_response(with_unread).unwrap(),
            CoremailOutcome::Ok(7)
        ));
    }

    #[test]
    fn parse_unread_count_classifies_session_expired_remote_http_and_decode_errors() {
        let expired_http_500 = raw_mail_response(
            MAIL_API_URL,
            500,
            r#"{
"result":"error",
"backURL":"/coremail/",
"backTarget":"_parent",
"errorMsg":"会话已过期, 请重新登录",
"errorCode":"FA_INVALID_SESSION"}"#,
        );
        let CoremailOutcome::SessionExpired(error) =
            parse_unread_count_response(expired_http_500).unwrap()
        else {
            panic!("expected session expired outcome");
        };
        assert!(error.is_remote());
        assert_eq!(error.message(), "会话已过期, 请重新登录");
        let context = error.request().unwrap();
        assert_eq!(context.status_code, Some(500));
        assert_eq!(context.error_code.as_deref(), Some("FA_INVALID_SESSION"));

        for code in ["FA_INVALID_TEMP_SESSION", "FA_SECURITY"] {
            let response = raw_mail_response(
                MAIL_API_URL,
                500,
                &json!({"result": "error", "errorCode": code, "errorMsg": "请重新登录"})
                    .to_string(),
            );
            assert!(matches!(
                parse_unread_count_response(response).unwrap(),
                CoremailOutcome::SessionExpired(_)
            ));
        }

        let remote_code = folders_response(&json!({"code": "FA_UNKNOWN"}));
        let error = parse_unread_count_response(remote_code).err().unwrap();
        assert!(error.is_remote());
        assert!(error.message().contains("FA_UNKNOWN"));

        let html_http_500 =
            raw_mail_response(MAIL_API_URL, 500, "<html>500 Internal Server Error</html>");
        assert!(
            parse_unread_count_response(html_http_500)
                .err()
                .unwrap()
                .is_http()
        );

        let invalid_json_200 = raw_mail_response(MAIL_API_URL, 200, "not json");
        assert!(
            parse_unread_count_response(invalid_json_200)
                .err()
                .unwrap()
                .is_decode()
        );

        for malformed in [
            json!({}),
            json!({"code": "S_OK"}),
            json!({"code": "S_OK", "var": []}),
            json!({"code": "S_OK", "var": [{"id": 1, "stats": {}}]}),
        ] {
            assert!(
                parse_unread_count_response(folders_response(&malformed))
                    .err()
                    .unwrap()
                    .is_decode()
            );
        }
    }

    #[test]
    fn parse_messages_normalizes_coremail_summaries_pagination_and_roundtrips() {
        let (start, size) = validate_mail_pagination(2, Some(10)).unwrap();
        assert_eq!((start, size), (10, 10));
        assert_eq!(
            build_list_messages_body(start, size),
            json!({
                "fid": 1,
                "start": 10,
                "limit": 10,
                "order": "receivedDate",
                "desc": true,
                "returnTotal": true,
                "summaryWindowSize": 10,
            })
        );

        let response = messages_response(&json!({
            "code": "S_OK",
            "total": 21,
            "midoffset": -1,
            "var": [
                {
                    "id": "2:msg0001",
                    "fid": 1,
                    "size": 4096,
                    "from": "\"教务通知\" <academic@example.edu.cn>",
                    "to": "\"学生\" <student@stu.example.edu.cn>",
                    "subject": "课程选课通知",
                    "summary": "  请于本周五前完成选课确认  ",
                    "sentDate": "2026-11-01 09:00:00",
                    "receivedDate": "2026-11-01 09:00:05",
                    "flags": {"read": false, "attached": true, "flagged": true}
                },
                {
                    "id": "2:msg0002",
                    "fid": 1,
                    "size": 1024,
                    "from": "system@example.edu.cn",
                    "to": "student@stu.example.edu.cn",
                    "subject": "",
                    "summary": "   ",
                    "sentDate": "",
                    "receivedDate": "2026-10-31 18:30:00",
                    "flags": {"read": true}
                }
            ]
        }));

        let CoremailOutcome::Ok(page) = parse_messages_response(response, 1, 20).unwrap() else {
            panic!("expected message page");
        };
        assert_eq!(page.page, 1);
        assert_eq!(page.page_size, 20);
        assert_eq!(page.total_items, 21);
        assert_eq!(page.total_pages, 2);
        assert_eq!(page.items.len(), 2);

        let first = &page.items[0];
        assert_eq!(first.id, "2:msg0001");
        assert_eq!(first.summary.as_deref(), Some("请于本周五前完成选课确认"));
        assert!(!first.is_read);
        assert!(first.has_attachment);
        assert!(first.is_flagged);
        assert_eq!(
            first.received_time.to_string(),
            "2026-11-01T09:00:05+08:00[Asia/Shanghai]"
        );

        let second = &page.items[1];
        assert_eq!(second.summary, None);
        assert_eq!(second.sent_time, None);
        assert!(second.is_read);
        assert!(!second.has_attachment);

        let encoded = serde_json::to_value(&page).unwrap();
        let restored: MailMessagePage = serde_json::from_value(encoded).unwrap();
        assert_eq!(restored, page);

        let expired = raw_mail_response(
            MAIL_JSON_API_URL,
            200,
            &json!({"code": "FA_INVALID_SESSION", "errorMsg": "会话失效"}).to_string(),
        );
        assert!(matches!(
            parse_messages_response(expired, 1, 20).unwrap(),
            CoremailOutcome::SessionExpired(_)
        ));

        let bad_received = messages_response(&json!({
            "code": "S_OK",
            "total": 1,
            "var": [{
                "id": "2:msg0003",
                "receivedDate": "invalid-date"
            }]
        }));
        let err = parse_messages_response(bad_received, 1, 20).err().unwrap();
        assert!(err.is_decode());
        assert!(std::error::Error::source(&err).is_some());
    }

    #[tokio::test]
    async fn unauthenticated_mail_client_validates_pagination_and_rejects_operations() {
        let cas = CasClient::new("", "", "test-device").unwrap();
        let mut mail = MailClient::new(cas).unwrap();
        assert!(!mail.is_logged_in());
        assert!(mail.messages(0, None).await.unwrap_err().is_invalid_input());
        assert!(
            mail.messages(1, Some(0))
                .await
                .unwrap_err()
                .is_invalid_input()
        );
        assert!(mail.webmail_url().await.unwrap_err().is_not_authenticated());
        assert!(
            mail.unread_count()
                .await
                .unwrap_err()
                .is_not_authenticated()
        );
        assert!(
            mail.messages(1, None)
                .await
                .unwrap_err()
                .is_not_authenticated()
        );
        assert!(mail.login().await.unwrap_err().is_not_authenticated());
    }
}
