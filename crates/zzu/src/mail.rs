//! 校园邮箱（Coremail）客户端，提供网页版入口地址与收件箱未读邮件数查询。

use std::sync::{Arc, Mutex, MutexGuard};

use jiff::Timestamp;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

use crate::{
    Error, Result,
    auth::{CasClient, fetch_email_login_redirect},
    http,
};

const MAIL_API_URL: &str = "https://mail.v.zzu.edu.cn/coremail/XT/jsp/mail.jsp";
const SESSION_MAX_AGE_SECONDS: i64 = 20 * 60;
const INBOX_FOLDER_ID: i64 = 1;

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
/// 即可获取网页版邮箱入口地址并查询收件箱未读邮件数。
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
            FoldersOutcome::Unread(count) => Ok(count),
            FoldersOutcome::SessionExpired(_) => {
                let refreshed = self.refresh_session_if_unchanged(&session.sid).await?;
                match self.fetch_unread_count(&refreshed.sid).await? {
                    FoldersOutcome::Unread(count) => Ok(count),
                    FoldersOutcome::SessionExpired(error) => Err(error),
                }
            }
        }
    }

    async fn fetch_unread_count(&self, sid: &str) -> Result<FoldersOutcome> {
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
}

enum FoldersOutcome {
    Unread(u32),
    SessionExpired(Error),
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

fn parse_unread_count_response(response: http::Response) -> Result<FoldersOutcome> {
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

    let value: Value = match serde_json::from_str(response.body()) {
        Ok(value) => value,
        Err(decode_error) => {
            let response = response.require_success()?;
            return Err(Error::from(decode_error).with_request(response.context));
        }
    };

    if let Some(code) = non_empty_str(&value, "code") {
        if code == "S_OK" {
            let context = response.context;
            let folders_value = value.get("var").ok_or_else(|| {
                Error::decode_message("邮箱文件夹响应缺少 var 字段".into())
                    .with_request(context.clone())
            })?;
            let folders: Vec<Folder> = Vec::deserialize(folders_value)
                .map_err(|error| Error::from(error).with_request(context.clone()))?;
            let inbox = folders
                .into_iter()
                .find(|folder| folder.id == INBOX_FOLDER_ID)
                .ok_or_else(|| {
                    Error::decode_message("邮箱文件夹列表缺少收件箱".into())
                        .with_request(context.clone())
                })?;
            let unread = inbox
                .stats
                .and_then(|stats| stats.unread_message_count)
                .ok_or_else(|| {
                    Error::decode_message("收件箱缺少 unreadMessageCount 字段".into())
                        .with_request(context)
                })?;
            return Ok(FoldersOutcome::Unread(unread));
        }
        return classify_coremail_error(&value, Some(code), &response.context);
    }

    if value.get("result").and_then(Value::as_str) == Some("error") {
        let error_code = non_empty_str(&value, "errorCode");
        return classify_coremail_error(&value, error_code, &response.context);
    }

    let response = response.require_success()?;
    Err(
        Error::decode_message("邮箱文件夹响应缺少有效的 code 字段".into())
            .with_request(response.context),
    )
}

fn classify_coremail_error(
    value: &Value,
    code: Option<&str>,
    context: &Arc<crate::RequestContext>,
) -> Result<FoldersOutcome> {
    let mut updated = (**context).clone();
    if let Some(code) = code {
        updated.error_code = Some(code.to_owned());
    }
    let error = Error::remote(coremail_error_message(value, code)).with_request(Arc::new(updated));
    if code.is_some_and(is_session_expired_code) {
        return Ok(FoldersOutcome::SessionExpired(error));
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

    fn raw_folders_response(status: u16, body: &str) -> http::Response {
        let error_code = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|value| value.get("code").and_then(Value::as_str).map(str::to_owned));
        http::Response {
            headers: HeaderMap::new(),
            url: MAIL_API_URL.parse().unwrap(),
            context: Arc::new(RequestContext {
                method: "POST".into(),
                url: MAIL_API_URL.into(),
                status_code: Some(status),
                error_code,
                response_body: Some(body.into()),
            }),
        }
    }

    fn folders_response(value: &Value) -> http::Response {
        raw_folders_response(200, &value.to_string())
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
                {
                    "id": -5,
                    "vp": {"filter": {"flags": {"deferHandle": true}}},
                    "noTr": true,
                    "name": "待办邮件",
                    "flags": {},
                    "stats": {"messageCount": 3, "unreadMessageCount": 3}
                },
                {
                    "id": -8,
                    "vp": {"tags": true},
                    "name": "标签邮件",
                    "flags": {},
                    "stats": {"messageCount": 1, "unreadMessageCount": 1}
                },
                {
                    "id": 1,
                    "name": "收件箱",
                    "flags": {"system": true},
                    "keepPeriod": -1,
                    "stats": {
                        "messageCount": 110,
                        "messageSize": 1_900_163,
                        "unreadMessageCount": 0,
                        "unreadMessageSize": 0
                    }
                },
                {
                    "id": 2,
                    "name": "草稿箱",
                    "flags": {"system": true},
                    "keepPeriod": -1,
                    "stats": {
                        "messageCount": 1,
                        "messageSize": 2081,
                        "unreadMessageCount": 0,
                        "unreadMessageSize": 0
                    },
                    "noTr": true
                },
                {
                    "id": 5,
                    "name": "垃圾邮件",
                    "flags": {"system": true},
                    "keepPeriod": 30,
                    "stats": {
                        "messageCount": 2,
                        "messageSize": 17_531,
                        "unreadMessageCount": 2,
                        "unreadMessageSize": 17_531
                    },
                    "noTr": true
                }
            ]
        }));
        assert!(matches!(
            parse_unread_count_response(response).unwrap(),
            FoldersOutcome::Unread(0)
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
            FoldersOutcome::Unread(7)
        ));
    }

    #[test]
    fn parse_unread_count_classifies_session_expired_remote_http_and_decode_errors() {
        // 实测响应：sid 过期时 Coremail 以 HTTP 500 返回 result=error / errorCode / errorMsg。
        let expired_http_500 = raw_folders_response(
            500,
            r#"{
"result":"error",
"backURL":"/coremail/",
"backTarget":"_parent",
"errorMsg":"会话已过期, 请重新登录",
"errorCode":"FA_INVALID_SESSION"}"#,
        );
        let FoldersOutcome::SessionExpired(error) =
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
            let response = raw_folders_response(
                500,
                &json!({"result": "error", "errorCode": code, "errorMsg": "请重新登录"})
                    .to_string(),
            );
            assert!(matches!(
                parse_unread_count_response(response).unwrap(),
                FoldersOutcome::SessionExpired(_)
            ));
        }

        let remote_http_500 = raw_folders_response(
            500,
            &json!({"result": "error", "errorCode": "FA_SERVER_BUSY", "errorMsg": "服务器正忙"})
                .to_string(),
        );
        let error = parse_unread_count_response(remote_http_500).err().unwrap();
        assert!(error.is_remote());
        assert_eq!(error.message(), "服务器正忙");
        assert_eq!(
            error.request().and_then(|ctx| ctx.error_code.as_deref()),
            Some("FA_SERVER_BUSY")
        );

        let remote_code = folders_response(&json!({
            "code": "FA_UNKNOWN"
        }));
        let error = parse_unread_count_response(remote_code).err().unwrap();
        assert!(error.is_remote());
        assert!(error.message().contains("FA_UNKNOWN"));
        assert!(error.request().is_some());

        let html_http_500 = raw_folders_response(500, "<html>500 Internal Server Error</html>");
        let error = parse_unread_count_response(html_http_500).err().unwrap();
        assert!(error.is_http());
        assert_eq!(error.request().and_then(|ctx| ctx.status_code), Some(500));

        let invalid_json_200 = raw_folders_response(200, "not json");
        let error = parse_unread_count_response(invalid_json_200).err().unwrap();
        assert!(error.is_decode());
        assert!(error.request().is_some());

        for malformed in [
            json!({}),
            json!({"code": 0, "var": []}),
            json!({"code": "S_OK"}),
            json!({"code": "S_OK", "var": []}),
            json!({"code": "S_OK", "var": [{"id": 1, "stats": {}}]}),
        ] {
            let error = parse_unread_count_response(folders_response(&malformed))
                .err()
                .unwrap();
            assert!(error.is_decode());
            assert!(error.request().is_some());
        }
    }

    #[tokio::test]
    async fn unauthenticated_mail_client_rejects_operations() {
        let cas = CasClient::new("", "", "test-device").unwrap();
        let mut mail = MailClient::new(cas).unwrap();
        assert!(!mail.is_logged_in());
        assert!(mail.webmail_url().await.unwrap_err().is_not_authenticated());
        assert!(
            mail.unread_count()
                .await
                .unwrap_err()
                .is_not_authenticated()
        );
        assert!(mail.login().await.unwrap_err().is_not_authenticated());
    }
}
