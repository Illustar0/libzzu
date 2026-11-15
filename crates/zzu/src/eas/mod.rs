//! 本科教务系统（EAMS）客户端与学期课表数据结构。

mod model;

pub use model::{
    Course, CourseTableEntry, Lesson, NamedEntity, PeriodInfo, Room, Schedule, Semester,
    TeachingWeek, TeachingWeeks,
};

use jiff::civil::Date;
use reqwest::{Client, RequestBuilder};
use serde::de::DeserializeOwned;

use crate::{Error, Result, auth::CasClient, http};

const EAS_URL: &str = "https://jwxt.zzu.edu.cn";

/// 本科教务系统客户端。
///
/// 共享 [`CasClient`] 的统一身份认证会话；调用 [`Self::login`] 后缓存当前学期信息，解锁学期与课表查询。
pub struct UndergradEasClient {
    http: Client,
    cas: CasClient,
    current_semester: Option<Semester>,
}

impl UndergradEasClient {
    /// 基于共享的 [`CasClient`] 与默认 HTTP 配置创建本科教务客户端。
    ///
    /// # Errors
    ///
    /// 当默认 HTTP 客户端初始化失败时返回错误。
    pub fn new(cas: CasClient) -> Result<Self> {
        Ok(Self::with_http_client(cas, http::client()?))
    }

    /// 基于共享的 [`CasClient`] 与自定义 [`reqwest::Client`] 创建本科教务客户端。
    #[must_use]
    pub fn with_http_client(cas: CasClient, client: Client) -> Self {
        Self {
            http: client,
            cas,
            current_semester: None,
        }
    }

    async fn request(&self, path: &str) -> Result<RequestBuilder> {
        let token = self.cas.user_token().await?;
        Ok(self
            .http
            .get(format!("{EAS_URL}{path}"))
            .header("Authorization", &token)
            .header("Cookie", format!("userToken={token}")))
    }

    /// 验证教务门户会话并拉取、缓存当前学期元数据。
    ///
    /// # Errors
    ///
    /// - 当底层 [`CasClient`] 尚未登录时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    /// - 当教务门户用户信息或当前学期接口请求失败、业务状态非零或响应解析失败时返回相应错误。
    pub async fn login(&mut self) -> Result<()> {
        self.current_semester = None;
        let response = http::json(
            self.request("/eams-door/api/v1/portal/home/user-info")
                .await?,
        )
        .await?;
        http::check_status(&response, "result")?;
        let semester = self.fetch_current_semester().await?;
        self.current_semester = Some(semester);
        Ok(())
    }

    /// 判断当前教务客户端是否已登录并缓存了当前学期信息。
    #[must_use]
    pub fn is_logged_in(&self) -> bool {
        self.current_semester.is_some()
    }

    /// 返回登录时缓存的当前学期信息引用。
    ///
    /// # Errors
    ///
    /// 当尚未调用 [`Self::login`] 完成教务登录时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    pub fn current_semester(&self) -> Result<&Semester> {
        self.current_semester
            .as_ref()
            .ok_or(Error::not_authenticated())
    }

    async fn fetch_current_semester(&self) -> Result<Semester> {
        let response = http::json(
            self.request("/eams-micro-server/api/v1/semester/current-semester")
                .await?,
        )
        .await?;
        http::data(response, "result")
    }

    async fn fetch<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.current_semester()?;
        let response = http::json(self.request(path).await?).await?;
        http::data(response, "result")
    }

    /// 查询教务系统中的全部学期列表。
    ///
    /// # Errors
    ///
    /// - 当教务客户端尚未登录时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    /// - 当网络传输、远端状态校验或学期列表解码失败时返回相应错误。
    pub async fn semesters(&self) -> Result<Vec<Semester>> {
        self.fetch("/eams-door/api/v1/calendar/get-all-semesters")
            .await
    }

    /// 查询指定学期（传入 `None` 时默认使用 [`Self::current_semester`]）的完整教学周课表。
    ///
    /// 当前学期直接使用登录时缓存的周次，其他学期才需要查询学期列表。
    ///
    /// # Errors
    ///
    /// - 当教务客户端尚未登录时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    /// - 当指定的 `semester_id` 在学期列表中不存在时返回 [`Error::NotFound`]。
    /// - 当网络请求或课表时间、节次解码校验失败时返回相应错误。
    pub async fn teaching_weeks(&self, semester_id: Option<i64>) -> Result<TeachingWeeks> {
        let current = self.current_semester()?;
        let semester_id = semester_id.unwrap_or(current.id);
        let week_indices = if semester_id == current.id {
            current.week_indices.clone()
        } else {
            self.semesters()
                .await?
                .into_iter()
                .find(|semester| semester.id == semester_id)
                .ok_or_else(|| Error::not_found(format!("semester_id={semester_id}")))?
                .week_indices
        };
        let request = self
            .request(&format!(
                "/eams-micro-server/api/v1/lesson/student/course-table/{semester_id}"
            ))
            .await?
            .query(&[("hasExperiment", true)]);
        let response = http::json(request).await?;
        let courses: Vec<CourseTableEntry> = http::data(response, "result")?;
        TeachingWeeks::from_courses(&week_indices, courses)
    }

    /// 按教学周序号 [`TeachingWeek::week_index`] 查询单周课表（`semester_id` 为 `None` 时默认使用当前学期）。
    ///
    /// # Errors
    ///
    /// - 当 `week == 0` 或目标学期不存在该教学周时返回 [`Error::InvalidInput`]。
    /// - 其他错误条件与 [`Self::teaching_weeks`] 相同。
    pub async fn teaching_week(&self, week: u32, semester_id: Option<i64>) -> Result<TeachingWeek> {
        if week == 0 {
            return Err(Error::invalid_input("教学周从 1 开始".into()));
        }
        self.teaching_weeks(semester_id)
            .await?
            .into_iter()
            .find(|teaching_week| teaching_week.week_index == week)
            .ok_or_else(|| Error::invalid_input(format!("学期不存在教学周 {week}")))
    }

    /// 查询指定公历日期 `date` 在校历中对应的教学周序号；假期等非教学周日期返回 `Ok(None)`。
    ///
    /// 内部换算为 `date` 所在周的周一发起查询，以规避上游校历接口在秋季学期跨年后周二至周日返回周次少 1 的缺陷。
    ///
    /// # Errors
    ///
    /// - 当教务客户端尚未登录或底层 CAS 令牌获取失败时返回 [`Error::Authentication`]。
    /// - 当校历接口网络请求失败、返回远端业务错误或周次字段不是正整数时返回相应错误。
    pub async fn week_index(&self, date: Date) -> Result<Option<u32>> {
        #[derive(serde::Deserialize)]
        struct WeekData {
            date: Vec<String>,
        }
        #[derive(serde::Deserialize)]
        struct Inner {
            code: i64,
            success: bool,
            msg: String,
            data: WeekData,
        }
        self.current_semester()?;
        let token = self.cas.user_token().await?;
        let monday = date
            .iso_week_date()
            .first_of_week()
            .map_err(Error::decode)?
            .date();
        let request = self
            .http
            .get(
                "https://info.s.zzu.edu.cn/portal-api/v1/calendar/share/schedule/getWeekOfTeaching",
            )
            .header("X-Id-Token", token)
            .query(&[("today", monday.to_string())]);
        let response = http::json(request).await?;
        let inner: Inner = http::data(response, "code")?;
        if inner.code != 0 || !inner.success {
            return Err(Error::remote(inner.msg));
        }
        let week = inner
            .data
            .date
            .first()
            .ok_or_else(|| Error::decode_message("教学周 date 为空".into()))?;
        if week.is_empty() {
            return Ok(None);
        }
        let week = week
            .parse::<u32>()
            .map_err(|_| Error::decode_message("教学周必须为正整数".into()))?;
        if week == 0 {
            return Err(Error::decode_message("教学周必须为正整数".into()));
        }
        Ok(Some(week))
    }

    /// 清除本地缓存的教务当前学期登录状态（不影响底层共享的 [`CasClient`]）。
    pub fn logout(&mut self) {
        self.current_semester = None;
    }
}
