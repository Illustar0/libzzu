//! 校园网 Portal 自动探测与认证、本机出口 IP 探测及校园网自助服务设备管理。

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    net::{IpAddr, SocketAddr, UdpSocket},
    sync::{Arc, LazyLock},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use encoding_rs::GBK;
use jiff::{Timestamp, Zoned, civil::DateTime};
use rand::Rng;
use regex::Regex;
use reqwest::{
    Client, Url,
    cookie::Jar,
    dns::{Addrs, Name, Resolve, Resolving},
    redirect::Policy,
};
use scraper::{Html, Selector};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{Error, Result, http, http::TrustRoots};

/// 自动探测得到的校园网 Portal 地址与客户端 IP 信息。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortalInfo {
    /// Portal 认证页面根地址（用于静态脚本与门户资源）。
    pub auth_url: Url,
    /// Portal `eportal` 登录接口服务地址（含探测到的协议与端口，如 801 或 802）。
    pub portal_server_url: Url,
    /// 门户重定向参数中标识的终端 IP 地址（终端位于 NAT 路由器后时为路由器 WAN 口地址）。
    pub user_ip: IpAddr,
}

/// 发起明文 HTTP 探测，自动发现当前网络下的校园网 Portal 参数。
///
/// 探测请求只连接 IPv4 地址，避免经其他网口的 IPv6 路由发出而探测到错误的终端地址。
///
/// # Errors
///
/// - 探测请求已重定向至 `https://www.bilibili.com`（即当前网络已连通外网、未被门户拦截）时，返回 [`Error::Remote`]。
/// - 网络传输失败、HTML 跳转链接缺失，或重定向 URL 缺少合法的客户端 IP 参数时返回错误。
pub async fn discover_portal_info() -> Result<PortalInfo> {
    discover_portal_info_with_trust_roots(&TrustRoots::default()).await
}

/// 探测校园网 Portal 参数，校验证书时信任 `roots`，其余同 [`discover_portal_info`]。
///
/// # Errors
///
/// 同 [`discover_portal_info`]。
pub async fn discover_portal_info_with_trust_roots(roots: &TrustRoots) -> Result<PortalInfo> {
    let client = roots
        .tls_client_builder()
        .dns_resolver(Ipv4OnlyResolver)
        .redirect(Policy::limited(10))
        .build()?;
    let response = http::send(client.get("http://www.bilibili.com"))
        .await?
        .require_success()?;
    let (pre_auth_url, user_ip) = parse_discovered_portal(&response)?;
    let js_url = pre_auth_url
        .join("a41.js")
        .map_err(|error| Error::decode(error).with_request(response.context.clone()))?;
    let script_response = http::send_with_text_decoder(client.get(js_url), async |response| {
        let bytes = response.bytes().await?;
        Ok(decode_portal_text(&bytes))
    })
    .await
    .and_then(http::Response::require_success)
    .ok();
    let script = script_response.as_ref().map(http::Response::body);
    let script_context = script_response
        .as_ref()
        .map_or(&response.context, |script| &script.context);
    let auth_url = auth_url(&pre_auth_url, script)
        .map_err(|error| error.with_request(script_context.clone()))?;
    let portal_server_url = portal_server_url(&auth_url, script)
        .map_err(|error| error.with_request(script_context.clone()))?;
    Ok(PortalInfo {
        auth_url,
        portal_server_url,
        user_ip,
    })
}

fn parse_discovered_portal(response: &http::Response) -> Result<(Url, IpAddr)> {
    if response.url.scheme() == "https" && response.url.host_str() == Some("www.bilibili.com") {
        return Err(Error::remote("未发现校园网认证重定向，可能已经联网".into()));
    }
    let portal_url = if response.url.host_str() == Some("www.bilibili.com") {
        let base = response.url.clone();
        let href = html_attribute(response.body(), "a[href]", "href")
            .map_err(|error| error.with_request(response.context.clone()))?;
        base.join(&href)
            .map_err(|error| Error::decode(error).with_request(response.context.clone()))?
    } else {
        response.url.clone()
    };

    let user_ip = portal_url
        .query_pairs()
        .find(|(key, _)| key == "userip" || key == "wlanuserip")
        .ok_or_else(|| {
            Error::decode_message("Portal URL 缺少客户端 IP".into())
                .with_request(response.context.clone())
        })?
        .1
        .parse()
        .map_err(|_| {
            Error::decode_message("Portal 客户端 IP 无效".into())
                .with_request(response.context.clone())
        })?;
    let pre_auth_url = Url::parse(&portal_url.origin().ascii_serialization())
        .map_err(|error| Error::decode(error).with_request(response.context.clone()))?;
    Ok((pre_auth_url, user_ip))
}

/// 只解析出 IPv4 地址的 DNS 解析器。
///
/// `eportal` 按 IPv4 地址识别终端。若允许 happy eyeballs 优先连接 IPv6，请求可能经持有 IPv6 默认路由的
/// 其他网口发出，门户看到的就不是待认证的校园网地址。
struct Ipv4OnlyResolver;

impl Resolve for Ipv4OnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(SocketAddr::is_ipv4)
                .collect();
            if addresses.is_empty() {
                return Err(format!("{host} 没有可用的 IPv4 地址").into());
            }
            let addresses: Addrs = Box::new(addresses.into_iter());
            Ok(addresses)
        })
    }
}

fn decode_portal_text(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    GBK.decode(bytes).0.into_owned()
}

fn auth_url(pre_auth_url: &Url, script: Option<&str>) -> Result<Url> {
    static DOMAIN_NAME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"\bvar\s+domainName\s*=\s*(?:"([^"]+)"|'([^']+)')\s*;"#)
            .expect("static JavaScript pattern")
    });
    let mut url = pre_auth_url.clone();
    if let Some(domain) = script
        .and_then(|script| DOMAIN_NAME.captures(script))
        .and_then(|captures| captures.get(1).or_else(|| captures.get(2)))
    {
        url.set_host(Some(domain.as_str())).map_err(Error::decode)?;
    }
    Ok(url)
}

fn portal_server_url(auth_url: &Url, script: Option<&str>) -> Result<Url> {
    static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"var\s+(\w+)\s*=\s*(\d+)\s*;").expect("static JavaScript pattern")
    });
    let parameters: BTreeMap<_, u16> = script
        .map(|script| {
            ASSIGNMENT
                .captures_iter(script)
                .filter_map(|captures| Some((captures[1].to_owned(), captures[2].parse().ok()?)))
                .collect()
        })
        .unwrap_or_default();
    let use_https = script.is_some() && parameters.get("enableHttps") != Some(&0);
    let (scheme, port) = if use_https {
        (
            "https",
            parameters
                .get("enHTTPSPort")
                .or_else(|| parameters.get("epHTTPSPort"))
                .copied()
                .unwrap_or(802),
        )
    } else {
        ("http", parameters.get("epHTTPPort").copied().unwrap_or(801))
    };
    let mut url = auth_url.clone();
    url.set_scheme(scheme)
        .map_err(|()| Error::decode_message("Portal scheme 无效".into()))?;
    url.set_port(Some(port))
        .map_err(|()| Error::decode_message("Portal port 无效".into()))?;
    Ok(url)
}

/// 校园网 Portal 提供的运营商选项信息。
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IspInfo {
    /// 运营商显示名称（如“校园网”“中国移动”）。
    pub name: String,
    /// 附加在登录账号后的运营商后缀（如 `"@cmcc"`，普通校园网通常为空字符串 `""`）。
    pub suffix: String,
}

/// Portal 认证与下线请求参数的编码方式。
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortalEncryption {
    /// 明文参数传输（登录密码做标准 Base64 编码）。
    #[default]
    Plain,
    /// 使用客户端 IP 字符异或密钥对查询参数逐字符编码（附带 `encrypt=1` 标记）。
    Xor,
}

/// Portal 认证或下线接口返回的 JSONP 结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AuthResult {
    /// 服务端结果码（`1` 表示操作成功）。
    pub result: i64,
    /// 服务端返回的状态说明消息。
    #[serde(rename = "msg")]
    pub message: String,
    /// 操作失败或特殊状态时附带的可选细分返回码。
    #[serde(rename = "retCode", alias = "ret_code", default)]
    pub ret_code: Option<i64>,
}

impl AuthResult {
    /// 判断本次 Portal 认证或下线请求是否成功（即 `result == 1`）。
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.result == 1
    }
}

/// Portal 登录请求的本地源地址选择方式。
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortalSourceAddress {
    /// 将 `user_ip` 绑定为套接字源地址（终端直接接入校园网，`user_ip` 是本机网卡地址）。
    #[default]
    UserIp,
    /// 由系统路由选择源地址（终端位于 NAT 路由器后，`user_ip` 是路由器 WAN 口地址、不属于本机）。
    System,
}

/// 校园网 `eportal` 认证客户端。
pub struct PortalClient {
    http: Client,
    base_url: Url,
    user_ip: IpAddr,
}

impl PortalClient {
    /// 创建 Portal 认证客户端，按 `source_address` 决定是否把 `user_ip` 绑定为本地源地址。
    ///
    /// `base_url` 通常取自 [`PortalInfo::portal_server_url`]；`user_ip` 通常取自 [`PortalInfo::user_ip`]，
    /// 登录时作为终端地址发送给门户。本函数仅校验 `base_url`，不立即绑定本地套接字；选择
    /// [`PortalSourceAddress::UserIp`] 而 `user_ip` 非本机可用网卡地址时，调用 [`Self::login`] 建立连接将返回
    /// [`Error::Transport`]。登录请求只连接 IPv4 地址。
    ///
    /// # Errors
    ///
    /// `base_url` 不是含主机名的有效 `http`/`https` URL，或底层 HTTP 客户端初始化失败时返回错误。
    pub fn new(
        base_url: &str,
        user_ip: IpAddr,
        source_address: PortalSourceAddress,
    ) -> Result<Self> {
        Self::with_trust_roots(base_url, user_ip, source_address, &TrustRoots::default())
    }

    /// 创建校验证书时信任 `roots` 的 Portal 认证客户端，其余同 [`Self::new`]。
    ///
    /// # Errors
    ///
    /// `base_url` 不是含主机名的有效 `http`/`https` URL，或底层 HTTP 客户端初始化失败时返回错误。
    pub fn with_trust_roots(
        base_url: &str,
        user_ip: IpAddr,
        source_address: PortalSourceAddress,
        roots: &TrustRoots,
    ) -> Result<Self> {
        let local_address = match source_address {
            PortalSourceAddress::UserIp => Some(user_ip),
            PortalSourceAddress::System => None,
        };
        let client = roots
            .tls_client_builder()
            .dns_resolver(Ipv4OnlyResolver)
            .local_address(local_address)
            .redirect(Policy::none())
            .build()?;
        Self::with_http_client(base_url, user_ip, client)
    }

    /// 使用自定义 [`reqwest::Client`] 创建 Portal 认证客户端。
    ///
    /// # Errors
    ///
    /// `base_url` 不是含主机名的有效 `http`/`https` URL 时返回 [`Error::InvalidInput`]。
    pub fn with_http_client(base_url: &str, user_ip: IpAddr, client: Client) -> Result<Self> {
        Ok(Self {
            http: client,
            base_url: parse_base_url(base_url)?,
            user_ip,
        })
    }

    /// 向校园网 Portal 服务器发起终端上线认证请求。
    ///
    /// 融合宽带用户需在 `account` 后自行附带对应的运营商后缀（如中国移动 `@cmcc`、中国联通 `@unicom`、中国电信 `@telecom`，可通过 [`Self::isps`] 查询）；普通校园网账号直接传入学号或工号即可。
    ///
    /// # Errors
    ///
    /// - 选择 [`PortalEncryption::Xor`] 且待编码字段超过 512 个字符时返回 [`Error::InvalidInput`]。
    /// - 选择 [`PortalSourceAddress::UserIp`] 而 `user_ip` 无法绑定、网络传输失败、HTTP 状态非成功或返回的 JSONP 报文无法解析时返回错误。
    pub async fn login(
        &self,
        account: &str,
        password: &str,
        encryption: PortalEncryption,
    ) -> Result<AuthResult> {
        let user_ip = self.user_ip.to_string();
        let encode = |text: &str| encode_portal_field(&user_ip, encryption, text);
        let query = PortalLogin {
            callback: encode("dr1003")?,
            login_method: encode("1")?,
            user_account: encode(&format!(",0,{account}"))?,
            user_password: encode(&STANDARD.encode(password))?,
            wlan_user_ip: encode(&user_ip)?,
            wlan_user_ipv6: "",
            wlan_user_mac: encode("000000000000")?,
            wlan_vlan_id: encode("0")?,
            wlan_ac_ip: "",
            wlan_ac_name: "",
            authex_enable: "",
            js_version: encode("4.2.2")?,
            terminal_type: encode("3")?,
            lang: encode("zh-cn")?,
            encrypt: matches!(encryption, PortalEncryption::Xor).then_some("1"),
            v: rand::thread_rng().gen_range(500..=10499),
        };
        self.send_portal_jsonp("/eportal/portal/login", &query)
            .await
    }

    /// 向校园网 Portal 服务器发起当前终端（`user_ip`）下线注销请求。
    ///
    /// 按 `eportal` 协议直接以配置在客户端上的 `user_ip` 通知接入控制器（`ac_logout = 1`）释放会话，
    /// 无需再次提供校园网账号密码，也无需登录自助服务后台。
    ///
    /// # Errors
    ///
    /// 选择 [`PortalSourceAddress::UserIp`] 而 `user_ip` 无法绑定、网络传输失败、HTTP 状态非成功或返回的 JSONP 报文无法解析时返回错误。
    pub async fn logout(&self, encryption: PortalEncryption) -> Result<AuthResult> {
        let user_ip = self.user_ip.to_string();
        let encode = |text: &str| encode_portal_field(&user_ip, encryption, text);
        let query = PortalLogout {
            callback: encode("dr1004")?,
            login_method: encode("1")?,
            user_account: encode("drcom")?,
            user_password: encode("123")?,
            ac_logout: encode("1")?,
            register_mode: encode("1")?,
            wlan_user_ip: encode(&user_ip)?,
            wlan_user_ipv6: "",
            wlan_vlan_id: encode("0")?,
            wlan_user_mac: encode("000000000000")?,
            wlan_ac_ip: "",
            wlan_ac_name: "",
            js_version: encode("4.2.2")?,
            lang: encode("zh-cn")?,
            encrypt: matches!(encryption, PortalEncryption::Xor).then_some("1"),
            v: rand::thread_rng().gen_range(500..=10499),
        };
        self.send_portal_jsonp("/eportal/portal/logout", &query)
            .await
    }

    async fn send_portal_jsonp<Q: Serialize>(&self, path: &str, query: &Q) -> Result<AuthResult> {
        let url = self
            .base_url
            .join(path)
            .map_err(|error| Error::invalid_input(error.to_string()))?;
        let request = self.http.get(url).query(query).query(&[("lang", "zh")]);
        let response = http::send(request).await?.require_success()?;
        parse_jsonp(response.body()).map_err(|error| error.with_request(response.context))
    }

    /// 查询当前校园网 Portal 页面提供的运营商列表及其登录账号后缀。
    ///
    /// 先请求 `loadConfig` 获取门户页面配置，再拉取对应 `hipad.js`（兼容 UTF-8 与 GBK 编码）并解析其中的运营商下拉选项。
    /// 若门户页面模板不含 `bodyContent` 或运营商选择列表，返回空列表。
    ///
    /// # Errors
    ///
    /// 选择 [`PortalSourceAddress::UserIp`] 而 `user_ip` 无法绑定、网络传输失败、HTTP 状态非成功，
    /// 或 `loadConfig` 返回的 JSONP 报文及页面配置字段无法解析时返回错误。
    pub async fn isps(&self) -> Result<Vec<IspInfo>> {
        let query = LoadConfigQuery {
            callback: "dr1001",
            wlan_user_ip: STANDARD.encode(self.user_ip.to_string()),
            wlan_user_ipv6: "",
            wlan_user_mac: "000000000000",
            wlan_vlan_id: "0",
            wlan_ac_ip: "",
            wlan_ac_name: "",
            js_version: "4.X",
            terminal_type: "3",
            lang: "zh",
            v: rand::thread_rng().gen_range(500..=10499),
        };
        let config_url = self
            .base_url
            .join("/eportal/portal/page/loadConfig")
            .map_err(|error| Error::invalid_input(error.to_string()))?;
        let response = http::send(self.http.get(config_url).query(&query))
            .await?
            .require_success()?;
        let config: LoadConfigResponse = parse_jsonp(response.body())
            .map_err(|error| error.with_request(response.context.clone()))?;
        if !is_valid_path_segment(&config.data.program_index)
            || !is_valid_path_segment(&config.data.page_index)
        {
            return Err(
                Error::decode_message("Portal 页面配置缺少有效的路径标识".into())
                    .with_request(response.context),
            );
        }

        let mut hipad_js_url = self
            .base_url
            .join("/eportal/extern")
            .map_err(|error| Error::invalid_input(error.to_string()))?;
        hipad_js_url
            .path_segments_mut()
            .map_err(|()| Error::decode_message("Portal 脚本地址无效".into()))?
            .push(&config.data.program_index)
            .push(&config.data.page_index)
            .push("hipad.js");
        let response =
            http::send_with_text_decoder(self.http.get(hipad_js_url), async |response| {
                let bytes = response.bytes().await?;
                Ok(decode_portal_text(&bytes))
            })
            .await?
            .require_success()?;
        Ok(parse_isp_info(response.body()))
    }
}

#[derive(Serialize)]
struct PortalLogin {
    callback: String,
    login_method: String,
    user_account: String,
    user_password: String,
    wlan_user_ip: String,
    wlan_user_ipv6: &'static str,
    wlan_user_mac: String,
    wlan_vlan_id: String,
    wlan_ac_ip: &'static str,
    wlan_ac_name: &'static str,
    authex_enable: &'static str,
    #[serde(rename = "jsVersion")]
    js_version: String,
    terminal_type: String,
    lang: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    encrypt: Option<&'static str>,
    v: u32,
}

#[derive(Serialize)]
struct PortalLogout {
    callback: String,
    login_method: String,
    user_account: String,
    user_password: String,
    ac_logout: String,
    register_mode: String,
    wlan_user_ip: String,
    wlan_user_ipv6: &'static str,
    wlan_vlan_id: String,
    wlan_user_mac: String,
    wlan_ac_ip: &'static str,
    wlan_ac_name: &'static str,
    #[serde(rename = "jsVersion")]
    js_version: String,
    lang: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    encrypt: Option<&'static str>,
    v: u32,
}

fn encode_portal_field(user_ip: &str, encryption: PortalEncryption, text: &str) -> Result<String> {
    match encryption {
        PortalEncryption::Plain => Ok(text.into()),
        PortalEncryption::Xor => {
            if text.chars().count() > 512 {
                return Err(Error::invalid_input(
                    "Portal 加密字段不能超过 512 个字符".into(),
                ));
            }
            let xor_key = user_ip
                .chars()
                .fold(0, |key, character| key ^ u32::from(character));
            let mut encoded = String::new();
            for character in text.chars() {
                let _ = write!(encoded, "{:02x}", u32::from(character) ^ xor_key);
            }
            Ok(encoded)
        }
    }
}

#[derive(Serialize)]
struct LoadConfigQuery {
    callback: &'static str,
    wlan_user_ip: String,
    wlan_user_ipv6: &'static str,
    wlan_user_mac: &'static str,
    wlan_vlan_id: &'static str,
    wlan_ac_ip: &'static str,
    wlan_ac_name: &'static str,
    #[serde(rename = "jsVersion")]
    js_version: &'static str,
    terminal_type: &'static str,
    lang: &'static str,
    v: u32,
}

#[derive(Deserialize)]
struct LoadConfigResponse {
    data: LoadConfigData,
}

#[derive(Deserialize)]
struct LoadConfigData {
    program_index: String,
    page_index: String,
}

fn is_valid_path_segment(segment: &str) -> bool {
    !segment.is_empty() && !matches!(segment, "." | "..")
}

fn parse_isp_info(hipad_js_content: &str) -> Vec<IspInfo> {
    static BODY_CONTENT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)var\s+bodyContent\s*=\s*'(.*?)'\s*;").expect("static JavaScript pattern")
    });
    static ISP_OPTION: LazyLock<Selector> = LazyLock::new(|| {
        Selector::parse(r#"select[name="ISP_select"] option"#).expect("static HTML selector")
    });

    let Some(body_content) = BODY_CONTENT
        .captures(hipad_js_content)
        .and_then(|captures| captures.get(1))
    else {
        return Vec::new();
    };

    let document = Html::parse_document(body_content.as_str());
    document
        .select(&ISP_OPTION)
        .filter_map(|element| {
            let suffix = element.value().attr("value")?;
            if suffix == "-1" {
                return None;
            }

            let name = element.text().collect::<String>().trim().to_owned();
            if name.is_empty() {
                return None;
            }

            Some(IspInfo {
                name,
                suffix: suffix.to_owned(),
            })
        })
        .collect()
}

fn parse_jsonp<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    let text = text.trim().strip_suffix(';').unwrap_or(text.trim()).trim();
    let (callback, body) = text
        .split_once('(')
        .ok_or_else(|| Error::decode_message("JSONP 缺少回调".into()))?;
    if callback.is_empty()
        || !callback
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
    {
        return Err(Error::decode_message("JSONP 回调名无效".into()));
    }
    let json = body
        .strip_suffix(')')
        .ok_or_else(|| Error::decode_message("JSONP 缺少闭合括号".into()))?;
    Ok(serde_json::from_str(json)?)
}

/// 通过 UDP 路由表查询本机前往 `target` 时选用的本地网卡 IP 地址（不实际发送数据包）。
///
/// # Errors
///
/// 绑定本地 UDP 套接字、查询前往 `target` 的路由或读取本地套接字地址失败时返回 [`std::io::Error`]。
pub fn local_ip(target: IpAddr) -> std::io::Result<IpAddr> {
    let bind = match target {
        IpAddr::V4(_) => "0.0.0.0:0",
        IpAddr::V6(_) => "[::]:0",
    };
    let socket = UdpSocket::bind(bind)?;
    socket.connect((target, 80))?;
    Ok(socket.local_addr()?.ip())
}

/// 校园网自助服务系统返回的当前在线终端信息。
///
/// 反序列化时将服务端返回的 `YYYY-MM-DD HH:MM:SS` 登录时间转为 `Asia/Shanghai` 时区的 [`Zoned`]
/// （同时兼容 RFC 9557 字符串以支持无损重序列化），并将在线时长统一解析为秒数。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnlineDevice {
    /// 宽带远程接入服务器（BRAS）标识。
    pub brasid: String,
    /// 已使用下行流量文本。
    pub down_flow: String,
    /// 终端主机名（未上报时为空字符串）。
    #[serde(default)]
    pub host_name: String,
    /// 终端分配的内网 IP 地址文本。
    pub ip: String,
    /// 终端登录时间（`Asia/Shanghai` 时区）。
    #[serde(deserialize_with = "deserialize_login_time")]
    pub login_time: Zoned,
    /// 终端 MAC 地址文本。
    pub mac: String,
    /// 在线会话唯一标识（可用于 [`SelfServiceClient::kick_device`] 强制下线）。
    pub session_id: String,
    /// 终端设备类型描述。
    pub terminal_type: String,
    /// 已使用上行流量文本。
    pub up_flow: String,
    /// 在线时长（单位：秒）。
    #[serde(deserialize_with = "deserialize_use_time")]
    pub use_time: u64,
    /// 校园网账户数字用户 ID。
    pub user_id: i64,
}

fn deserialize_login_time<'de, D>(deserializer: D) -> std::result::Result<Zoned, D::Error>
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

fn deserialize_use_time<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawSeconds {
        Integer(u64),
        Text(String),
    }
    match RawSeconds::deserialize(deserializer)? {
        RawSeconds::Integer(seconds) => Ok(seconds),
        RawSeconds::Text(text) => text
            .trim()
            .parse::<u64>()
            .map_err(|_| serde::de::Error::custom("在线时长 useTime 必须为非负整数秒")),
    }
}

/// 校园网自助服务系统（`/Self`）客户端，用于查询与强制下线在线设备。
pub struct SelfServiceClient {
    http: Client,
    login_http: Client,
    base_url: Url,
    is_logged_in: bool,
}

impl SelfServiceClient {
    /// 使用自助服务根地址创建客户端（内部维护独立的会话 Cookie 罐）。
    ///
    /// # Errors
    ///
    /// `base_url` 不是有效的 `http`/`https` URL 或 HTTP 客户端构建失败时返回错误。
    pub fn new(base_url: &str) -> Result<Self> {
        Self::with_trust_roots(base_url, &TrustRoots::default())
    }

    /// 使用自助服务根地址创建校验证书时信任 `roots` 的客户端。
    ///
    /// # Errors
    ///
    /// `base_url` 不是有效的 `http`/`https` URL 或 HTTP 客户端构建失败时返回错误。
    pub fn with_trust_roots(base_url: &str, roots: &TrustRoots) -> Result<Self> {
        let cookies = Arc::new(Jar::default());
        let http = roots
            .tls_client_builder()
            .cookie_provider(cookies.clone())
            .redirect(Policy::limited(10))
            .build()?;
        let login_http = roots
            .tls_client_builder()
            .cookie_provider(cookies)
            .redirect(Policy::none())
            .build()?;
        Ok(Self {
            http,
            login_http,
            base_url: parse_base_url(base_url)?,
            is_logged_in: false,
        })
    }

    fn url(&self, path: &str) -> Result<Url> {
        self.base_url
            .join(path)
            .map_err(|error| Error::invalid_input(error.to_string()))
    }

    /// 判断当前自助服务客户端是否已完成登录。
    #[must_use]
    pub fn is_logged_in(&self) -> bool {
        self.is_logged_in
    }

    /// 登录校园网自助服务系统。
    ///
    /// # Errors
    ///
    /// 登录页缺少 `checkcode` 表单字段、网络异常，或提交后未跳转至控制台页面（如账号密码错误）时返回错误。
    pub async fn login(&mut self, account: &str, password: &str) -> Result<()> {
        #[derive(Serialize)]
        struct Login<'a> {
            foo: &'a str,
            bar: &'a str,
            checkcode: &'a str,
            account: &'a str,
            password: &'a str,
            code: &'a str,
        }
        self.is_logged_in = false;
        let request = self.login_http.get(self.url("/Self/login/")?);
        let response = http::send(request).await?.require_success()?;
        let checkcode = html_attribute(response.body(), "input[name=checkcode]", "value")
            .map_err(|error| error.with_request(response.context))?;
        let request = self
            .http
            .get(self.url("/Self/login/randomCode")?)
            .query(&[("t", rand::random::<f64>())]);
        http::send(request).await?.require_success()?;
        let form = Login {
            foo: "",
            bar: "",
            checkcode: &checkcode,
            account,
            password,
            code: "",
        };
        let request = self.http.post(self.url("/Self/login/verify")?).form(&form);
        let response = http::send(request).await?.require_success()?;
        if !response.url.path().contains("dashboard") {
            return Err(Error::authentication(
                crate::AuthenticationErrorKind::Unknown,
                "校园网自助系统登录失败".into(),
            )
            .with_request(response.context));
        }
        self.is_logged_in = true;
        Ok(())
    }

    /// 获取当前账号下的所有在线终端列表。
    ///
    /// # Errors
    ///
    /// - 尚未调用 [`Self::login`] 登录时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    /// - 网络请求失败或设备列表 JSON 解析失败时返回错误。
    pub async fn online_devices(&self) -> Result<Vec<OnlineDevice>> {
        #[derive(Serialize)]
        struct Query {
            t: f64,
            order: &'static str,
            #[serde(rename = "_")]
            timestamp: i64,
        }
        if !self.is_logged_in {
            return Err(Error::not_authenticated());
        }
        let query = Query {
            t: rand::random(),
            order: "asc",
            timestamp: Timestamp::now().as_second(),
        };
        let response = http::json(
            self.http
                .get(self.url("/Self/dashboard/getOnlineList")?)
                .query(&query),
        )
        .await?;
        response.decode()
    }

    /// 根据在线设备的 `session_id` 强制指定终端下线。
    ///
    /// # Errors
    ///
    /// - 尚未调用 [`Self::login`] 登录时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    /// - `session_id` 为空字符串时返回 [`Error::InvalidInput`]。
    /// - 下线请求失败时返回错误。
    pub async fn kick_device(&self, session_id: &str) -> Result<()> {
        #[derive(Serialize)]
        struct Query<'a> {
            t: f64,
            sessionid: &'a str,
        }
        if !self.is_logged_in {
            return Err(Error::not_authenticated());
        }
        if session_id.is_empty() {
            return Err(Error::invalid_input("session_id 不能为空".into()));
        }
        let query = Query {
            t: rand::random(),
            sessionid: session_id,
        };
        let request = self
            .http
            .get(self.url("/Self/dashboard/tooffline")?)
            .query(&query);
        http::send(request).await?.require_success()?;
        Ok(())
    }

    /// 登出校园网自助服务会话并重置本地登录状态。
    ///
    /// # Errors
    ///
    /// - 客户端尚未登录时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    /// - 服务端登出请求失败时返回错误（本地登录标记仍会被清除）。
    pub async fn logout(&mut self) -> Result<()> {
        if !self.is_logged_in {
            return Err(Error::not_authenticated());
        }
        let response = http::send(self.http.get(self.url("/Self/login/logout")?)).await;
        self.is_logged_in = false;
        response?.require_success()?;
        Ok(())
    }
}

fn parse_base_url(base_url: &str) -> Result<Url> {
    let url = Url::parse(base_url).map_err(|error| Error::invalid_input(error.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(Error::invalid_input("服务地址必须为 HTTP(S) URL".into()));
    }
    Ok(url)
}

fn html_attribute(html: &str, selector: &str, attribute: &str) -> Result<String> {
    let selector = Selector::parse(selector)
        .map_err(|_| Error::decode_message("HTML selector 无效".into()))?;
    Html::parse_document(html)
        .select(&selector)
        .next()
        .and_then(|element| element.value().attr(attribute))
        .map(str::to_owned)
        .ok_or_else(|| Error::decode_message(format!("HTML 缺少 {attribute}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_parsers_handle_real_protocol_shapes() {
        let response: AuthResult =
            parse_jsonp(" dr1003(\n{\"result\":1,\"msg\":\"成功 (ok)\"}\n); ").unwrap();
        assert!(response.is_success());
        assert!(parse_jsonp::<AuthResult>("alert(1);dr1003({})").is_err());
        let base = Url::parse("http://10.0.0.1/").unwrap();
        assert_eq!(
            portal_server_url(&base, Some("var enableHttps = 0; var epHTTPPort=8801;"))
                .unwrap()
                .as_str(),
            "http://10.0.0.1:8801/"
        );
        assert_eq!(
            portal_server_url(&base, None).unwrap().as_str(),
            "http://10.0.0.1:801/"
        );
        assert_eq!(
            portal_server_url(
                &base,
                Some("var\u{a0}enableHttps\u{2003}=0; var epHTTPPort=8802;"),
            )
            .unwrap()
            .as_str(),
            "http://10.0.0.1:8802/"
        );
        assert_eq!(
            html_attribute(
                "<input name='checkcode' value='a&amp;b'>",
                "input[name=checkcode]",
                "value"
            )
            .unwrap(),
            "a&b"
        );

        let online_probe = http::Response {
            headers: reqwest::header::HeaderMap::new(),
            url: "https://www.bilibili.com/".parse().unwrap(),
            context: Arc::new(crate::RequestContext {
                method: "GET".into(),
                url: "http://www.bilibili.com/".into(),
                status_code: Some(200),
                error_code: None,
                response_body: Some("<html>bilibili</html>".into()),
            }),
        };
        let err = parse_discovered_portal(&online_probe).unwrap_err();
        assert!(err.is_remote());
        assert!(err.request().is_none());

        let missing_ip = http::Response {
            headers: reqwest::header::HeaderMap::new(),
            url: "http://10.0.0.1/a79.htm".parse().unwrap(),
            context: online_probe.context,
        };
        let err = parse_discovered_portal(&missing_ip).unwrap_err();
        assert!(err.is_decode());
        assert!(err.request().is_some());
    }

    #[test]
    fn online_device_parses_shanghai_login_time_and_seconds_with_roundtrip() {
        let raw = serde_json::json!({
            "brasid": "1",
            "downFlow": "1024",
            "ip": "10.1.2.3",
            "loginTime": "2026-10-31 08:30:45",
            "mac": "001122334455",
            "sessionId": "sid-1",
            "terminalType": "PC",
            "upFlow": "256",
            "useTime": "3661",
            "userId": 42
        });
        let device: OnlineDevice = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(
            device.login_time.to_string(),
            "2026-10-31T08:30:45+08:00[Asia/Shanghai]"
        );
        assert_eq!(device.use_time, 3661);

        let serialized = serde_json::to_string(&device).unwrap();
        let roundtripped: OnlineDevice = serde_json::from_str(&serialized).unwrap();
        assert_eq!(roundtripped.login_time, device.login_time);
        assert_eq!(roundtripped.use_time, 3661);

        let mut bad_time = raw.clone();
        bad_time["loginTime"] = serde_json::json!("not-a-datetime");
        assert!(serde_json::from_value::<OnlineDevice>(bad_time).is_err());

        let mut bad_duration = raw;
        bad_duration["useTime"] = serde_json::json!("1h2m");
        assert!(serde_json::from_value::<OnlineDevice>(bad_duration).is_err());
    }

    #[test]
    fn parse_isp_info_extracts_options_and_ignores_placeholders() {
        let hipad_js = r#"
            var other = 'ignored';
            var bodyContent = '<form>
                <select name="other_select"><option value="@ignored">忽略</option></select>
                <select name="ISP_select">
                    <option value="-1">请选择运营商</option>
                    <option value=""> 校园网 </option>
                    <option value="@cmcc">中国移动</option>
                    <option value="@unicom"> 中国联通 &amp; 宽带 </option>
                    <option value="@empty">   </option>
                    <option>缺少属性</option>
                </select>
            </form>';
        "#;
        assert_eq!(
            parse_isp_info(hipad_js),
            vec![
                IspInfo {
                    name: "校园网".into(),
                    suffix: String::new(),
                },
                IspInfo {
                    name: "中国移动".into(),
                    suffix: "@cmcc".into(),
                },
                IspInfo {
                    name: "中国联通 & 宽带".into(),
                    suffix: "@unicom".into(),
                },
            ]
        );
        assert_eq!(parse_isp_info("var other = 'no body';"), Vec::new());
        assert_eq!(
            parse_isp_info("var bodyContent = '<div>no select</div>';"),
            Vec::new()
        );
    }

    fn spawn_stub_http_server(
        responses: Vec<(&'static str, Vec<u8>)>,
    ) -> (SocketAddr, std::thread::JoinHandle<Vec<String>>) {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::with_capacity(responses.len());
            for (content_type, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0_u8; 4096];
                let mut read = 0;
                while read < buffer.len() {
                    let count = stream.read(&mut buffer[read..]).unwrap();
                    if count == 0 {
                        break;
                    }
                    read += count;
                    if buffer[..read]
                        .windows(4)
                        .any(|window| window == b"\r\n\r\n")
                    {
                        break;
                    }
                }
                let request_line = std::str::from_utf8(&buffer[..read])
                    .unwrap()
                    .lines()
                    .next()
                    .unwrap()
                    .to_owned();
                requests.push(request_line);

                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(header.as_bytes()).unwrap();
                stream.write_all(&body).unwrap();
            }
            requests
        });
        (addr, handle)
    }

    #[tokio::test]
    async fn portal_client_isps_loads_config_and_decodes_gbk_hipad_js() {
        let gbk_script = GBK
            .encode(
                r#"var bodyContent = '<select name="ISP_select"><option value="-1">请选择</option><option value="">校园网</option><option value="@cmcc">中国移动</option></select>';"#,
            )
            .0
            .into_owned();
        let (addr, server) = spawn_stub_http_server(vec![
            (
                "application/javascript; charset=utf-8",
                br#"dr1001({"data":{"program_index":"prog_1","page_index":"page_2"}});"#.to_vec(),
            ),
            ("application/javascript", gbk_script),
        ]);

        let client = PortalClient::with_http_client(
            &format!("http://{addr}"),
            "10.171.51.216".parse().unwrap(),
            Client::new(),
        )
        .unwrap();
        let isps = client.isps().await.unwrap();
        assert_eq!(
            isps,
            vec![
                IspInfo {
                    name: "校园网".into(),
                    suffix: String::new(),
                },
                IspInfo {
                    name: "中国移动".into(),
                    suffix: "@cmcc".into(),
                },
            ]
        );

        let requests = server.join().unwrap();
        let config_target = requests[0].split_whitespace().nth(1).unwrap();
        let config_url = Url::parse(&format!("http://stub{config_target}")).unwrap();
        assert_eq!(config_url.path(), "/eportal/portal/page/loadConfig");
        let query: BTreeMap<_, _> = config_url.query_pairs().into_owned().collect();
        assert_eq!(query.get("callback").map(String::as_str), Some("dr1001"));
        assert_eq!(
            query.get("wlan_user_ip").map(String::as_str),
            Some("MTAuMTcxLjUxLjIxNg==")
        );
        assert_eq!(query.get("jsVersion").map(String::as_str), Some("4.X"));
        assert_eq!(query.get("terminal_type").map(String::as_str), Some("3"));
        assert_eq!(query.get("lang").map(String::as_str), Some("zh"));
        assert_eq!(
            requests[1].split_whitespace().nth(1),
            Some("/eportal/extern/prog_1/page_2/hipad.js")
        );
    }

    #[tokio::test]
    async fn portal_client_isps_rejects_invalid_config_path_segments() {
        let (addr, server) = spawn_stub_http_server(vec![(
            "application/javascript; charset=utf-8",
            br#"dr1001({"data":{"program_index":"..","page_index":"page_2"}});"#.to_vec(),
        )]);

        let client = PortalClient::with_http_client(
            &format!("http://{addr}"),
            "10.171.51.216".parse().unwrap(),
            Client::new(),
        )
        .unwrap();
        let error = client.isps().await.unwrap_err();
        assert!(error.is_decode());
        assert!(error.request().is_some());
        server.join().unwrap();
    }

    #[tokio::test]
    async fn portal_client_logout_supports_plain_and_xor_queries() {
        let (addr, server) = spawn_stub_http_server(vec![
            (
                "application/javascript; charset=utf-8",
                r#"dr1004({"result":1,"msg":"注销成功！"});"#.as_bytes().to_vec(),
            ),
            (
                "application/javascript; charset=utf-8",
                r#"dr1004({"result":0,"msg":"终端不在线","ret_code":2});"#
                    .as_bytes()
                    .to_vec(),
            ),
        ]);

        let client = PortalClient::with_http_client(
            &format!("http://{addr}"),
            "10.171.51.216".parse().unwrap(),
            Client::new(),
        )
        .unwrap();

        let plain = client.logout(PortalEncryption::Plain).await.unwrap();
        assert!(plain.is_success());
        assert_eq!(plain.message, "注销成功！");

        let xor = client.logout(PortalEncryption::Xor).await.unwrap();
        assert!(!xor.is_success());
        assert_eq!(xor.ret_code, Some(2));

        let requests = server.join().unwrap();
        let plain_url = Url::parse(&format!(
            "http://stub{}",
            requests[0].split_whitespace().nth(1).unwrap()
        ))
        .unwrap();
        assert_eq!(plain_url.path(), "/eportal/portal/logout");
        let plain_query: BTreeMap<_, _> = plain_url.query_pairs().into_owned().collect();
        assert_eq!(
            plain_query.get("callback").map(String::as_str),
            Some("dr1004")
        );
        assert_eq!(
            plain_query.get("user_account").map(String::as_str),
            Some("drcom")
        );
        assert_eq!(
            plain_query.get("user_password").map(String::as_str),
            Some("123")
        );
        assert_eq!(plain_query.get("ac_logout").map(String::as_str), Some("1"));
        assert_eq!(
            plain_query.get("register_mode").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            plain_query.get("wlan_user_ip").map(String::as_str),
            Some("10.171.51.216")
        );
        assert!(!plain_query.contains_key("encrypt"));

        let xor_url = Url::parse(&format!(
            "http://stub{}",
            requests[1].split_whitespace().nth(1).unwrap()
        ))
        .unwrap();
        assert_eq!(xor_url.path(), "/eportal/portal/logout");
        let xor_query: BTreeMap<_, _> = xor_url.query_pairs().into_owned().collect();
        assert_eq!(xor_query.get("encrypt").map(String::as_str), Some("1"));
        assert_ne!(
            xor_query.get("wlan_user_ip").map(String::as_str),
            Some("10.171.51.216")
        );
    }
}
