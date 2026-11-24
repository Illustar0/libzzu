//! 统一身份认证（CAS）会话管理、短信多因素认证、设备信任管理、个人信息查询与业务系统单点登录。

use std::sync::Arc;

use jiff::Timestamp;
use reqwest::{Client, Url};
use rsa::RsaPublicKey;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{AuthenticationErrorKind, Error, Result, crypto, http};

const CAS_URL: &str = "https://cas.s.zzu.edu.cn";
const APP_AGENT: &str = "SWSuperApp/1.1.1()";
const AUTHX_URL: &str = "https://authx-service.s.zzu.edu.cn";
const INFO_URL: &str = "https://authx-service.s.zzu.edu.cn/personal/api/v1/personal/me/user";
pub(crate) const EMAIL_URL: &str = "https://service.s.zzu.edu.cn/portalApi/api/open/email/login";
const PRESIDENT_MAILBOX_URL: &str = "https://mailbox.v.zzu.edu.cn/mobile/auth2/cas";
const ECARD_URL: &str = "https://ecard.v.zzu.edu.cn/server/auth/host/open?host=11&org=2";
const LIBRARY_URL: &str = "https://library.zzu.edu.cn/sso/login/3rd/22757";
const LIBRARY_RESERVATION_URL: &str = "http://order.lib.zzu.edu.cn/v4/login/cas";
const LEAVE_URL: &str = "https://jf.v.zzu.edu.cn/jksb/servlet/qjxtcas?returnUrl=https%3A%2F%2Fjf.v.zzu.edu.cn%2Fleave%2Finforhome.jsp";

/// 可通过统一身份认证获取单点登录跳转地址的业务系统。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Service {
    /// 校园邮箱系统。
    Email,
    /// 图书馆门户系统。
    Library,
    /// 图书馆空间（座位与研讨间）预约系统。
    LibraryReservation,
    /// 学生请假系统。
    Leave,
    /// 校长信箱系统。
    PresidentMailbox,
    /// 校园卡（一卡通）系统。
    Ecard,
    /// 本科教务系统（EAMS）下的指定前端子应用。
    UndergradEas(UndergradEasService),
}

/// 本科教务系统（EAMS）支持携带令牌直接跳转的前端子应用。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UndergradEasService {
    /// 学籍信息查询。
    StudentStatus,
    /// 培养方案完成情况。
    ProgramCompletion,
    /// 考试查询。
    Exams,
    /// 学业预警查询。
    AcademicWarnings,
    /// 成绩查询。
    Grades,
    /// 课表查询。
    Timetable,
    /// 全校开课查询。
    CourseOfferings,
    /// 校历。
    SchoolCalendar,
    /// 空闲教室查询。
    AvailableClassrooms,
    /// 教室课表。
    ClassroomTimetable,
}

impl UndergradEasService {
    const fn url(self) -> &'static str {
        match self {
            Self::StudentStatus => "https://jwxt.zzu.edu.cn/eams-student-status-app/index.html",
            Self::ProgramCompletion => {
                "https://jwxt.zzu.edu.cn/eams-student-plan-complete-app/index.html"
            }
            Self::Exams => "https://jwxt.zzu.edu.cn/eams-exam-student-app/index.html",
            Self::AcademicWarnings => {
                "https://jwxt.zzu.edu.cn/eams-study-precaution-search-app/index.html"
            }
            Self::Grades => "https://jwxt.zzu.edu.cn/eams-student-grade-app/index.html",
            Self::Timetable => "https://jwxt.zzu.edu.cn/eams-student-course-table-app/index.html",
            Self::CourseOfferings => "https://jwxt.zzu.edu.cn/eams-lesson-search-app/index.html",
            Self::SchoolCalendar => "https://jwxt.zzu.edu.cn/eams-school-calendar-app/index.html",
            Self::AvailableClassrooms => "https://jwxt.zzu.edu.cn/eams-room-occupy-app/index.html",
            Self::ClassroomTimetable => {
                "https://jwxt.zzu.edu.cn/eams-room-course-table-app/index.html"
            }
        }
    }
}

/// 统一身份认证签发的用户令牌与刷新令牌对。
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tokens {
    /// 访问各业务系统的用户 JWT（有效期约 1 个月；反序列化时兼容 `idToken` 字段名）。
    #[serde(alias = "idToken")]
    pub user_token: String,
    /// 登录时一并签发的长效刷新 JWT（有效期约 2 个月）。
    ///
    /// 本库仅在预置或复用令牌时校验其尚未过期，并原样保留供外部持久化，不会调用刷新接口自动续期。
    pub refresh_token: String,
}

impl Tokens {
    fn is_reusable(&self) -> bool {
        let now = Timestamp::now().as_second();
        matches!(crypto::jwt_expiration(&self.user_token), Ok(expiration) if expiration > now + 900)
            && matches!(crypto::jwt_expiration(&self.refresh_token), Ok(expiration) if expiration > now)
    }

    fn validate(&self) -> Result<()> {
        let now = Timestamp::now().as_second();
        if crypto::jwt_expiration(&self.user_token)? <= now
            || crypto::jwt_expiration(&self.refresh_token)? <= now
        {
            return Err(Error::authentication(
                AuthenticationErrorKind::TokenExpired,
                "Token 已过期".into(),
            ));
        }
        Ok(())
    }
}

/// 当前会话的多因素认证（MFA）检测与校验状态。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MfaStatus {
    /// 当前设备与账号登录是否需要多因素认证。
    pub is_required: bool,
    /// 当前账号是否支持安全手机短信验证。
    pub can_use_sms: bool,
    /// 当前多因素认证是否已校验通过。
    pub is_verified: bool,
}

/// 短信多因素认证的发送上下文。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SmsChallenge {
    /// 服务端返回的脱敏安全手机号；服务端未提供时为 `None`。
    pub secure_phone: Option<String>,
}

#[derive(Default)]
struct MfaState {
    state: String,
    gid: String,
    secure_phone: Option<String>,
    attest_url: String,
    is_sms_sent: bool,
    status: MfaStatus,
}

impl MfaState {
    fn sms_challenge(&self) -> SmsChallenge {
        SmsChallenge {
            secure_phone: self.secure_phone.clone(),
        }
    }
}

struct Session {
    key: Option<RsaPublicKey>,
    tokens: Option<Tokens>,
    is_logged_in: bool,
    mfa: MfaState,
}

struct Inner {
    http: Client,
    account: String,
    password: String,
    device_id: String,
    session: Mutex<Session>,
}

/// 统一身份认证（CAS）客户端。
///
/// 内部通过引用计数与异步互斥锁维护认证会话。克隆出的实例共享同一套令牌缓存、
/// 设备标识与登录状态，可直接传给 [`crate::eas::UndergradEasClient`] 与 [`crate::ecard::EcardClient`]。
#[derive(Clone)]
pub struct CasClient {
    inner: Arc<Inner>,
}

impl CasClient {
    /// 使用账号、密码、设备 ID 与默认 HTTP 配置创建统一身份认证客户端。
    ///
    /// `device_id` 可以是任意包含非空白字符的字符串，但它本质上是凭证，请确保它唯一且难以预测。
    /// 仅初始化本地状态，不发起网络请求；实际认证须调用 [`Self::login`]。
    ///
    /// # Errors
    ///
    /// `device_id` 为空或仅含空白字符时返回 [`Error::InvalidInput`]。底层 TLS 或默认 HTTP 客户端初始化失败时也会返回错误。
    pub fn new(
        account: impl Into<String>,
        password: impl Into<String>,
        device_id: impl Into<String>,
    ) -> Result<Self> {
        Self::with_http_client(account, password, device_id, http::client()?)
    }

    /// 使用自定义 [`reqwest::Client`] 创建统一身份认证客户端。
    ///
    /// `client` 必须禁用自动重定向（如通过 [`crate::http_client_builder`] 构建），以便 [`Self::service_login_url`] 直接读取单点登录响应的 `Location` 头。
    /// `device_id` 可以是任意包含非空白字符的字符串，但它本质上是凭证，请确保它唯一且难以预测。
    ///
    /// # Errors
    ///
    /// `device_id` 为空或仅含空白字符时返回 [`Error::InvalidInput`]。
    pub fn with_http_client(
        account: impl Into<String>,
        password: impl Into<String>,
        device_id: impl Into<String>,
        client: Client,
    ) -> Result<Self> {
        let device_id = device_id.into();
        if device_id.trim().is_empty() {
            return Err(Error::invalid_input("设备 ID 必须包含非空白字符".into()));
        }
        Ok(Self {
            inner: Arc::new(Inner {
                http: client,
                account: account.into(),
                password: password.into(),
                device_id,
                session: Mutex::new(Session {
                    key: None,
                    tokens: None,
                    is_logged_in: false,
                    mfa: MfaState::default(),
                }),
            }),
        })
    }

    /// 预置外部持久化的认证令牌对，并重置会话的登录与 MFA 状态。
    ///
    /// 预置后仍须调用 [`Self::login`] 激活会话；若令牌尚在可复用有效期内，[`Self::login`] 会直接复用，不发起密码登录请求。
    ///
    /// # Errors
    ///
    /// `user_token` 或 `refresh_token` 不是合法的 JWT，或任一令牌已过期时，返回 [`Error::InvalidInput`]。
    pub async fn set_tokens(&self, tokens: Tokens) -> Result<()> {
        tokens
            .validate()
            .map_err(|error| Error::invalid_input(error.to_string()))?;
        let mut session = self.inner.session.lock().await;
        session.tokens = Some(tokens);
        session.is_logged_in = false;
        session.mfa = MfaState::default();
        Ok(())
    }

    /// 返回当前会话缓存的令牌对副本；未设置且未登录时返回 `None`。
    pub async fn tokens(&self) -> Option<Tokens> {
        self.inner.session.lock().await.tokens.clone()
    }

    /// 判断当前会话是否已完成登录。
    pub async fn is_logged_in(&self) -> bool {
        self.inner.session.lock().await.is_logged_in
    }

    /// 登录统一身份认证会话。
    ///
    /// 若已缓存距离过期超过 15 分钟的 `user_token` 且 `refresh_token` 未过期，则直接复用现有令牌；
    /// 否则自动检测 MFA 状态、获取服务端 RSA 公钥并发起密码登录。
    /// 若密码登录收到服务端拒绝或无效响应，会自动清空当前 MFA 状态；网络传输失败时保留已验证状态，便于重试。
    ///
    /// # Errors
    ///
    /// - 服务端要求多因素认证且尚未通过校验时，返回 [`AuthenticationErrorKind::MfaRequired`]。可先调用 [`Self::prepare_sms`] 预览脱敏手机号，再依次调用 [`Self::send_sms`]、[`Self::verify_sms`] 与 [`Self::login`]。
    /// - 账号或密码错误时，返回 [`AuthenticationErrorKind::InvalidCredentials`]。
    /// - 网络传输失败、公钥或响应解析失败、RSA 加密失败，或服务端签发的令牌已过期时返回错误。
    pub async fn login(&self) -> Result<()> {
        let mut session = self.inner.session.lock().await;
        self.login_session(&mut session).await
    }

    /// 丢弃已缓存的令牌与 MFA 状态，强制重新用密码登录。
    ///
    /// # Errors
    ///
    /// 错误条件同 [`Self::login`]。
    pub async fn reauthenticate(&self) -> Result<()> {
        let mut session = self.inner.session.lock().await;
        session.tokens = None;
        session.mfa = MfaState::default();
        self.login_session(&mut session).await
    }

    /// 获取已登录会话的可用 `user_token`。
    ///
    /// 本方法不发起网络请求，也不会用密码重新登录：密码重登可能挤掉手机端 App 的在线状态，须由调用方显式调用 [`Self::login`] 触发。
    ///
    /// # Errors
    ///
    /// - 尚未调用 [`Self::login`] 登录或已调用 [`Self::logout`] 登出时，返回 [`AuthenticationErrorKind::NotAuthenticated`]。
    /// - `user_token` 剩余有效期不足 15 分钟或 `refresh_token` 已过期时，返回 [`AuthenticationErrorKind::TokenExpired`]。
    pub async fn user_token(&self) -> Result<String> {
        let session = self.inner.session.lock().await;
        if !session.is_logged_in {
            return Err(Error::not_authenticated());
        }
        let tokens = session.tokens.as_ref().ok_or(Error::not_authenticated())?;
        if !tokens.is_reusable() {
            return Err(Error::authentication(
                AuthenticationErrorKind::TokenExpired,
                "Token 已过期或即将过期".into(),
            ));
        }
        Ok(tokens.user_token.clone())
    }

    async fn login_session(&self, session: &mut Session) -> Result<()> {
        if session.tokens.as_ref().is_some_and(Tokens::is_reusable) {
            session.is_logged_in = true;
            return Ok(());
        }
        session.is_logged_in = false;
        if session.mfa.state.is_empty() {
            self.detect_mfa_session(session).await?;
        }
        if session.mfa.status.is_required && !session.mfa.status.is_verified {
            return Err(Error::mfa_required());
        }
        let key = self.public_key(session).await?;
        let credentials = EncryptedCredentials {
            username: crypto::rsa_encrypt(key, &self.inner.account)?,
            password: crypto::rsa_encrypt(key, &self.inner.password)?,
        };
        let query = PasswordLogin {
            credentials,
            app_id: "com.supwisdom.zzu",
            os_type: "android",
            geo: "",
            device_id: &self.inner.device_id,
            client_id: "",
            mfa_state: &session.mfa.state,
        };
        let response = match http::json(
            self.inner
                .http
                .post(format!("{CAS_URL}/token/password/passwordLogin"))
                .header("User-Agent", APP_AGENT)
                .query(&query),
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                if !error.is_transport() {
                    session.mfa = MfaState::default();
                }
                return Err(password_login_error(error));
            }
        };
        let context = response.context.clone();
        let tokens: Tokens = match http::data(response, "code") {
            Ok(tokens) => tokens,
            Err(error) => {
                session.mfa = MfaState::default();
                return Err(password_login_error(error));
            }
        };
        if let Err(error) = tokens.validate() {
            session.mfa = MfaState::default();
            return Err(error.with_request(context));
        }
        session.tokens = Some(tokens);
        session.is_logged_in = true;
        session.mfa = MfaState::default();
        Ok(())
    }

    async fn public_key<'a>(&self, session: &'a mut Session) -> Result<&'a RsaPublicKey> {
        if session.key.is_none() {
            let request = self
                .inner
                .http
                .get(format!("{CAS_URL}/token/jwt/publicKey"))
                .header("User-Agent", "okhttp/3.12.1");
            let response = http::send(request).await?.require_success()?;
            session.key = Some(
                crypto::rsa_key(response.body())
                    .map_err(|error| Error::decode(error).with_request(response.context))?,
            );
        }
        session
            .key
            .as_ref()
            .ok_or_else(|| Error::crypto("认证公钥不存在".into()))
    }

    /// 向服务端探测当前账号与设备是否需要多因素认证，并更新内部 MFA 状态。
    ///
    /// 每次调用都会重置未完成的 MFA 状态，可在会话过期后用于重新开启一轮验证。
    ///
    /// # Errors
    ///
    /// 获取公钥、RSA 加密、网络请求失败，或服务端返回的 MFA 状态字段缺失时返回错误。
    pub async fn detect_mfa(&self) -> Result<MfaStatus> {
        let mut session = self.inner.session.lock().await;
        self.detect_mfa_session(&mut session).await?;
        Ok(session.mfa.status)
    }

    async fn detect_mfa_session(&self, session: &mut Session) -> Result<()> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct DetectionRequest<'a> {
            #[serde(flatten)]
            credentials: EncryptedCredentials,
            device_id: &'a str,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Detection {
            state: String,
            need: bool,
            #[serde(default)]
            mfa_type_secure_phone: bool,
        }
        session.mfa = MfaState::default();
        let key = self.public_key(session).await?;
        let credentials = EncryptedCredentials {
            username: crypto::rsa_encrypt(key, &self.inner.account)?,
            password: crypto::rsa_encrypt(key, &self.inner.password)?,
        };
        let query = DetectionRequest {
            credentials,
            device_id: &self.inner.device_id,
        };
        let request = self
            .inner
            .http
            .post(format!("{CAS_URL}/token/mfa/detect"))
            .header("User-Agent", APP_AGENT)
            .query(&query);
        let response = http::json(request)
            .await
            .map_err(unknown_authentication_error)?;
        let context = response.context.clone();
        let detection: Detection =
            http::data(response, "code").map_err(unknown_authentication_error)?;
        if detection.state.is_empty() {
            return Err(Error::decode_message("MFA state 为空".into()).with_request(context));
        }
        session.mfa.state = detection.state;
        session.mfa.status.is_required = detection.need;
        session.mfa.status.can_use_sms = detection.mfa_type_secure_phone;
        Ok(())
    }

    /// 初始化短信 MFA 会话并返回发送上下文（含脱敏安全手机号），不发送短信。
    ///
    /// 该步骤可选，适合先在界面展示脱敏手机号，再调用 [`Self::send_sms`]。
    /// 若尚未探测 MFA 状态，会先自动探测；同一轮验证内重复调用直接返回已缓存的上下文。
    ///
    /// # Errors
    ///
    /// - 会话无需 MFA 或不支持短信验证时，返回 [`AuthenticationErrorKind::InvalidMfaState`]。
    /// - 探测或初始化请求失败时返回错误；若收到服务端拒绝或无效响应，会自动清空 MFA 状态。
    pub async fn prepare_sms(&self) -> Result<SmsChallenge> {
        let mut session = self.inner.session.lock().await;
        self.prepare_sms_session(&mut session).await
    }

    async fn prepare_sms_session(&self, session: &mut Session) -> Result<SmsChallenge> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Initialization {
            gid: String,
            #[serde(default)]
            secure_phone: Option<String>,
            #[serde(default)]
            attest_server_url: Option<String>,
        }
        if session.mfa.state.is_empty() {
            self.detect_mfa_session(session).await?;
        }
        if !session.mfa.status.is_required || !session.mfa.status.can_use_sms {
            return Err(Error::invalid_mfa_state(
                "当前会话不支持或不需要短信 MFA".into(),
            ));
        }
        if !session.mfa.gid.is_empty() {
            return Ok(session.mfa.sms_challenge());
        }
        let request = self
            .inner
            .http
            .get(format!("{CAS_URL}/token/mfa/initByType/securephone"))
            .header("User-Agent", APP_AGENT)
            .query(&[("state", &session.mfa.state)]);
        let response = match http::json(request).await {
            Ok(response) => response,
            Err(error) => {
                if !error.is_transport() {
                    session.mfa = MfaState::default();
                }
                return Err(unknown_authentication_error(error));
            }
        };
        let context = response.context.clone();
        let initialized: Initialization = match http::data(response, "code") {
            Ok(initialized) => initialized,
            Err(error) => {
                session.mfa = MfaState::default();
                return Err(unknown_authentication_error(error));
            }
        };
        if initialized.gid.trim().is_empty() {
            session.mfa = MfaState::default();
            return Err(Error::decode_message("MFA gid 为空".into()).with_request(context));
        }
        session.mfa.gid = initialized.gid;
        session.mfa.secure_phone = initialized
            .secure_phone
            .map(|phone| phone.trim().to_owned())
            .filter(|phone| !phone.is_empty());
        session.mfa.attest_url = initialized
            .attest_server_url
            .map(|url| url.trim().to_owned())
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| format!("{CAS_URL}/attest"));
        Ok(session.mfa.sms_challenge())
    }

    /// 请求服务端向绑定安全手机发送短信验证码，并返回发送上下文（含脱敏安全手机号）。
    ///
    /// 若尚未探测或初始化短信 MFA 会话，会先自动完成 [`Self::prepare_sms`]。
    /// 短信发送失败时（如触发频控冷却）会保留当前 MFA 会话，避免作废此前已发出的验证码；
    /// 若状态已过期，可调用 [`Self::detect_mfa`] 重开一轮验证。
    ///
    /// # Errors
    ///
    /// - 会话无需 MFA 或不支持短信验证时，返回 [`AuthenticationErrorKind::InvalidMfaState`]。
    /// - MFA 初始化或短信发送请求失败时返回错误。
    pub async fn send_sms(&self) -> Result<SmsChallenge> {
        let mut session = self.inner.session.lock().await;
        let challenge = self.prepare_sms_session(&mut session).await?;
        let url = format!(
            "{}/api/guard/securephone/send",
            session.mfa.attest_url.trim_end_matches('/')
        );
        let request = self
            .inner
            .http
            .post(url)
            .header("User-Agent", APP_AGENT)
            .json(&json!({"gid": session.mfa.gid}));
        let response = http::json(request)
            .await
            .map_err(unknown_authentication_error)?;
        let _: SmsResult = http::data(response, "code").map_err(unknown_authentication_error)?;
        session.mfa.is_sms_sent = true;
        session.mfa.status.is_verified = false;
        Ok(challenge)
    }

    /// 校验短信验证码以完成多因素认证。
    ///
    /// 校验通过后须再次调用 [`Self::login`] 完成令牌签发。
    /// 验证码不匹配时会保留当前 MFA 会话，可直接重试；若服务端返回非 2xx 或业务失败
    /// （如验证会话已过期），会自动清空 MFA 状态。
    ///
    /// # Errors
    ///
    /// - `code` 为空字符串时返回 [`Error::InvalidInput`]。
    /// - 尚未调用 [`Self::send_sms`] 发送短信验证码时，返回 [`AuthenticationErrorKind::InvalidMfaState`]。
    /// - 验证码不匹配或校验未通过时，返回 [`AuthenticationErrorKind::MfaVerificationFailed`]。
    /// - 网络传输失败、服务端拒绝或响应解析失败时返回错误。
    pub async fn verify_sms(&self, code: &str) -> Result<()> {
        #[derive(Deserialize)]
        struct Verification {
            status: i64,
            #[serde(rename = "result")]
            _result: Value,
        }
        if code.is_empty() {
            return Err(Error::invalid_input("短信验证码不能为空".into()));
        }
        let mut session = self.inner.session.lock().await;
        if session.mfa.gid.is_empty() || !session.mfa.is_sms_sent {
            return Err(Error::invalid_mfa_state("请先发送短信验证码".into()));
        }
        session.mfa.status.is_verified = false;
        let url = format!(
            "{}/api/guard/securephone/valid",
            session.mfa.attest_url.trim_end_matches('/')
        );
        let request = self
            .inner
            .http
            .post(url)
            .header("User-Agent", APP_AGENT)
            .json(&json!({"gid": session.mfa.gid, "code": code}));
        let response = match http::json(request).await {
            Ok(response) => response,
            Err(error) => {
                if error.is_http() {
                    session.mfa = MfaState::default();
                }
                return Err(unknown_authentication_error(error));
            }
        };
        let context = response.context.clone();
        let verification: Verification = match http::data(response, "code") {
            Ok(verification) => verification,
            Err(error) => {
                if error.is_remote() {
                    session.mfa = MfaState::default();
                }
                return Err(unknown_authentication_error(error));
            }
        };
        if verification.status != 2 {
            return Err(Error::authentication(
                AuthenticationErrorKind::MfaVerificationFailed,
                "短信验证码校验失败".into(),
            )
            .with_request(context));
        }
        session.mfa.status.is_verified = true;
        Ok(())
    }

    /// 更新当前账号下指定设备的信任状态（受信任设备登录时无须 MFA）。
    ///
    /// `is_trusted` 为 `true` 时信任设备，为 `false` 时取消信任。
    ///
    /// # Errors
    /// - `device_id` 为空时返回 [`Error::InvalidInput`]。
    /// - 客户端尚未登录或令牌不可用时返回 [`Error::Authentication`]。
    /// - 请求失败、服务端拒绝更新或响应格式不符时返回错误。
    pub async fn set_device_trust(&self, device_id: &str, is_trusted: bool) -> Result<()> {
        if device_id.is_empty() {
            return Err(Error::invalid_input("设备标识不能为空".into()));
        }
        let token = self.user_token().await?;
        let request = self
            .inner
            .http
            .put(format!(
                "{AUTHX_URL}/personal/api/v1/personal/user/accountUsedAgent/trustUsedAgent"
            ))
            .header("X-Id-Token", token)
            .json(&json!({"agentId": device_id, "agentTrust": is_trusted}));
        let response = http::json(request).await?;
        http::check_status(&response, "code")?;
        match response.get("acknowleged").and_then(Value::as_bool) {
            Some(true) => Ok(()),
            Some(false) => {
                Err(Error::remote(http::message(&response)).with_request(response.context))
            }
            None => Err(
                Error::decode_message("设备信任响应缺少有效的 acknowleged 字段".into())
                    .with_request(response.context),
            ),
        }
    }

    /// 查询当前用户的个人身份信息。
    ///
    /// # Errors
    ///
    /// - 客户端尚未登录或令牌不可用时返回 [`Error::Authentication`]。
    /// - 认证服务接口请求失败、业务状态非零或响应结构不符时返回错误。
    pub async fn user_info(&self) -> Result<PersonalInfo> {
        let token = self.user_token().await?;
        let response =
            http::json(self.inner.http.get(INFO_URL).header("X-Id-Token", token)).await?;
        parse_user_info(response)
    }

    /// 获取当前账号访问指定业务系统 `service` 的单点登录跳转 URL。
    ///
    /// - [`Service::PresidentMailbox`]、[`Service::Ecard`] 与 [`Service::UndergradEas`]：直接在本地用 `user_token` 拼接携带令牌参数（`token=...` 或 `idToken=...`）的目标地址，不发起网络请求。
    /// - 其余服务：携带 `user_token` 请求对应门户入口或 CAS 票据签发接口，不跟随重定向，直接提取响应头中的 `Location` 跳转地址。
    ///
    /// 返回的 URL 含单点登录票据或用户令牌，切勿写入日志。
    ///
    /// # Errors
    ///
    /// - 会话尚未登录或令牌不可用时，返回 [`Error::Authentication`]（细分为 [`AuthenticationErrorKind::NotAuthenticated`] 或 [`AuthenticationErrorKind::TokenExpired`]）。
    /// - 申请跳转票据时网络传输失败或返回非 2xx/3xx 状态码，返回 [`Error::Transport`] 或 [`Error::Http`]。
    /// - 重定向响应缺少非空 `Location` 头或目标 URL 构造失败时，返回 [`Error::Decode`]。
    pub async fn service_login_url(&self, service: Service) -> Result<String> {
        match service {
            Service::Email => self.email_login_url().await,
            Service::Library => self.library_login_url().await,
            Service::LibraryReservation => self.library_reservation_login_url().await,
            Service::Leave => self.leave_login_url().await,
            Service::PresidentMailbox => self.token_login_url(PRESIDENT_MAILBOX_URL).await,
            Service::Ecard => self.token_login_url(ECARD_URL).await,
            Service::UndergradEas(service) => self.undergrad_eas_login_url(service).await,
        }
    }

    /// 校园邮箱经门户接口单点登录，不走通用 CAS `service` 票据签发。
    async fn email_login_url(&self) -> Result<String> {
        let token = self.user_token().await?;
        let response = fetch_email_login_redirect(&self.inner.http, &token).await?;
        extract_redirect_location(response, "邮箱")
    }

    async fn library_login_url(&self) -> Result<String> {
        self.assign_ticket(LIBRARY_URL, "图书馆").await
    }

    async fn library_reservation_login_url(&self) -> Result<String> {
        self.assign_ticket(LIBRARY_RESERVATION_URL, "图书馆空间预约")
            .await
    }

    async fn leave_login_url(&self) -> Result<String> {
        self.assign_ticket(LEAVE_URL, "请假系统").await
    }

    /// 在 `base` 上追加 `token` 查询参数，供直接认可 CAS 用户令牌的业务系统使用。
    async fn token_login_url(&self, base: &str) -> Result<String> {
        let token = self.user_token().await?;
        let url = Url::parse_with_params(base, [("token", token)]).map_err(Error::decode)?;
        Ok(url.into())
    }

    async fn undergrad_eas_login_url(&self, service: UndergradEasService) -> Result<String> {
        let token = self.user_token().await?;
        let url =
            Url::parse_with_params(service.url(), [("idToken", token)]).map_err(Error::decode)?;
        Ok(url.into())
    }

    async fn assign_ticket(&self, service: &str, system_name: &str) -> Result<String> {
        let token = self.user_token().await?;
        let request = self
            .inner
            .http
            .get(format!("{CAS_URL}/cas/a/login"))
            .query(&[("service", service)])
            .header("User-Agent", APP_AGENT)
            .header("Cookie", format!("X-Id-Token={token}"))
            .header("X-Id-Token", token);
        let response = http::send(request).await?.allow_redirect()?;
        extract_redirect_location(response, system_name)
    }

    /// 清除本地缓存的令牌、MFA 状态与登录标记。
    ///
    /// 所有共享同一会话的 [`CasClient`] 实例会立即变为未登录状态。
    /// 派生的 [`crate::eas::UndergradEasClient`]、[`crate::ecard::EcardClient`] 与
    /// [`crate::mail::MailClient`] 仍保留各自的本地缓存（如 `is_logged_in()`、`current_semester()`），
    /// 但下次读取 [`Self::user_token`] 时会返回未认证错误。
    pub async fn logout(&self) {
        let mut session = self.inner.session.lock().await;
        session.tokens = None;
        session.mfa = MfaState::default();
        session.is_logged_in = false;
    }
}

/// 携带 CAS `user_token` 请求校园邮箱单点登录入口，返回不自动跟随的重定向响应。
pub(crate) async fn fetch_email_login_redirect(
    http: &Client,
    token: &str,
) -> Result<http::Response> {
    let request = http
        .get(EMAIL_URL)
        .header("Cookie", format!("X-Id-Token={token}"))
        .header("X-Id-Token", token);
    http::send(request).await?.allow_redirect()
}

fn extract_redirect_location(response: http::Response, system_name: &str) -> Result<String> {
    response
        .headers
        .get("location")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            Error::decode_message(format!("{system_name}响应缺少 Location"))
                .with_request(response.context)
        })
}

fn password_login_error(error: Error) -> Error {
    let kind = error
        .request()
        .and_then(|context| context.response_body.as_deref())
        .and_then(|body| serde_json::from_str::<Value>(body).ok())
        .filter(|value| value.get("message").and_then(Value::as_str) == Some("Bad credentials"))
        .map_or(AuthenticationErrorKind::Unknown, |_| {
            AuthenticationErrorKind::InvalidCredentials
        });
    error.into_authentication(kind)
}

fn unknown_authentication_error(error: Error) -> Error {
    error.into_authentication(AuthenticationErrorKind::Unknown)
}

#[derive(Serialize)]
struct EncryptedCredentials {
    username: String,
    password: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PasswordLogin<'a> {
    #[serde(flatten)]
    credentials: EncryptedCredentials,
    app_id: &'a str,
    os_type: &'a str,
    geo: &'a str,
    device_id: &'a str,
    client_id: &'a str,
    mfa_state: &'a str,
}

#[derive(Deserialize)]
struct SmsResult {
    #[serde(rename = "result")]
    _result: Value,
}

fn parse_user_info(response: http::JsonResponse) -> Result<PersonalInfo> {
    #[derive(Deserialize)]
    struct User {
        attributes: Attributes,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Attributes {
        user_uid: String,
        user_name: String,
        identity_type_name: String,
        identity_type_id: String,
        organization_name: String,
        organization_id: String,
    }
    let user: User = http::data(response, "code")?;
    Ok(PersonalInfo {
        uid: user.attributes.user_uid,
        name: user.attributes.user_name,
        identity_type_name: user.attributes.identity_type_name,
        identity_type_id: user.attributes.identity_type_id,
        organization_name: user.attributes.organization_name,
        organization_id: user.attributes.organization_id,
    })
}

/// 统一身份认证服务返回的个人基础身份信息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalInfo {
    /// 学号或工号（用户唯一标识）。
    pub uid: String,
    /// 用户姓名。
    pub name: String,
    /// 身份类别名称（如本科生、研究生、教职工）。
    pub identity_type_name: String,
    /// 身份类别编码。
    pub identity_type_id: String,
    /// 所属院系或组织机构名称。
    pub organization_name: String,
    /// 所属院系或组织机构编码。
    pub organization_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    #[test]
    fn password_login_classifies_only_confirmed_responses() {
        for (status, body, expected) in [
            (
                401,
                r#"{"message":"Bad credentials"}"#,
                AuthenticationErrorKind::InvalidCredentials,
            ),
            (
                200,
                r#"{"code":123,"message":"new rejection"}"#,
                AuthenticationErrorKind::Unknown,
            ),
            (
                401,
                r#"{"message":"Unauthorized"}"#,
                AuthenticationErrorKind::Unknown,
            ),
            (
                403,
                "<html>blocked</html>",
                AuthenticationErrorKind::Unknown,
            ),
            (503, "unavailable", AuthenticationErrorKind::Unknown),
        ] {
            let context = Arc::new(crate::RequestContext {
                method: "POST".into(),
                url: format!("{CAS_URL}/token/password/passwordLogin"),
                status_code: Some(status),
                error_code: None,
                response_body: Some(body.into()),
            });
            let error = password_login_error(Error::http("rejected".into()).with_request(context));
            let Error::Authentication(error) = error else {
                panic!("expected authentication error")
            };
            assert_eq!(error.kind(), expected);
            assert_eq!(error.request().unwrap().status_code, Some(status));
            assert_eq!(
                error.request().unwrap().response_body.as_deref(),
                Some(body)
            );
        }
        let source = serde_json::from_str::<Value>("invalid JSON").unwrap_err();
        assert!(password_login_error(Error::from(source)).is_decode());
    }

    #[tokio::test]
    async fn token_reuse_and_logout_are_shared_across_clients() {
        let cas = CasClient::new("", "", "test-device").unwrap();
        let other = cas.clone();
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
        assert!(other.user_token().await.unwrap_err().is_not_authenticated());
        cas.login().await.unwrap();
        assert_eq!(other.user_token().await.unwrap(), token);
        other.logout().await;
        assert!(!cas.is_logged_in().await);
        assert!(cas.user_token().await.unwrap_err().is_not_authenticated());
        assert!(
            cas.email_login_url()
                .await
                .unwrap_err()
                .is_not_authenticated()
        );
        assert!(cas.user_info().await.unwrap_err().is_not_authenticated());
    }

    fn authx_user_response(value: Value) -> http::JsonResponse {
        http::JsonResponse::new(
            value,
            Arc::new(crate::RequestContext {
                method: "GET".into(),
                url: INFO_URL.into(),
                status_code: Some(200),
                error_code: None,
                response_body: None,
            }),
        )
    }

    #[test]
    fn parse_user_info_extracts_authx_attributes_and_serializes_camel_case() {
        let response = authx_user_response(json!({
            "code": 0,
            "data": {
                "attributes": {
                    "userUid": "202400000001",
                    "userName": "张三",
                    "identityTypeName": "本科生",
                    "identityTypeId": "1",
                    "organizationName": "计算机与人工智能学院",
                    "organizationId": "1001"
                }
            }
        }));
        let info = parse_user_info(response).unwrap();
        assert_eq!(
            info,
            PersonalInfo {
                uid: "202400000001".into(),
                name: "张三".into(),
                identity_type_name: "本科生".into(),
                identity_type_id: "1".into(),
                organization_name: "计算机与人工智能学院".into(),
                organization_id: "1001".into(),
            }
        );
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            json!({
                "uid": "202400000001",
                "name": "张三",
                "identityTypeName": "本科生",
                "identityTypeId": "1",
                "organizationName": "计算机与人工智能学院",
                "organizationId": "1001"
            })
        );
    }

    #[test]
    fn parse_user_info_surfaces_remote_and_decode_errors_with_context() {
        let remote_failure = authx_user_response(json!({
            "code": 401,
            "message": "登录已失效"
        }));
        let error = parse_user_info(remote_failure).unwrap_err();
        assert!(error.is_remote());
        assert_eq!(error.message(), "登录已失效");
        assert!(error.request().is_some());

        let missing_attributes = authx_user_response(json!({
            "code": 0,
            "data": {}
        }));
        let error = parse_user_info(missing_attributes).unwrap_err();
        assert!(error.is_decode());
        assert!(error.request().is_some());
    }
}
