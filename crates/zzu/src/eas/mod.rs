//! 本科教务系统（EAMS）客户端与学期课表、教室占用数据结构。

mod classroom;
mod model;

pub use classroom::{
    Building, Campus, ClassroomCatalog, ClassroomFetchOptions, ClassroomOccupancy, ClassroomPage,
    ClassroomQuery, DEFAULT_CLASSROOM_CONCURRENCY, DEFAULT_CLASSROOM_PAGE_SIZE, RoomActivityType,
    RoomOccupation, RoomType,
};
pub use model::{
    Course, CourseTableEntry, Lesson, NamedEntity, PeriodInfo, Room, Schedule, Semester,
    TeachingWeek, TeachingWeeks,
};

use futures_util::stream::{self, StreamExt, TryStreamExt};
use jiff::civil::Date;
use reqwest::{Client, RequestBuilder};
use serde::de::DeserializeOwned;

use crate::{Error, Result, auth::CasClient, http};
use classroom::{RawClassroomPage, RoomsRequest, merge_pages, validate_page, validate_page_size};

const EAS_URL: &str = "https://jwxt.zzu.edu.cn";

/// 本科教务系统客户端。
///
/// 共享 [`CasClient`] 的统一身份认证会话；调用 [`Self::login`] 缓存当前学期信息后，即可查询学期、课表与教室占用。
pub struct UndergradEasClient {
    http: Client,
    cas: CasClient,
    base_url: String,
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
            base_url: EAS_URL.to_owned(),
            current_semester: None,
        }
    }

    fn get_with_token(&self, token: &str, path: &str) -> RequestBuilder {
        self.http
            .get(format!("{}{path}", self.base_url))
            .header("Authorization", token)
            .header("Cookie", format!("userToken={token}"))
    }

    fn post_with_token(&self, token: &str, path: &str) -> RequestBuilder {
        self.http
            .post(format!("{}{path}", self.base_url))
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

    /// 查询教务系统中的全部学期列表。
    ///
    /// # Errors
    ///
    /// - 教务客户端尚未登录时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    /// - 网络传输、远端状态校验或学期列表解码失败时返回错误。
    pub async fn semesters(&self) -> Result<Vec<Semester>> {
        self.fetch("/eams-door/api/v1/calendar/get-all-semesters")
            .await
    }

    /// 查询指定学期的完整教学周课表（`semester_id` 为 `None` 时取 [`Self::current_semester`]）。
    ///
    /// 当前学期直接复用登录时缓存的周次，其他学期会先查询学期列表。
    ///
    /// # Errors
    ///
    /// - 教务客户端尚未登录时返回 [`crate::AuthenticationErrorKind::NotAuthenticated`]。
    /// - 指定的 `semester_id` 在学期列表中不存在时返回 [`Error::NotFound`]。
    /// - 网络请求或课表时间、节次解码校验失败时返回错误。
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

    /// 按教学周序号 [`TeachingWeek::week_index`] 查询单周课表（`semester_id` 为 `None` 时取当前学期）。
    ///
    /// # Errors
    ///
    /// - `week == 0` 或目标学期不存在该教学周时返回 [`Error::InvalidInput`]。
    /// - 其余错误条件同 [`Self::teaching_weeks`]。
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

    /// 查询公历日期 `date` 在校历中对应的教学周序号；假期等非教学周返回 `Ok(None)`。
    ///
    /// 内部先换算为 `date` 所在周的周一再发起查询，以规避上游校历接口在秋季学期跨年后周二至周日周次少 1 的缺陷。
    ///
    /// # Errors
    ///
    /// - 教务客户端尚未登录或底层 CAS 令牌不可用时返回 [`Error::Authentication`]。
    /// - 校历接口请求失败、返回远端业务错误或周次字段不是正整数时返回错误。
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

    /// 并发查询教室占用筛选所需的全部参照目录（校区、教学楼、教室类型与楼层）。
    ///
    /// 内部并发请求 4 个目录接口，返回扁平列表供前端渲染筛选项及按
    /// [`ClassroomOccupancy::building_id`] 反查教学楼信息。
    ///
    /// # Errors
    ///
    /// - 教务客户端尚未登录或底层 CAS 令牌不可用时返回 [`Error::Authentication`]。
    /// - 任一目录接口请求失败、业务状态非零或响应解码失败时返回错误。
    pub async fn classroom_catalog(&self) -> Result<ClassroomCatalog> {
        self.current_semester()?;
        let token = self.cas.user_token().await?;
        let (campuses, buildings, room_types, floors) = tokio::try_join!(
            self.fetch_with_token::<Vec<Campus>>(
                &token,
                "/eams-micro-server/api/v1/room/place/campus"
            ),
            self.fetch_with_token::<Vec<Building>>(
                &token,
                "/eams-micro-server/api/v1/room/place/building?campusAssoc=",
            ),
            self.fetch_with_token::<Vec<RoomType>>(
                &token,
                "/eams-micro-server/api/v1/room/place/roomTypes",
            ),
            self.fetch_with_token::<Vec<i32>>(
                &token,
                "/eams-micro-server/api/v1/room/place/floors"
            ),
        )?;
        Ok(ClassroomCatalog {
            campuses,
            buildings,
            room_types,
            floors,
        })
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
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::{SocketAddr, TcpListener},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use jiff::Timestamp;
    use serde_json::{Value, json};

    use super::*;
    use crate::auth::Tokens;

    struct StubRequest {
        method: String,
        target: String,
        authorization: Option<String>,
        cookie: Option<String>,
        body: String,
    }

    struct StubServer {
        addr: SocketAddr,
        stop: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl StubServer {
        fn start<F>(handler: F) -> Self
        where
            F: Fn(StubRequest) -> String + Send + Sync + 'static,
        {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let stop_flag = stop.clone();
            let handler = Arc::new(handler);
            let handle = thread::spawn(move || {
                while !stop_flag.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let handler = handler.clone();
                            thread::spawn(move || {
                                stream.set_nonblocking(false).unwrap();
                                let mut reader = BufReader::new(stream);
                                let mut request_line = String::new();
                                if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                                    return;
                                }
                                let mut parts = request_line.split_whitespace();
                                let method = parts.next().unwrap_or_default().to_owned();
                                let target = parts.next().unwrap_or_default().to_owned();

                                let mut content_length = 0_usize;
                                let mut authorization = None;
                                let mut cookie = None;
                                loop {
                                    let mut line = String::new();
                                    reader.read_line(&mut line).unwrap();
                                    let trimmed = line.trim();
                                    if trimmed.is_empty() {
                                        break;
                                    }
                                    if let Some((name, value)) = trimmed.split_once(':') {
                                        let value = value.trim().to_owned();
                                        if name.eq_ignore_ascii_case("content-length") {
                                            content_length = value.parse().unwrap_or(0);
                                        } else if name.eq_ignore_ascii_case("authorization") {
                                            authorization = Some(value);
                                        } else if name.eq_ignore_ascii_case("cookie") {
                                            cookie = Some(value);
                                        }
                                    }
                                }

                                let mut body_bytes = vec![0_u8; content_length];
                                if content_length > 0 {
                                    reader.read_exact(&mut body_bytes).unwrap();
                                }
                                let body = String::from_utf8(body_bytes).unwrap();
                                let response_body = handler(StubRequest {
                                    method,
                                    target,
                                    authorization,
                                    cookie,
                                    body,
                                });
                                let response = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                                    response_body.len()
                                );
                                let mut stream = reader.into_inner();
                                let _ = stream.write_all(response.as_bytes());
                                let _ = stream.flush();
                            });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                addr,
                stop,
                handle: Some(handle),
            }
        }

        fn base_url(&self) -> String {
            format!("http://{}", self.addr)
        }
    }

    impl Drop for StubServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    async fn authenticated_eas(base_url: String) -> (UndergradEasClient, String) {
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
        let mut eas = UndergradEasClient::with_http_client(cas, Client::new());
        eas.base_url = base_url;
        eas.current_semester = Some(
            serde_json::from_value(json!({
                "id": 100,
                "code": "2026-1",
                "nameZh": "2026-2027学年第一学期",
                "nameEn": null,
                "schoolYear": "2026-2027",
                "startDate": "2026-09-01",
                "endDate": "2027-01-15",
                "weekStartOnSunday": false,
                "countInTerm": true,
                "season": "AUTUMN",
                "weekIndices": [1, 2]
            }))
            .unwrap(),
        );
        (eas, token)
    }

    fn room_json(id: i64, code: &str, name: &str, with_occupation: bool) -> Value {
        let occupations = if with_occupation {
            json!([{
                "id": id,
                "activityId": 468_530,
                "date": "2026-11-23",
                "startTime": 1010,
                "endTime": 1150,
                "activityType": "Lesson",
                "activityName": "课程：程序设计",
                "activityNameEn": "course: Programming",
                "teacherName": "教师",
                "teacherNameEn": null
            }])
        } else {
            Value::Null
        };
        json!({
            "id": id,
            "nameZh": name,
            "nameEn": null,
            "code": code,
            "buildingId": 5,
            "campusNameZh": "主校区",
            "floor": 0,
            "virtual": false,
            "seatsForLesson": 0,
            "enabled": false,
            "experiment": false,
            "roomOccupationInfoVms": occupations
        })
    }

    #[tokio::test]
    async fn classroom_catalog_fetches_all_four_endpoints_and_maps_fields() {
        let server = StubServer::start(|request| {
            assert_eq!(request.method, "GET");
            assert!(request.authorization.is_some());
            assert!(
                request
                    .cookie
                    .as_deref()
                    .is_some_and(|cookie| cookie.starts_with("userToken="))
            );
            match request.target.as_str() {
                "/eams-micro-server/api/v1/room/place/campus" => json!({
                    "result": 0,
                    "message": null,
                    "data": [{
                        "id": 2, "code": "01", "nameZh": "主校区", "nameEn": null,
                        "remark": null, "enabled": true
                    }]
                })
                .to_string(),
                "/eams-micro-server/api/v1/room/place/building?campusAssoc=" => json!({
                    "result": 0,
                    "message": null,
                    "data": [{
                        "id": 5, "code": "北1", "nameZh": "北1", "nameEn": null,
                        "campusAssoc": 2, "remark": null, "enabled": true
                    }]
                })
                .to_string(),
                "/eams-micro-server/api/v1/room/place/roomTypes" => json!({
                    "result": 0,
                    "message": null,
                    "data": [{
                        "id": 2, "code": "01", "nameZh": "多媒体教室", "nameEn": null,
                        "enabled": true
                    }]
                })
                .to_string(),
                "/eams-micro-server/api/v1/room/place/floors" => json!({
                    "result": 0,
                    "message": null,
                    "data": [1, 2, 3]
                })
                .to_string(),
                other => panic!("unexpected path: {other}"),
            }
        });

        let (eas, _) = authenticated_eas(server.base_url()).await;
        let catalog = eas.classroom_catalog().await.unwrap();
        assert_eq!(catalog.campuses.len(), 1);
        assert_eq!(catalog.campuses[0].name_zh, "主校区");
        assert_eq!(catalog.buildings.len(), 1);
        assert_eq!(catalog.buildings[0].campus_id, 2);
        assert_eq!(catalog.room_types.len(), 1);
        assert_eq!(catalog.room_types[0].name_zh, "多媒体教室");
        assert_eq!(catalog.floors, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn classroom_occupancies_page_serializes_filters_and_decodes_page() {
        let server = StubServer::start(|request| {
            assert_eq!(request.method, "POST");
            assert_eq!(request.target, "/eams-micro-server/api/v1/room/place/rooms");
            let payload: Value = serde_json::from_str(&request.body).unwrap();
            assert_eq!(payload["currentPage"], 2);
            assert_eq!(payload["pageSize"], DEFAULT_CLASSROOM_PAGE_SIZE);
            assert_eq!(payload["campusAssoc"], 2);
            assert_eq!(payload["buildingIds"], json!([5, 6]));
            assert_eq!(payload["roomTypeIds"], json!([2]));
            assert_eq!(payload["floors"], json!([1, 2]));
            assert_eq!(payload["minSeat"], "38");
            assert_eq!(payload["maxSeat"], "74");
            assert_eq!(payload["date"], "2026-11-23");
            json!({
                "result": 0,
                "message": null,
                "data": {
                    "data": [
                        room_json(101, "001", "北1-101", true),
                        room_json(102, "002", "北1-102", false)
                    ],
                    "_page_": {
                        "currentPage": 2,
                        "rowsInPage": 2,
                        "rowsPerPage": DEFAULT_CLASSROOM_PAGE_SIZE,
                        "totalRows": 100,
                        "totalPages": 3
                    }
                }
            })
            .to_string()
        });

        let (eas, _) = authenticated_eas(server.base_url()).await;
        let query = ClassroomQuery {
            date: "2026-11-23".parse().unwrap(),
            campus_id: Some(2),
            building_ids: vec![5, 6],
            room_type_ids: vec![2],
            floors: vec![1, 2],
            min_seats: Some(38),
            max_seats: Some(74),
        };
        let page = eas
            .classroom_occupancies_page(&query, 2, None)
            .await
            .unwrap();
        assert_eq!(page.page, 2);
        assert_eq!(page.page_size, DEFAULT_CLASSROOM_PAGE_SIZE);
        assert_eq!(page.total_items, 100);
        assert_eq!(page.total_pages, 3);
        assert!(page.has_next());
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].occupations.len(), 1);
        assert!(page.items[1].occupations.is_empty());
    }

    #[tokio::test]
    async fn classroom_occupancies_fetches_concurrently_preserves_order_and_deduplicates() {
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let in_flight_ref = in_flight.clone();
        let max_in_flight_ref = max_in_flight.clone();

        let server = StubServer::start(move |request| {
            let payload: Value = serde_json::from_str(&request.body).unwrap();
            let page = payload["currentPage"].as_u64().unwrap();
            let current = in_flight_ref.fetch_add(1, Ordering::AcqRel) + 1;
            max_in_flight_ref.fetch_max(current, Ordering::AcqRel);

            // 让第 2 页比后续页稍慢，验证乱序返回时仍严格按页码顺序合并。
            let delay_ms = match page {
                1 => 0,
                2 => 90,
                _ => 35,
            };
            if delay_ms > 0 {
                thread::sleep(Duration::from_millis(delay_ms));
            }
            in_flight_ref.fetch_sub(1, Ordering::AcqRel);

            let items = match page {
                1 => vec![
                    room_json(1, "001", "R1", false),
                    room_json(2, "002", "R2", false),
                ],
                2 => vec![
                    room_json(3, "003", "R3", false),
                    room_json(4, "004", "R4", false),
                ],
                // 故意携带与第 2 页重复的 id=4，验证去重保序。
                3 => vec![
                    room_json(4, "004", "R4-dup", false),
                    room_json(5, "005", "R5", false),
                ],
                4 => vec![room_json(6, "006", "R6", false)],
                5 => vec![room_json(7, "007", "R7", false)],
                other => panic!("unexpected page: {other}"),
            };
            json!({
                "result": 0,
                "message": null,
                "data": {
                    "data": items,
                    "_page_": {
                        "currentPage": page,
                        "rowsInPage": 2,
                        "rowsPerPage": 2,
                        "totalRows": 7,
                        "totalPages": 5
                    }
                }
            })
            .to_string()
        });

        let (eas, _) = authenticated_eas(server.base_url()).await;
        let query = ClassroomQuery::new("2026-11-23".parse().unwrap());
        let rooms = eas
            .classroom_occupancies(
                &query,
                ClassroomFetchOptions {
                    page_size: 2,
                    concurrency: 2,
                },
            )
            .await
            .unwrap();
        let ids: Vec<i64> = rooms.iter().map(|room| room.id).collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(rooms[3].name_zh, "R4");
        let observed_max = max_in_flight.load(Ordering::Acquire);
        assert!(
            observed_max <= 2,
            "expected max concurrency <= 2, got {observed_max}"
        );
        assert!(
            observed_max >= 2,
            "expected concurrent requests, got {observed_max}"
        );
    }

    #[tokio::test]
    async fn classroom_occupancies_fails_fast_when_a_page_errors() {
        let server = StubServer::start(|request| {
            let payload: Value = serde_json::from_str(&request.body).unwrap();
            let page = payload["currentPage"].as_u64().unwrap();
            if page == 3 {
                return json!({
                    "result": 1,
                    "message": "服务端查询失败",
                    "data": null
                })
                .to_string();
            }
            json!({
                "result": 0,
                "message": null,
                "data": {
                    "data": [room_json(i64::try_from(page).unwrap(), "001", "R", false)],
                    "_page_": {
                        "currentPage": page,
                        "rowsInPage": 1,
                        "rowsPerPage": 1,
                        "totalRows": 4,
                        "totalPages": 4
                    }
                }
            })
            .to_string()
        });

        let (eas, _) = authenticated_eas(server.base_url()).await;
        let query = ClassroomQuery::new("2026-11-23".parse().unwrap());
        let error = eas
            .classroom_occupancies(
                &query,
                ClassroomFetchOptions {
                    page_size: 1,
                    concurrency: 2,
                },
            )
            .await
            .unwrap_err();
        assert!(error.is_remote());
    }

    #[tokio::test]
    async fn classroom_queries_reject_invalid_inputs_and_unauthenticated_client() {
        let cas = CasClient::new("", "").unwrap();
        let eas = UndergradEasClient::new(cas).unwrap();
        let date: Date = "2026-11-23".parse().unwrap();
        let query = ClassroomQuery::new(date);

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
