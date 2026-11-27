//! 本科教务系统（EAMS）客户端与学期校历、课表、成绩单、考试安排、培养方案及教室占用数据结构。

mod calendar;
mod classroom;
mod common;
mod exam;
mod plan;
mod semester;
mod timetable;
mod transcript;

pub use calendar::CalendarExportOptions;
pub use classroom::{
    Building, Campus, ClassroomCatalog, ClassroomFetchOptions, ClassroomOccupancy, ClassroomPage,
    ClassroomQuery, DEFAULT_CLASSROOM_CONCURRENCY, DEFAULT_CLASSROOM_PAGE_SIZE, RoomActivityType,
    RoomOccupation, RoomType,
};
pub use common::{CourseUnit, DayPart, NamedEntity};
pub use exam::Exam;
pub use plan::{CompletionStatus, PlanCourse, PlanModule, ProgramCompletion};
pub use semester::{Semester, SemesterSeason};
pub use timetable::{
    ClassOrigin, ClassSession, Course, ExperimentProject, PeriodInfo, Room, ScheduleState,
    SessionConflict, SessionKind, SessionState, SessionTeacher, SessionView, TeacherName,
    TeachingClass, Timetable, TimetableOptions, WeekView,
};
pub use transcript::{
    CourseGrade, GpaRank, GradeSemester, GradeState, ProcessGrade, StudyType, SubGrade, Transcript,
};

use std::{collections::BTreeMap, sync::Arc};

use futures_util::future::{try_join, try_join5};
use futures_util::stream::{self, StreamExt, TryStreamExt};
use reqwest::{Client, RequestBuilder};
use serde::de::DeserializeOwned;

use crate::{Error, RequestContext, Result, auth::CasClient, http};
use classroom::{
    RawBuilding, RawCampus, RawClassroomPage, RawRoomType, RoomsRequest, merge_pages,
    validate_page, validate_page_size,
};
use exam::normalize_exams;
use plan::{RawCompletionInfo, build_program_completion};
use timetable::{RawCourseTableEntry, build_timetable, normalize_unit_layouts};
use transcript::{RawCourseGrade, RawGpaRank, build_transcript};

const EAS_URL: &str = "https://jwxt.zzu.edu.cn";

/// 本科教务系统客户端。
///
/// 共享 [`CasClient`] 的统一身份认证会话；调用 [`Self::login`] 缓存当前学期信息后，即可查询学期、课表与教室占用。
pub struct UndergradEasClient {
    http: Client,
    cas: CasClient,
    current_semester: Option<Semester>,
}

impl UndergradEasClient {
    /// 使用共享的 [`CasClient`] 与默认 HTTP 配置创建本科教务客户端。
    ///
    /// # Errors
    ///
    /// 默认 HTTP 客户端初始化失败时返回错误。
    pub fn new(cas: CasClient) -> Result<Self> {
        Ok(Self::with_http_client(cas, http::client()?))
    }

    /// 使用共享的 [`CasClient`] 与自定义 [`reqwest::Client`] 创建本科教务客户端。
    #[must_use]
    pub fn with_http_client(cas: CasClient, client: Client) -> Self {
        Self {
            http: client,
            cas,
            current_semester: None,
        }
    }

    fn get_with_token(&self, token: &str, path: &str) -> RequestBuilder {
        self.http
            .get(format!("{EAS_URL}{path}"))
            .header("Authorization", token)
            .header("Cookie", format!("userToken={token}"))
    }

    fn post_with_token(&self, token: &str, path: &str) -> RequestBuilder {
        self.http
            .post(format!("{EAS_URL}{path}"))
            .header("Authorization", token)
            .header("Cookie", format!("userToken={token}"))
    }

    async fn request(&self, path: &str) -> Result<RequestBuilder> {
        let token = self.cas.user_token().await?;
        Ok(self.get_with_token(&token, path))
    }

    async fn fetch_with_token<T: DeserializeOwned>(&self, token: &str, path: &str) -> Result<T> {
        let response = http::json(self.get_with_token(token, path)).await?;
        http::data(response, "result")
    }

    async fn fetch_with_context<T: DeserializeOwned>(
        &self,
        token: &str,
        path: &str,
    ) -> Result<(T, Arc<RequestContext>)> {
        let response = http::json(self.get_with_token(token, path)).await?;
        let context = response.context.clone();
        Ok((http::data(response, "result")?, context))
    }

    /// 验证教务门户会话，并拉取、缓存当前学期元数据。
    ///
    /// # Errors
    ///
    /// - 底层 [`CasClient`] 尚未登录时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    /// - 教务门户用户信息或当前学期接口请求失败、业务状态非零或响应解析失败时返回错误。
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
    /// 尚未调用 [`Self::login`] 登录时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
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

    /// 查询当前学生可见的全部学期列表。
    ///
    /// # Errors
    ///
    /// - 教务客户端尚未登录时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    /// - 网络传输、远端状态校验或学期列表解码失败时返回错误。
    pub async fn semesters(&self) -> Result<Vec<Semester>> {
        self.fetch("/eams-micro-server/api/v1/semester/std-semesters")
            .await
    }

    /// 查询指定学期的完整课表（`semester_id` 为 `None` 时取 [`Self::current_semester`]）。
    ///
    /// 当前学期直接复用登录时缓存的学期信息，其他学期会先拉取学期列表。
    /// 返回的 [`Timetable`] 包含学期校历、适用的每日节次作息、全部教学班与按时间排序的排课场次，
    /// 按周查询、冲突检测与日历导出均在本地完成。
    ///
    /// # Errors
    ///
    /// - 教务客户端尚未登录时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    /// - 指定的 `semester_id` 在学期列表中不存在时返回 [`Error::NotFound`]。
    /// - 网络请求、远端状态校验或课表解析失败时返回错误。
    pub async fn timetable(
        &self,
        semester_id: Option<i64>,
        options: TimetableOptions,
    ) -> Result<Timetable> {
        let current = self.current_semester()?;
        let semester = match semester_id {
            None => current.clone(),
            Some(id) if id == current.id => current.clone(),
            Some(id) => self
                .semesters()
                .await?
                .into_iter()
                .find(|semester| semester.id == id)
                .ok_or_else(|| Error::not_found(format!("semester_id={id}")))?,
        };
        let token = self.cas.user_token().await?;
        let course_table_path = format!(
            "/eams-micro-server/api/v1/lesson/student/course-table/{}",
            semester.id
        );
        let fetch_entries = async {
            let request = self
                .get_with_token(&token, &course_table_path)
                .query(&[("hasExperiment", options.include_experiments)]);
            let response = http::json(request).await?;
            let context = response.context.clone();
            Ok::<_, Error>((
                http::data::<Vec<RawCourseTableEntry>>(response, "result")?,
                context,
            ))
        };
        let fetch_layouts = self.fetch_with_context::<BTreeMap<String, Vec<CourseUnit>>>(
            &token,
            "/eams-micro-server/api/v1/system/get-all-course-units",
        );
        let ((entries, entries_context), (unit_layouts, layouts_context)) =
            try_join(fetch_entries, fetch_layouts).await?;
        let normalized_layouts = normalize_unit_layouts(unit_layouts)
            .map_err(|error| error.with_request(layouts_context))?;
        build_timetable(semester, entries, normalized_layouts)
            .map_err(|error| error.with_request(entries_context))
    }

    /// 并发查询当前学生的完整成绩单与累计专业绩点排名。
    ///
    /// 内部并发请求全部学期课程成绩与累计绩点排名接口，剥离重复的学生与教学班冗余字段，
    /// 按最新学期在前排序，并对尚未完成网上评教的课程自动隐藏占位零分。
    ///
    /// # Errors
    ///
    /// - 教务客户端尚未登录或底层 CAS 令牌不可用时返回 [`Error::Authentication`]。
    /// - 成绩或绩点排名接口请求失败、业务状态非零或响应解码校验失败时返回错误。
    pub async fn transcript(&self) -> Result<Transcript> {
        self.current_semester()?;
        let token = self.cas.user_token().await?;
        let (grades, gpa_rank) = try_join(
            self.fetch_with_token::<Vec<RawCourseGrade>>(
                &token,
                "/eams-micro-server/api/v1/grade/student/grades",
            ),
            self.fetch_with_token::<Option<RawGpaRank>>(
                &token,
                "/eams-micro-server/api/v1/grade/student/gpa-rank",
            ),
        )
        .await?;
        Ok(build_transcript(grades, gpa_rank))
    }

    /// 查询当前学生尚未结束的考试安排列表。
    ///
    /// 教务移动端接口仅返回待考安排（已结束的历史考试不在返回范围内）。结果按考试时间升序排列，
    /// 尚未排定具体时间的考试排在末尾。
    ///
    /// # Errors
    ///
    /// - 教务客户端尚未登录或底层 CAS 令牌不可用时返回 [`Error::Authentication`]。
    /// - 网络传输、远端状态校验或考试时间解码失败时返回错误。
    pub async fn exams(&self) -> Result<Vec<Exam>> {
        let exams: Vec<Exam> = self
            .fetch("/eams-micro-server/api/v1/exam/student/exam")
            .await?;
        Ok(normalize_exams(exams))
    }

    /// 查询当前学生在各培养类别（主修、辅修等）下的培养方案完成进度。
    ///
    /// 内部先查询学生关联的培养类别列表，再按类别编码并发拉取各方案的模块学分树与计划外课程记录，
    /// 调用方无需手动传递上游必填的 `code` 参数。
    ///
    /// # Errors
    ///
    /// - 教务客户端尚未登录或底层 CAS 令牌不可用时返回 [`Error::Authentication`]。
    /// - 培养类别或完成详情接口请求失败、业务状态非零、缺少类别编码或响应解码失败时返回错误。
    pub async fn program_completion(&self) -> Result<Vec<ProgramCompletion>> {
        self.current_semester()?;
        let token = self.cas.user_token().await?;
        let (cultivate_types, cultivate_context) = self
            .fetch_with_context::<Vec<NamedEntity>>(
                &token,
                "/eams-micro-server/api/v1/plan/search/completion/cultivateTypes",
            )
            .await?;
        stream::iter(cultivate_types)
            .map(|cultivate| {
                let token = &token;
                let cultivate_context = cultivate_context.clone();
                async move {
                    let code = cultivate.code.trim();
                    if code.is_empty() {
                        return Err(Error::decode_message("培养类别缺少有效的 code 字段".into())
                            .with_request(cultivate_context));
                    }
                    let request = self
                        .get_with_token(
                            token,
                            "/eams-micro-server/api/v1/plan/search/completion/info",
                        )
                        .query(&[("code", code)]);
                    let response = http::json(request).await?;
                    let info: RawCompletionInfo = http::data(response, "result")?;
                    Ok(build_program_completion(cultivate, info))
                }
            })
            .buffered(4)
            .try_collect()
            .await
    }

    /// 并发查询教室占用筛选所需的全部参照目录（校区、教学楼、教室类型、楼层与标准节次作息）。
    ///
    /// 内部并发请求 5 个目录接口，返回扁平列表供前端渲染筛选项、绘制节次概览及按
    /// [`ClassroomOccupancy::building_id`] 反查教学楼信息。
    ///
    /// # Errors
    ///
    /// - 教务客户端尚未登录或底层 CAS 令牌不可用时返回 [`Error::Authentication`]。
    /// - 任一目录接口请求失败、业务状态非零或响应解码失败时返回错误。
    pub async fn classroom_catalog(&self) -> Result<ClassroomCatalog> {
        self.current_semester()?;
        let token = self.cas.user_token().await?;
        let (campuses, buildings, room_types, floors, (units, units_context)) = try_join5(
            self.fetch_with_token::<Vec<RawCampus>>(
                &token,
                "/eams-micro-server/api/v1/room/place/campus",
            ),
            self.fetch_with_token::<Vec<RawBuilding>>(
                &token,
                "/eams-micro-server/api/v1/room/place/building?campusAssoc=",
            ),
            self.fetch_with_token::<Vec<RawRoomType>>(
                &token,
                "/eams-micro-server/api/v1/room/place/roomTypes",
            ),
            self.fetch_with_token::<Vec<i32>>(
                &token,
                "/eams-micro-server/api/v1/room/place/floors",
            ),
            self.fetch_with_context::<Vec<CourseUnit>>(
                &token,
                "/eams-micro-server/api/v1/room/system/course-units",
            ),
        )
        .await?;
        ClassroomCatalog::new(
            campuses.into_iter().map(Campus::from).collect(),
            buildings.into_iter().map(Building::from).collect(),
            room_types.into_iter().map(RoomType::from).collect(),
            floors,
            units,
        )
        .map_err(|error| error.with_request(units_context))
    }

    async fn fetch_classroom_page(
        &self,
        token: &str,
        query: &ClassroomQuery,
        page: u32,
        page_size: u32,
    ) -> Result<ClassroomPage> {
        let request = self
            .post_with_token(token, "/eams-micro-server/api/v1/room/place/rooms")
            .json(&RoomsRequest::new(query, page, page_size));
        let response = http::json(request).await?;
        let raw: RawClassroomPage = http::data(response, "result")?;
        Ok(raw.into())
    }

    /// 按页查询符合筛选条件的教室及其指定日期的占用情况。
    ///
    /// `page` 从 `1` 开始；`page_size` 为 `None` 时默认采用 [`DEFAULT_CLASSROOM_PAGE_SIZE`]（`44`）。
    ///
    /// 注意：教务上游该接口存在严重的性能问题，响应耗时随 `page_size` 呈非线性放大
    /// （`100` 约 1 秒以上，`1000` 约 25 秒以上）；一般而言 `44` 左右是单条均摊耗时最低的甜点值。
    /// 若不清楚后端性能特性，建议保持默认值；在高占用率校区可适当调小以降低单页尾延迟。
    ///
    /// # Errors
    ///
    /// - `page == 0`、`page_size == Some(0)` 或 `query.min_seats > query.max_seats` 时返回 [`Error::InvalidInput`]。
    /// - 教务客户端尚未登录或底层 CAS 令牌不可用时返回 [`Error::Authentication`]。
    /// - 网络请求失败、业务状态非零或占用时间段解码校验失败时返回错误。
    pub async fn classroom_occupancies_page(
        &self,
        query: &ClassroomQuery,
        page: u32,
        page_size: Option<u32>,
    ) -> Result<ClassroomPage> {
        query.validate()?;
        validate_page(page)?;
        let page_size = page_size.unwrap_or(DEFAULT_CLASSROOM_PAGE_SIZE);
        validate_page_size(page_size)?;
        self.current_semester()?;
        let token = self.cas.user_token().await?;
        self.fetch_classroom_page(&token, query, page, page_size)
            .await
    }

    /// 以有界并发拉取符合筛选条件的全部教室及其指定日期占用情况。
    ///
    /// 内部先以 `options.page_size` 拉取第 1 页获知总页数，再以 `options.concurrency` 为并发上限
    /// 并发拉取剩余分页，最后按页序合并并按教室 ID 去重。任意一页失败时立即中止在途分页并返回错误（fail-fast）。
    ///
    /// 提示：不带空间筛选条件的整校查询（约 1658 间教室）在高占用时段耗时较长，
    /// 建议配合校区或教学楼筛选、酌情放宽调用时限，或在需要进度展示与单页容错时改用
    /// [`Self::classroom_occupancies_page`]。
    ///
    /// # Errors
    ///
    /// - `query`、`options.page_size` 或 `options.concurrency` 参数非法时返回 [`Error::InvalidInput`]。
    /// - 教务客户端尚未登录或底层 CAS 令牌不可用时返回 [`Error::Authentication`]。
    /// - 任一分页请求失败或响应解码校验失败时返回错误。
    pub async fn classroom_occupancies(
        &self,
        query: &ClassroomQuery,
        options: ClassroomFetchOptions,
    ) -> Result<Vec<ClassroomOccupancy>> {
        query.validate()?;
        options.validate()?;
        self.current_semester()?;
        let token = self.cas.user_token().await?;
        let first = self
            .fetch_classroom_page(&token, query, 1, options.page_size)
            .await?;
        if first.total_pages <= 1 {
            return Ok(merge_pages(first, Vec::new()));
        }
        let remaining = stream::iter(2..=first.total_pages)
            .map(|page| {
                let token = &token;
                async move {
                    let result = self
                        .fetch_classroom_page(token, query, page, options.page_size)
                        .await?;
                    Ok::<_, Error>((page, result))
                }
            })
            .buffer_unordered(options.concurrency)
            .try_collect::<Vec<_>>()
            .await?;
        Ok(merge_pages(first, remaining))
    }

    /// 清除本地缓存的教务当前学期登录状态（不影响底层共享的 [`CasClient`]）。
    pub fn logout(&mut self) {
        self.current_semester = None;
    }
}

#[cfg(test)]
mod tests {
    use jiff::civil::Date;

    use super::*;

    #[tokio::test]
    async fn classroom_queries_reject_invalid_inputs_and_unauthenticated_client() {
        let cas = CasClient::new("", "", "test-device").unwrap();
        let eas = UndergradEasClient::new(cas).unwrap();
        let date: Date = "2026-11-23".parse().unwrap();
        let query = ClassroomQuery::new(date);

        assert!(eas.transcript().await.unwrap_err().is_not_authenticated());
        assert!(eas.exams().await.unwrap_err().is_not_authenticated());
        assert!(
            eas.program_completion()
                .await
                .unwrap_err()
                .is_not_authenticated()
        );
        assert!(
            eas.classroom_catalog()
                .await
                .unwrap_err()
                .is_not_authenticated()
        );
        assert!(
            eas.classroom_occupancies_page(&query, 1, None)
                .await
                .unwrap_err()
                .is_not_authenticated()
        );
        assert!(
            eas.classroom_occupancies(&query, ClassroomFetchOptions::default())
                .await
                .unwrap_err()
                .is_not_authenticated()
        );

        assert!(
            eas.classroom_occupancies_page(&query, 0, None)
                .await
                .unwrap_err()
                .is_invalid_input()
        );
        assert!(
            eas.classroom_occupancies_page(&query, 1, Some(0))
                .await
                .unwrap_err()
                .is_invalid_input()
        );
        assert!(
            eas.classroom_occupancies(
                &query,
                ClassroomFetchOptions {
                    page_size: 44,
                    concurrency: 0,
                },
            )
            .await
            .unwrap_err()
            .is_invalid_input()
        );

        let mut bad_seats = query;
        bad_seats.min_seats = Some(100);
        bad_seats.max_seats = Some(50);
        assert!(
            eas.classroom_occupancies_page(&bad_seats, 1, None)
                .await
                .unwrap_err()
                .is_invalid_input()
        );
    }
}
