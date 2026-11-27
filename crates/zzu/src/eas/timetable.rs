use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use jiff::{ZonedDateTime, civil::Date};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::{Error, Result};

use super::{
    common::{
        CourseUnit, NamedEntity, deserialize_null_default, normalize_course_units, parse_time,
        trim_non_empty,
    },
    semester::Semester,
};

/// 拉取学期课表时的请求选项。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TimetableOptions {
    /// 是否包含实验系统排课（默认 `true`）。
    pub include_experiments: bool,
}

impl Default for TimetableOptions {
    fn default() -> Self {
        Self {
            include_experiments: true,
        }
    }
}

/// 课程学时与周次统计信息。
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PeriodInfo {
    /// 课程总学时。
    pub total: u32,
    /// 开课总周数。
    #[serde(default)]
    pub weeks: Option<u32>,
    /// 每周学时数。
    #[serde(default)]
    pub periods_per_week: Option<u32>,
    /// 理论学时。
    #[serde(default)]
    pub theory: Option<u32>,
    /// 实验学时。
    #[serde(default)]
    pub experiment: Option<u32>,
    /// 实践学时。
    #[serde(default)]
    pub practice: Option<u32>,
    /// 设计学时。
    #[serde(default)]
    pub design: Option<u32>,
    /// 上机学时。
    #[serde(default)]
    pub machine: Option<u32>,
    /// 测验学时。
    #[serde(default)]
    pub test: Option<u32>,
}

/// 课程目录级基础元数据。
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Course {
    /// 课程数字 ID。
    pub id: i64,
    /// 课程代码。
    pub code: String,
    /// 课程中文名称。
    pub name_zh: String,
    /// 课程英文名称。
    #[serde(default)]
    pub name_en: Option<String>,
    /// 课程学分（独立实验课程可能为 `None`）。
    #[serde(default)]
    pub credits: Option<f64>,
    /// 学时与周次统计。
    pub period_info: PeriodInfo,
}

/// 教学班任课教师姓名。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TeacherName {
    /// 教师中文姓名。
    pub name_zh: String,
    /// 教师英文姓名。
    #[serde(default)]
    pub name_en: Option<String>,
}

/// 教学班数据来源。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ClassOrigin {
    /// 教务常规选课教学班。
    Regular,
    /// 未匹配到常规教学班时，由独立实验排课归并生成的教学班。
    StandaloneExperiment,
}

/// 教学班排课发布状态。
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(rename_all = "snake_case")
)]
pub enum ScheduleState {
    /// 已发布（上游 `"published"`）。
    Published,
    /// 未发布（上游 `"unpublish"` / `"unpublished"`）。
    Unpublished,
    /// 上游返回的其他状态文本。
    #[cfg_attr(feature = "schemars", schemars(untagged))]
    Other(String),
}

impl ScheduleState {
    /// 返回规范化后的字符串标识。
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Published => "published",
            Self::Unpublished => "unpublished",
            Self::Other(other) => other.as_str(),
        }
    }
}

impl fmt::Display for ScheduleState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<String> for ScheduleState {
    fn from(value: String) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "published" => Self::Published,
            "unpublish" | "unpublished" => Self::Unpublished,
            _ => Self::Other(value),
        }
    }
}

impl Serialize for ScheduleState {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ScheduleState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from(value))
    }
}

/// 学期内选修的一个教学班（包含未排课课程与实验课程父班）。
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TeachingClass {
    /// 教学班数字 ID（在 [`Timetable::classes`] 中唯一）。
    pub id: i64,
    /// 教学班选课课号（如 `"222302-013"`）。
    pub code: String,
    /// 教学班来源。
    pub origin: ClassOrigin,
    /// 关联的课程元数据。
    pub course: Course,
    /// 课程性质（如专业必修课、分类通识课）。
    #[serde(default)]
    pub course_type: Option<NamedEntity>,
    /// 培养类型（如主修、辅修）。
    #[serde(default)]
    pub cultivate_type: Option<NamedEntity>,
    /// 开课院系或教学单位。
    #[serde(default)]
    pub department: Option<NamedEntity>,
    /// 开课校区。
    #[serde(default)]
    pub campus: Option<NamedEntity>,
    /// 教学班全部任课教师列表。
    #[serde(default)]
    pub teachers: Vec<TeacherName>,
    /// 选课学生人数。
    #[serde(default)]
    pub student_count: Option<u32>,
    /// 教务排课发布状态。
    pub schedule_state: ScheduleState,
    /// 官方汇总排课中文文本摘要。
    #[serde(default)]
    pub schedule_text: Option<String>,
    /// 官方汇总排课英文文本摘要。
    #[serde(default)]
    pub schedule_text_en: Option<String>,
    /// 教学班备注信息。
    #[serde(default)]
    pub remark: Option<String>,
}

/// 单次排课的授课类型。
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(rename_all = "snake_case")
)]
pub enum SessionKind {
    /// 理论课（上游 `"THEORY"`）。
    Theory,
    /// 实验课（上游 `"EXPERIMENT"` 或实验系统排课条目）。
    Experiment,
    /// 实践课（上游 `"PRACTICE"`）。
    Practice,
    /// 上机课（上游 `"MACHINE"`）。
    Machine,
    /// 设计课（上游 `"DESIGN"`）。
    Design,
    /// 上游返回的其他授课类型文本。
    #[cfg_attr(feature = "schemars", schemars(untagged))]
    Other(String),
}

impl SessionKind {
    /// 返回规范化后的字符串标识。
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Theory => "theory",
            Self::Experiment => "experiment",
            Self::Practice => "practice",
            Self::Machine => "machine",
            Self::Design => "design",
            Self::Other(other) => other.as_str(),
        }
    }

    /// 返回用于展示的中文类型名称。
    #[must_use]
    pub fn label_zh(&self) -> &str {
        match self {
            Self::Theory => "理论",
            Self::Experiment => "实验",
            Self::Practice => "实践",
            Self::Machine => "上机",
            Self::Design => "设计",
            Self::Other(other) => other.as_str(),
        }
    }
}

impl fmt::Display for SessionKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<String> for SessionKind {
    fn from(value: String) -> Self {
        match value.trim().to_ascii_uppercase().as_str() {
            "THEORY" => Self::Theory,
            "EXPERIMENT" => Self::Experiment,
            "PRACTICE" => Self::Practice,
            "MACHINE" => Self::Machine,
            "DESIGN" => Self::Design,
            _ => Self::Other(value),
        }
    }
}

impl Serialize for SessionKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from(value))
    }
}

/// 单次排课的执行状态。
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(rename_all = "snake_case")
)]
pub enum SessionState {
    /// 未完成（上游 `"未完成"` / `"pending"`）。
    Pending,
    /// 已完成或已结束（上游 `"已完成"` / `"已结束"` / `"completed"`）。
    Completed,
    /// 上游返回的其他状态文本。
    #[cfg_attr(feature = "schemars", schemars(untagged))]
    Other(String),
}

impl SessionState {
    /// 返回规范化后的字符串标识。
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Other(other) => other.as_str(),
        }
    }
}

impl fmt::Display for SessionState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<String> for SessionState {
    fn from(value: String) -> Self {
        match value.trim() {
            "未完成" | "pending" | "PENDING" => Self::Pending,
            "已完成" | "已结束" | "completed" | "COMPLETED" => Self::Completed,
            _ => Self::Other(value),
        }
    }
}

impl Serialize for SessionState {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from(value))
    }
}

/// 单次排课场次的授课教师信息。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SessionTeacher {
    /// 教务教师 ID（上游未提供时为 `None`）。
    #[serde(default)]
    pub id: Option<i64>,
    /// 人员 ID（上游未提供时为 `None`）。
    #[serde(default)]
    pub person_id: Option<i64>,
    /// 教师中文姓名。
    pub name_zh: String,
    /// 教师英文姓名。
    #[serde(default)]
    pub name_en: Option<String>,
}

/// 实验排课关联的具体实验项目信息。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ExperimentProject {
    /// 实验项目 ID。
    pub id: i64,
    /// 实验项目编码。
    pub code: String,
    /// 实验项目中文名称。
    pub name_zh: String,
    /// 该实验项目总学时。
    #[serde(default)]
    pub total_periods: Option<u32>,
}

/// 上课教室及其所属教学楼与校区信息。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Room {
    /// 教室数字 ID。
    pub id: i64,
    /// 教室中文名称。
    pub name_zh: String,
    /// 教室英文名称。
    #[serde(default)]
    pub name_en: Option<String>,
    /// 所属教学楼。
    pub building: NamedEntity,
    /// 所属校区。
    pub campus: NamedEntity,
    /// 座位号说明（常规排课通常为 `None`）。
    #[serde(default)]
    pub seat_number: Option<String>,
}

/// 单次具体上课场次。
///
/// 同教学班在同一时间、同一地点由多位教师合上的上游重复记录会无损合并为单条场次，
/// 所有教师按顺序保留在 [`Self::teachers`] 中。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(!try_from)
)]
#[serde(rename_all = "camelCase", try_from = "RawClassSession")]
pub struct ClassSession {
    /// 所属教学班 ID（指向 [`Timetable::classes`] 中的某一项）。
    pub class_id: i64,
    /// 排课组 ID。
    pub schedule_group_id: i64,
    /// 所属教学周序号（从 1 开始）。
    pub week_index: u32,
    /// 上课公历日期。
    #[cfg_attr(feature = "schemars", schemars(with = "crate::schema::Date"))]
    pub date: Date,
    /// 调课前的原始上课日期（未发生调课时为 `None`）。
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<crate::schema::Date>"))]
    pub original_date: Option<Date>,
    /// 星期序号（`1` 表示周一，`7` 表示周日）。
    pub weekday: u8,
    /// 有效开始时间（`Asia/Shanghai` 时区；上游提供非零实际时间时优先取实际时间）。
    #[cfg_attr(feature = "schemars", schemars(with = "crate::schema::ZonedDateTime"))]
    pub start_time: ZonedDateTime,
    /// 有效结束时间（`Asia/Shanghai` 时区，严格晚于 `start_time`）。
    #[cfg_attr(feature = "schemars", schemars(with = "crate::schema::ZonedDateTime"))]
    pub end_time: ZonedDateTime,
    /// 起始节次（从 1 开始，不限 10 节）。
    pub start_unit: u8,
    /// 结束节次（不小于 `start_unit`）。
    pub end_unit: u8,
    /// 起始节次展示名称（如 `"1"`、`"第+1节"`）。
    #[serde(default)]
    pub start_unit_name_zh: Option<String>,
    /// 结束节次展示名称。
    #[serde(default)]
    pub end_unit_name_zh: Option<String>,
    /// 本场次授课教师列表（多人合上时包含全部教师）。
    #[serde(default)]
    pub teachers: Vec<SessionTeacher>,
    /// 固定上课教室（无固定教室时为 `None`）。
    #[serde(default)]
    pub room: Option<Room>,
    /// 自定义上课地点（如实验室地址）。
    #[serde(default)]
    pub custom_place: Option<String>,
    /// 授课类型。
    pub kind: SessionKind,
    /// 执行状态（实验课通常为 `None`）。
    #[serde(default)]
    pub state: Option<SessionState>,
    /// 本场次占用的学时数。
    #[serde(default)]
    pub periods: Option<u32>,
    /// 实验项目详情（非实验项目排课时为 `None`）。
    #[serde(default)]
    pub experiment_project: Option<ExperimentProject>,
}

impl ClassSession {
    /// 返回上课地点文本：有固定教室时拼接校区、教学楼与教室名称，否则返回自定义地点。
    #[must_use]
    pub fn display_place(&self) -> Option<String> {
        match &self.room {
            Some(room) => Some(format!(
                "{} {} {}",
                room.campus.name_zh, room.building.name_zh, room.name_zh
            )),
            None => self.custom_place.clone(),
        }
    }

    fn validate_fields(&self) -> Result<()> {
        if !(1..=7).contains(&self.weekday) {
            return Err(Error::decode_message("星期序号必须在 1..=7 范围内".into()));
        }
        if self.week_index == 0 {
            return Err(Error::decode_message("教学周序号必须大于 0".into()));
        }
        if self.start_unit == 0 || self.start_unit > self.end_unit {
            return Err(Error::decode_message("课程节次范围无效".into()));
        }
        if self.start_time.timestamp() >= self.end_time.timestamp() {
            return Err(Error::decode_message("课程结束时间必须晚于开始时间".into()));
        }
        Ok(())
    }

    fn sort_key(&self) -> (Date, i128, i128, u8, i64, i64, Option<i64>) {
        (
            self.date,
            self.start_time.timestamp().as_nanosecond(),
            self.end_time.timestamp().as_nanosecond(),
            self.start_unit,
            self.class_id,
            self.schedule_group_id,
            self.experiment_project.as_ref().map(|project| project.id),
        )
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawClassSession {
    class_id: i64,
    schedule_group_id: i64,
    week_index: u32,
    date: Date,
    #[serde(default)]
    original_date: Option<Date>,
    weekday: u8,
    start_time: ZonedDateTime,
    end_time: ZonedDateTime,
    start_unit: u8,
    end_unit: u8,
    #[serde(default)]
    start_unit_name_zh: Option<String>,
    #[serde(default)]
    end_unit_name_zh: Option<String>,
    #[serde(default)]
    teachers: Vec<SessionTeacher>,
    #[serde(default)]
    room: Option<Room>,
    #[serde(default)]
    custom_place: Option<String>,
    kind: SessionKind,
    #[serde(default)]
    state: Option<SessionState>,
    #[serde(default)]
    periods: Option<u32>,
    #[serde(default)]
    experiment_project: Option<ExperimentProject>,
}

impl TryFrom<RawClassSession> for ClassSession {
    type Error = Error;

    fn try_from(raw: RawClassSession) -> Result<Self> {
        let session = Self {
            class_id: raw.class_id,
            schedule_group_id: raw.schedule_group_id,
            week_index: raw.week_index,
            date: raw.date,
            original_date: raw.original_date,
            weekday: raw.weekday,
            start_time: raw.start_time,
            end_time: raw.end_time,
            start_unit: raw.start_unit,
            end_unit: raw.end_unit,
            start_unit_name_zh: trim_non_empty(raw.start_unit_name_zh),
            end_unit_name_zh: trim_non_empty(raw.end_unit_name_zh),
            teachers: raw.teachers,
            room: raw.room,
            custom_place: trim_non_empty(raw.custom_place),
            kind: raw.kind,
            state: raw.state,
            periods: raw.periods,
            experiment_project: raw.experiment_project,
        };
        session.validate_fields()?;
        Ok(session)
    }
}

/// 单个学期的完整课表数据。
///
/// 包含与当前选课匹配的每日节次作息、全部教学班以及按上课时间升序排列的排课场次；
/// 每条 [`ClassSession::class_id`] 均对应 [`Self::classes`] 中的教学班。
/// 按周查询、冲突检测与日历导出均在本地完成，不发起网络请求。
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(!try_from)
)]
#[serde(rename_all = "camelCase", try_from = "RawTimetable")]
pub struct Timetable {
    /// 课表所属学期校历。
    pub semester: Semester,
    /// 本课表适用的每日节次作息列表（按节次序号升序排列）。
    pub units: Vec<CourseUnit>,
    /// 本学期全部选修教学班（包含未排课课程与实验父班）。
    pub classes: Vec<TeachingClass>,
    /// 本学期全部排课场次（按上课时间升序排列）。
    pub sessions: Vec<ClassSession>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTimetable {
    semester: Semester,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    units: Vec<CourseUnit>,
    classes: Vec<TeachingClass>,
    sessions: Vec<ClassSession>,
}

impl TryFrom<RawTimetable> for Timetable {
    type Error = Error;

    fn try_from(mut raw: RawTimetable) -> Result<Self> {
        let units = normalize_course_units(raw.units)?;
        let mut class_ids = HashSet::with_capacity(raw.classes.len());
        for class in &raw.classes {
            if !class_ids.insert(class.id) {
                return Err(Error::decode_message(format!(
                    "课表中存在重复的教学班 ID：{}",
                    class.id
                )));
            }
        }
        for session in &raw.sessions {
            session.validate_fields()?;
            if !class_ids.contains(&session.class_id) {
                return Err(Error::decode_message(format!(
                    "排课记录引用了不存在的教学班 ID：{}",
                    session.class_id
                )));
            }
        }
        raw.sessions.sort_by_key(ClassSession::sort_key);
        Ok(Self {
            semester: raw.semester,
            units,
            classes: raw.classes,
            sessions: raw.sessions,
        })
    }
}

/// 关联教学班与单次排课的只读引用视图。
#[derive(Clone, Copy, Debug)]
pub struct SessionView<'a> {
    class: &'a TeachingClass,
    session: &'a ClassSession,
}

impl<'a> SessionView<'a> {
    /// 返回所属教学班引用。
    #[must_use]
    pub fn class(&self) -> &'a TeachingClass {
        self.class
    }

    /// 返回所属课程元数据引用。
    #[must_use]
    pub fn course(&self) -> &'a Course {
        &self.class.course
    }

    /// 返回单次排课记录引用。
    #[must_use]
    pub fn session(&self) -> &'a ClassSession {
        self.session
    }

    /// 返回用于展示的课程标题。
    ///
    /// 实验项目排课返回 `"课程名 - 实验项目名"`（两者相同时不重复拼接）；常规课程返回课程中文名。
    #[must_use]
    pub fn display_title(&self) -> Cow<'a, str> {
        match &self.session.experiment_project {
            Some(project) if project.name_zh != self.class.course.name_zh => Cow::Owned(format!(
                "{} - {}",
                self.class.course.name_zh, project.name_zh
            )),
            _ => Cow::Borrowed(&self.class.course.name_zh),
        }
    }

    /// 返回用于展示的上课地点文本。
    #[must_use]
    pub fn display_place(&self) -> Option<String> {
        self.session.display_place()
    }
}

/// 同一时间段内发生重叠的排课冲突。
#[derive(Clone, Debug)]
pub struct SessionConflict<'a> {
    /// 冲突重叠区间的开始时间（含）。
    pub start_time: ZonedDateTime,
    /// 冲突重叠区间的结束时间（不含）。
    pub end_time: ZonedDateTime,
    /// 在 `[start_time, end_time)` 内同时重叠的全部排课视图。
    pub sessions: Vec<SessionView<'a>>,
}

/// 指定教学周的课表只读视图。
#[derive(Clone, Copy, Debug)]
pub struct WeekView<'a> {
    timetable: &'a Timetable,
    week_index: u32,
}

impl<'a> WeekView<'a> {
    /// 返回本视图对应的教学周序号。
    #[must_use]
    pub fn week_index(&self) -> u32 {
        self.week_index
    }

    /// 返回本教学周在校历中对应的完整 7 天日期闭区间；超出校历范围时返回 `None`。
    #[must_use]
    pub fn date_range(&self) -> Option<(Date, Date)> {
        self.timetable.semester.week_date_range(self.week_index)
    }

    /// 返回本周排课的最大节次；无课时返回 `0`。
    #[must_use]
    pub fn max_unit(&self) -> u8 {
        self.timetable
            .sessions
            .iter()
            .filter(|session| session.week_index == self.week_index)
            .map(|session| session.end_unit)
            .max()
            .unwrap_or(0)
    }

    /// 返回本周全部排课视图（按时间升序排列）。
    #[must_use]
    pub fn sessions(&self) -> Vec<SessionView<'a>> {
        self.timetable
            .sessions
            .iter()
            .filter(|session| session.week_index == self.week_index)
            .filter_map(|session| self.timetable.view(session))
            .collect()
    }

    /// 返回本周指定星期（`1..=7`）的全部排课视图。
    ///
    /// # Errors
    ///
    /// `weekday` 不在 `1..=7` 范围内时返回 [`Error::InvalidInput`]。
    pub fn day(&self, weekday: u8) -> Result<Vec<SessionView<'a>>> {
        validate_weekday(weekday)?;
        Ok(self
            .timetable
            .sessions
            .iter()
            .filter(|session| session.week_index == self.week_index && session.weekday == weekday)
            .filter_map(|session| self.timetable.view(session))
            .collect())
    }

    /// 返回本周指定星期（`1..=7`）与节次（`>= 1`）上全部重叠的排课视图。
    ///
    /// # Errors
    ///
    /// `weekday` 不在 `1..=7` 范围内或 `unit == 0` 时返回 [`Error::InvalidInput`]。
    pub fn at(&self, weekday: u8, unit: u8) -> Result<Vec<SessionView<'a>>> {
        validate_weekday(weekday)?;
        if unit == 0 {
            return Err(Error::invalid_input("节次从 1 开始".into()));
        }
        Ok(self
            .timetable
            .sessions
            .iter()
            .filter(|session| {
                session.week_index == self.week_index
                    && session.weekday == weekday
                    && (session.start_unit..=session.end_unit).contains(&unit)
            })
            .filter_map(|session| self.timetable.view(session))
            .collect())
    }

    /// 检测本周内按真实时间区间 `[start_time, end_time)` 重叠的全部排课冲突。
    #[must_use]
    pub fn conflicts(&self) -> Vec<SessionConflict<'a>> {
        let sessions = self.sessions();
        detect_conflicts(&sessions)
    }
}

impl Timetable {
    /// 按节次序号（从 1 开始）查找对应的作息时间定义。
    #[must_use]
    pub fn unit(&self, index: u8) -> Option<&CourseUnit> {
        self.units.iter().find(|unit| unit.index == index)
    }

    /// 按教学班 ID 查找教学班元数据。
    #[must_use]
    pub fn class(&self, class_id: i64) -> Option<&TeachingClass> {
        self.classes.iter().find(|class| class.id == class_id)
    }

    /// 返回尚未排课的教学班列表。
    #[must_use]
    pub fn unscheduled_classes(&self) -> Vec<&TeachingClass> {
        let scheduled: HashSet<i64> = self
            .sessions
            .iter()
            .map(|session| session.class_id)
            .collect();
        self.classes
            .iter()
            .filter(|class| !scheduled.contains(&class.id))
            .collect()
    }

    /// 汇总本学期所有教学班的学分总和（忽略学分为 `None` 的条目）。
    #[must_use]
    pub fn total_credits(&self) -> f64 {
        self.classes
            .iter()
            .filter_map(|class| class.course.credits)
            .sum()
    }

    /// 计算公历日期 `date` 在本学期中对应的教学周序号。
    #[must_use]
    pub fn week_of(&self, date: Date) -> Option<u32> {
        self.semester.week_of(date)
    }

    /// 返回指定教学周（`week_index >= 1`）的只读视图。
    ///
    /// # Errors
    ///
    /// `week_index == 0` 时返回 [`Error::InvalidInput`]。
    pub fn week(&self, week_index: u32) -> Result<WeekView<'_>> {
        if week_index == 0 {
            return Err(Error::invalid_input("教学周从 1 开始".into()));
        }
        Ok(WeekView {
            timetable: self,
            week_index,
        })
    }

    /// 返回本课表涉及的全部教学周序号（校历周次与实际排课周次的升序并集）。
    #[must_use]
    pub fn week_indices(&self) -> Vec<u32> {
        let mut weeks: BTreeSet<u32> = self.semester.week_indices.iter().copied().collect();
        weeks.extend(self.sessions.iter().map(|session| session.week_index));
        weeks.into_iter().collect()
    }

    /// 返回指定公历日期的全部排课视图（按时间升序排列）。
    #[must_use]
    pub fn sessions_on(&self, date: Date) -> Vec<SessionView<'_>> {
        self.sessions
            .iter()
            .filter(|session| session.date == date)
            .filter_map(|session| self.view(session))
            .collect()
    }

    /// 返回与半开时间区间 `[start, end)` 存在重叠的全部排课视图。
    ///
    /// 相邻边界（前一场次结束时刻恰好等于 `start`）不视为重叠。
    ///
    /// # Errors
    ///
    /// `start` 不早于 `end` 时返回 [`Error::InvalidInput`]。
    pub fn sessions_between(
        &self,
        start: &ZonedDateTime,
        end: &ZonedDateTime,
    ) -> Result<Vec<SessionView<'_>>> {
        let start = start.timestamp();
        let end = end.timestamp();
        if start >= end {
            return Err(Error::invalid_input("查询结束时间必须晚于开始时间".into()));
        }
        Ok(self
            .sessions
            .iter()
            .filter(|session| {
                session.start_time.timestamp() < end && session.end_time.timestamp() > start
            })
            .filter_map(|session| self.view(session))
            .collect())
    }

    /// 返回在时刻 `at` 正在进行（`start_time <= at < end_time`）的全部排课视图。
    #[must_use]
    pub fn current_sessions(&self, at: &ZonedDateTime) -> Vec<SessionView<'_>> {
        let now = at.timestamp();
        self.sessions
            .iter()
            .filter(|session| {
                session.start_time.timestamp() <= now && now < session.end_time.timestamp()
            })
            .filter_map(|session| self.view(session))
            .collect()
    }

    /// 返回开始时间严格晚于 `after` 的下一场排课视图。
    #[must_use]
    pub fn next_session(&self, after: &ZonedDateTime) -> Option<SessionView<'_>> {
        let after = after.timestamp();
        self.sessions
            .iter()
            .find(|session| session.start_time.timestamp() > after)
            .and_then(|session| self.view(session))
    }

    /// 检测整张课表内按真实时间区间 `[start_time, end_time)` 重叠的全部排课冲突。
    #[must_use]
    pub fn conflicts(&self) -> Vec<SessionConflict<'_>> {
        let all: Vec<_> = self
            .sessions
            .iter()
            .filter_map(|session| self.view(session))
            .collect();
        detect_conflicts(&all)
    }

    pub(super) fn view<'a>(&'a self, session: &'a ClassSession) -> Option<SessionView<'a>> {
        self.class(session.class_id)
            .map(|class| SessionView { class, session })
    }
}

fn validate_weekday(weekday: u8) -> Result<()> {
    if !(1..=7).contains(&weekday) {
        return Err(Error::invalid_input("星期序号必须在 1..=7 范围内".into()));
    }
    Ok(())
}

/// 按时间区间切分重叠片段，避免把不直接冲突的相邻课程合并到同一组。
fn detect_conflicts<'a>(sessions: &[SessionView<'a>]) -> Vec<SessionConflict<'a>> {
    if sessions.len() < 2 {
        return Vec::new();
    }
    let mut points: BTreeMap<i128, ZonedDateTime> = BTreeMap::new();
    for view in sessions {
        let start = &view.session.start_time;
        let end = &view.session.end_time;
        points
            .entry(start.timestamp().as_nanosecond())
            .or_insert_with(|| start.clone());
        points
            .entry(end.timestamp().as_nanosecond())
            .or_insert_with(|| end.clone());
    }
    let timeline: Vec<(i128, ZonedDateTime)> = points.into_iter().collect();
    let mut conflicts: Vec<SessionConflict<'a>> = Vec::new();

    for window in timeline.windows(2) {
        let (seg_start_ns, seg_start) = &window[0];
        let (seg_end_ns, seg_end) = &window[1];
        let active: Vec<SessionView<'a>> = sessions
            .iter()
            .copied()
            .filter(|view| {
                let s_start = view.session.start_time.timestamp().as_nanosecond();
                let s_end = view.session.end_time.timestamp().as_nanosecond();
                s_start <= *seg_start_ns && s_end >= *seg_end_ns
            })
            .collect();
        if active.len() < 2 {
            continue;
        }
        // 相邻片段的冲突课程完全相同时，合并为一个连续区间。
        if let Some(last) = conflicts.last_mut()
            && last.end_time.timestamp().as_nanosecond() == *seg_start_ns
            && same_session_set(&last.sessions, &active)
        {
            last.end_time = seg_end.clone();
        } else {
            conflicts.push(SessionConflict {
                start_time: seg_start.clone(),
                end_time: seg_end.clone(),
                sessions: active,
            });
        }
    }
    conflicts
}

fn same_session_set(left: &[SessionView<'_>], right: &[SessionView<'_>]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| std::ptr::eq(a.session, b.session))
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawNamedEntity {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    name_zh: String,
    #[serde(default)]
    name_en: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    code: String,
}

impl RawNamedEntity {
    fn into_named_entity(self) -> Option<NamedEntity> {
        let id = self.id?;
        Some(NamedEntity {
            id,
            name_zh: self.name_zh,
            name_en: trim_non_empty(self.name_en),
            code: self.code,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RawCourseTableEntry {
    id: i64,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    code: String,
    #[serde(default)]
    biz_type_id: Option<i64>,
    #[serde(default)]
    time_table_layout_assoc: Option<i64>,
    course: RawCourse,
    #[serde(default)]
    course_type: Option<RawNamedEntity>,
    #[serde(default)]
    cultivate_type: Option<RawNamedEntity>,
    #[serde(default)]
    open_department: Option<RawNamedEntity>,
    #[serde(default)]
    campus: Option<RawNamedEntity>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    teacher_assignment_list: Vec<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    teacher_assignment_en_list: Vec<Option<String>>,
    #[serde(default)]
    std_count: Option<u32>,
    #[serde(default)]
    schedule_state: Option<ScheduleState>,
    #[serde(default)]
    schedule_text: Option<RawScheduleText>,
    #[serde(default)]
    remark: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    schedules: Vec<RawUpstreamSchedule>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCourse {
    id: i64,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    code: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    name_zh: String,
    #[serde(default)]
    name_en: Option<String>,
    #[serde(default)]
    credits: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    period_info: RawPeriodInfo,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPeriodInfo {
    #[serde(default, deserialize_with = "deserialize_null_default")]
    total: u32,
    #[serde(default)]
    weeks: Option<u32>,
    #[serde(default)]
    periods_per_week: Option<u32>,
    #[serde(default)]
    theory: Option<u32>,
    #[serde(default)]
    experiment: Option<u32>,
    #[serde(default)]
    practice: Option<u32>,
    #[serde(default)]
    design: Option<u32>,
    #[serde(default)]
    machine: Option<u32>,
    #[serde(default)]
    test: Option<u32>,
}

impl From<RawPeriodInfo> for PeriodInfo {
    fn from(raw: RawPeriodInfo) -> Self {
        Self {
            total: raw.total,
            weeks: raw.weeks,
            periods_per_week: raw.periods_per_week,
            theory: raw.theory,
            experiment: raw.experiment,
            practice: raw.practice,
            design: raw.design,
            machine: raw.machine,
            test: raw.test,
        }
    }
}

#[derive(Deserialize)]
struct RawScheduleText {
    #[serde(default, rename = "dateTimeText")]
    time_only: Option<RawLocalizedText>,
    #[serde(default, rename = "dateTimePlaceText")]
    time_and_place: Option<RawLocalizedText>,
    #[serde(default, rename = "dateTimePlacePersonText")]
    full: Option<RawLocalizedText>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLocalizedText {
    #[serde(default)]
    text_zh: Option<String>,
    #[serde(default)]
    text_en: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawUpstreamSchedule {
    schedule_group_id: i64,
    date: Date,
    #[serde(default)]
    original_date: Option<Date>,
    weekday: u8,
    start_time: Value,
    end_time: Value,
    #[serde(default)]
    real_start_time: Option<Value>,
    #[serde(default)]
    real_end_time: Option<Value>,
    #[serde(default)]
    teacher_id: Option<i64>,
    #[serde(default)]
    person_id: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    teacher_name: String,
    #[serde(default)]
    teacher_name_en: Option<String>,
    #[serde(default)]
    custom_place: Option<String>,
    #[serde(default)]
    room: Option<Room>,
    start_unit: u8,
    end_unit: u8,
    #[serde(default)]
    start_unit_name_zh: Option<String>,
    #[serde(default)]
    end_unit_name_zh: Option<String>,
    #[serde(default)]
    state: Option<SessionState>,
    week_index: u32,
    #[serde(default)]
    lesson_type: Option<SessionKind>,
    #[serde(default)]
    periods: Option<u32>,
    #[serde(default)]
    exp_project_name_zh: Option<String>,
    #[serde(default)]
    exp_course_code_or_name_zh_str: Option<String>,
}

#[derive(Eq, PartialEq, Hash)]
struct SessionMergeKey {
    class_id: i64,
    schedule_group_id: i64,
    date: Date,
    start_ns: i128,
    end_ns: i128,
    start_unit: u8,
    end_unit: u8,
    room_id: Option<i64>,
    custom_place: Option<String>,
    experiment_project_id: Option<i64>,
}

/// 将上游 `course-table` 与全局节次作息响应整理为 [`Timetable`]。
pub(super) fn build_timetable(
    semester: Semester,
    entries: Vec<RawCourseTableEntry>,
    unit_layouts: BTreeMap<String, Vec<CourseUnit>>,
) -> Result<Timetable> {
    let referenced_layouts: BTreeSet<String> = entries
        .iter()
        .filter_map(|entry| entry.time_table_layout_assoc.map(|id| id.to_string()))
        .collect();
    let (experiment_entries, regular_entries): (Vec<_>, Vec<_>) =
        entries.into_iter().partition(is_synthetic_experiment);

    let mut classes: Vec<TeachingClass> = Vec::with_capacity(regular_entries.len());
    let mut class_id_set: HashSet<i64> = HashSet::with_capacity(regular_entries.len());
    let mut code_to_class_id: HashMap<String, i64> = HashMap::new();
    let mut name_to_class_ids: HashMap<String, Vec<(i64, bool)>> = HashMap::new();
    let mut merged_sessions: Vec<ClassSession> = Vec::new();
    let mut session_positions: HashMap<SessionMergeKey, usize> = HashMap::new();

    for mut entry in regular_entries {
        if !class_id_set.insert(entry.id) {
            return Err(Error::decode_message(format!(
                "教务接口返回了重复的教学班 ID：{}",
                entry.id
            )));
        }
        let has_experiment_hours = entry.course.period_info.experiment.is_some();
        if !entry.code.trim().is_empty() {
            code_to_class_id.insert(entry.code.trim().to_owned(), entry.id);
        }
        name_to_class_ids
            .entry(entry.course.name_zh.trim().to_owned())
            .or_default()
            .push((entry.id, has_experiment_hours));

        let class_id = entry.id;
        let schedules = std::mem::take(&mut entry.schedules);
        classes.push(convert_regular_class(entry));

        for schedule in schedules {
            let session = normalize_schedule(class_id, schedule, None)?;
            merge_session(&mut merged_sessions, &mut session_positions, session);
        }
    }

    // 将实验系统的合成项目条目关联回父教学班，无父班时按上游负 ID 归并。
    let mut orphan_group_to_class_id: HashMap<(i64, String), i64> = HashMap::new();
    let mut orphan_seen_projects: HashMap<i64, HashSet<i64>> = HashMap::new();
    let mut next_fallback_id: i64 = -1;

    for mut entry in experiment_entries {
        let schedules = std::mem::take(&mut entry.schedules);
        let project = ExperimentProject {
            id: entry.course.id,
            code: entry.course.code.clone(),
            name_zh: entry.course.name_zh.clone(),
            total_periods: (entry.course.period_info.total > 0)
                .then_some(entry.course.period_info.total),
        };
        for schedule in schedules {
            let reference = schedule
                .exp_course_code_or_name_zh_str
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty());
            let class_id =
                resolve_experiment_parent(reference, &code_to_class_id, &name_to_class_ids)
                    .unwrap_or_else(|| {
                        let orphan_id = ensure_orphan_class(
                            &entry,
                            reference,
                            &mut classes,
                            &mut class_id_set,
                            &mut orphan_group_to_class_id,
                            &mut next_fallback_id,
                        );
                        orphan_seen_projects.entry(orphan_id).or_default();
                        orphan_id
                    });

            // 独立实验班按实验项目去重累计总学时。
            if let Some(seen_projects) = orphan_seen_projects.get_mut(&class_id)
                && seen_projects.insert(project.id)
                && let Some(periods) = project.total_periods
                && let Some(class) = classes.iter_mut().find(|class| class.id == class_id)
            {
                class.course.period_info.total =
                    class.course.period_info.total.saturating_add(periods);
            }

            let mut item_project = project.clone();
            if let Some(name) = trim_non_empty(schedule.exp_project_name_zh.clone()) {
                item_project.name_zh = name;
            }
            let session = normalize_schedule(class_id, schedule, Some(item_project))?;
            merge_session(&mut merged_sessions, &mut session_positions, session);
        }
    }

    merged_sessions.sort_by_key(ClassSession::sort_key);
    let units = resolve_timetable_units(unit_layouts, &referenced_layouts, &merged_sessions)?;
    Ok(Timetable {
        semester,
        units,
        classes,
        sessions: merged_sessions,
    })
}

pub(super) fn normalize_unit_layouts(
    layouts: BTreeMap<String, Vec<CourseUnit>>,
) -> Result<BTreeMap<String, Vec<CourseUnit>>> {
    let mut normalized: BTreeMap<String, Vec<CourseUnit>> = BTreeMap::new();
    for (key, units) in layouts {
        normalized.insert(key, normalize_course_units(units)?);
    }
    Ok(normalized)
}

/// 根据选课引用的作息布局 ID 与实际排课场次，选取最匹配的每日节次作息表。
fn resolve_timetable_units(
    layouts: BTreeMap<String, Vec<CourseUnit>>,
    referenced_layouts: &BTreeSet<String>,
    sessions: &[ClassSession],
) -> Result<Vec<CourseUnit>> {
    let max_session_unit = sessions
        .iter()
        .map(|session| session.end_unit)
        .max()
        .unwrap_or(0);
    let normalized = normalize_unit_layouts(layouts)?;

    // 优先从教学班显式引用的作息布局中选取节次覆盖最完整的一套（兼容跨院系混选）。
    let matched = referenced_layouts
        .iter()
        .filter_map(|key| normalized.get(key))
        .max_by_key(|units| (units.last().map_or(0, |unit| unit.index), units.len()))
        .cloned();
    if let Some(units) = matched {
        return Ok(units);
    }

    // 无显式布局关联（如空课表或纯独立实验课）时，若默认 10 节布局足以覆盖排课节次则优先使用。
    if let Some(default_units) = normalized.get("1") {
        let default_max = default_units.last().map_or(0, |unit| unit.index);
        if max_session_unit <= default_max {
            return Ok(default_units.clone());
        }
    }

    // 排课节次超出默认 10 节时，取能覆盖 max_session_unit 的最小布局，否则回退至最完整布局。
    if let Some(covering) = normalized
        .values()
        .filter(|units| units.last().map_or(0, |unit| unit.index) >= max_session_unit)
        .min_by_key(|units| (units.last().map_or(0, |unit| unit.index), units.len()))
    {
        return Ok(covering.clone());
    }

    Ok(normalized
        .into_values()
        .max_by_key(|units| (units.last().map_or(0, |unit| unit.index), units.len()))
        .unwrap_or_default())
}

fn is_synthetic_experiment(entry: &RawCourseTableEntry) -> bool {
    entry.id < 0
        || (entry.biz_type_id.is_none()
            && entry.schedules.iter().any(|schedule| {
                schedule.exp_project_name_zh.is_some()
                    || schedule.exp_course_code_or_name_zh_str.is_some()
            }))
}

fn convert_regular_class(entry: RawCourseTableEntry) -> TeachingClass {
    let teachers = zip_teachers(
        entry.teacher_assignment_list,
        entry.teacher_assignment_en_list,
    );
    let (schedule_text, schedule_text_en) = extract_schedule_text(entry.schedule_text);
    TeachingClass {
        id: entry.id,
        code: entry.code,
        origin: ClassOrigin::Regular,
        course: Course {
            id: entry.course.id,
            code: entry.course.code,
            name_zh: entry.course.name_zh,
            name_en: trim_non_empty(entry.course.name_en),
            credits: entry.course.credits,
            period_info: entry.course.period_info.into(),
        },
        course_type: entry
            .course_type
            .and_then(RawNamedEntity::into_named_entity),
        cultivate_type: entry
            .cultivate_type
            .and_then(RawNamedEntity::into_named_entity),
        department: entry
            .open_department
            .and_then(RawNamedEntity::into_named_entity),
        campus: entry.campus.and_then(RawNamedEntity::into_named_entity),
        teachers,
        student_count: entry.std_count,
        schedule_state: entry.schedule_state.unwrap_or(ScheduleState::Published),
        schedule_text,
        schedule_text_en,
        remark: trim_non_empty(entry.remark),
    }
}

fn zip_teachers(zh_list: Vec<Option<String>>, en_list: Vec<Option<String>>) -> Vec<TeacherName> {
    let mut en_iter = en_list.into_iter();
    let mut teachers = Vec::with_capacity(zh_list.len());
    for zh in zh_list {
        let en = trim_non_empty(en_iter.next().flatten());
        if let Some(name_zh) = trim_non_empty(zh) {
            teachers.push(TeacherName {
                name_zh,
                name_en: en,
            });
        }
    }
    teachers
}

fn extract_schedule_text(text: Option<RawScheduleText>) -> (Option<String>, Option<String>) {
    let Some(text) = text else {
        return (None, None);
    };
    let candidates = [text.full, text.time_and_place, text.time_only];
    let zh = candidates.iter().find_map(|item| {
        item.as_ref()
            .and_then(|value| trim_non_empty(value.text_zh.clone()))
    });
    let en = candidates.iter().find_map(|item| {
        item.as_ref()
            .and_then(|value| trim_non_empty(value.text_en.clone()))
    });
    (zh, en)
}

fn parse_experiment_reference(reference: &str) -> (Option<&str>, &str) {
    if let Some(rest) = reference.strip_prefix('[')
        && let Some((code, name)) = rest.split_once(']')
    {
        let code = code.trim();
        let name = name.trim();
        return ((!code.is_empty()).then_some(code), name);
    }
    (None, reference.trim())
}

fn resolve_experiment_parent(
    reference: Option<&str>,
    code_to_class_id: &HashMap<String, i64>,
    name_to_class_ids: &HashMap<String, Vec<(i64, bool)>>,
) -> Option<i64> {
    let reference = reference?;
    let (code, name) = parse_experiment_reference(reference);
    // 优先按选课课号精确匹配父教学班。
    if let Some(code) = code
        && let Some(&class_id) = code_to_class_id.get(code)
    {
        return Some(class_id);
    }
    // 其次按课程中文名匹配唯一常规教学班，有重名时优先选带实验学时的班级。
    if let Some(candidates) = name_to_class_ids.get(name) {
        if candidates.len() == 1 {
            return Some(candidates[0].0);
        }
        let experiment_only: Vec<i64> = candidates
            .iter()
            .filter_map(|&(id, is_exp)| is_exp.then_some(id))
            .collect();
        if experiment_only.len() == 1 {
            return Some(experiment_only[0]);
        }
    }
    None
}

fn ensure_orphan_class(
    entry: &RawCourseTableEntry,
    reference: Option<&str>,
    classes: &mut Vec<TeachingClass>,
    class_id_set: &mut HashSet<i64>,
    orphan_group_to_class_id: &mut HashMap<(i64, String), i64>,
    next_fallback_id: &mut i64,
) -> i64 {
    let (parsed_code, parsed_name) = reference.map_or((None, ""), parse_experiment_reference);
    let group_code = parsed_code.unwrap_or(entry.code.as_str()).to_owned();
    let group_key = (entry.id, group_code.clone());
    if let Some(&existing_id) = orphan_group_to_class_id.get(&group_key) {
        return existing_id;
    }

    let class_id = if class_id_set.insert(entry.id) {
        entry.id
    } else {
        while !class_id_set.insert(*next_fallback_id) {
            *next_fallback_id -= 1;
        }
        let allocated = *next_fallback_id;
        *next_fallback_id -= 1;
        allocated
    };

    let course_name = if parsed_name.is_empty() {
        entry.course.name_zh.clone()
    } else {
        parsed_name.to_owned()
    };
    classes.push(TeachingClass {
        id: class_id,
        code: group_code.clone(),
        origin: ClassOrigin::StandaloneExperiment,
        course: Course {
            id: class_id,
            code: group_code,
            name_zh: course_name,
            name_en: None,
            credits: None,
            period_info: PeriodInfo::default(),
        },
        course_type: entry
            .course_type
            .clone()
            .and_then(RawNamedEntity::into_named_entity),
        cultivate_type: entry
            .cultivate_type
            .clone()
            .and_then(RawNamedEntity::into_named_entity),
        department: entry
            .open_department
            .clone()
            .and_then(RawNamedEntity::into_named_entity),
        campus: entry
            .campus
            .clone()
            .and_then(RawNamedEntity::into_named_entity),
        teachers: Vec::new(),
        student_count: entry.std_count,
        schedule_state: entry
            .schedule_state
            .clone()
            .unwrap_or(ScheduleState::Published),
        schedule_text: None,
        schedule_text_en: None,
        remark: trim_non_empty(entry.remark.clone()),
    });
    orphan_group_to_class_id.insert(group_key, class_id);
    class_id
}

fn normalize_schedule(
    class_id: i64,
    raw: RawUpstreamSchedule,
    experiment_project: Option<ExperimentProject>,
) -> Result<ClassSession> {
    let schedule_group_id = raw.schedule_group_id;
    normalize_schedule_inner(class_id, raw, experiment_project).map_err(|error| {
        error.map_message(|message| {
            format!(
                "排课场次解析失败（classId={class_id}, scheduleGroupId={schedule_group_id}）：{message}"
            )
        })
    })
}

fn normalize_schedule_inner(
    class_id: i64,
    raw: RawUpstreamSchedule,
    experiment_project: Option<ExperimentProject>,
) -> Result<ClassSession> {
    let start_time =
        resolve_effective_time(raw.date, &raw.start_time, raw.real_start_time.as_ref())?;
    let end_time = resolve_effective_time(raw.date, &raw.end_time, raw.real_end_time.as_ref())?;

    let mut teachers = Vec::new();
    if let Some(name_zh) = trim_non_empty(Some(raw.teacher_name)) {
        teachers.push(SessionTeacher {
            id: raw.teacher_id,
            person_id: raw.person_id,
            name_zh,
            name_en: trim_non_empty(raw.teacher_name_en),
        });
    }

    let kind = match (raw.lesson_type, experiment_project.is_some()) {
        (Some(kind), _) => kind,
        (None, true) => SessionKind::Experiment,
        (None, false) => SessionKind::Other("unknown".into()),
    };

    let session = ClassSession {
        class_id,
        schedule_group_id: raw.schedule_group_id,
        week_index: raw.week_index,
        date: raw.date,
        original_date: raw.original_date,
        weekday: raw.weekday,
        start_time,
        end_time,
        start_unit: raw.start_unit,
        end_unit: raw.end_unit,
        start_unit_name_zh: trim_non_empty(raw.start_unit_name_zh),
        end_unit_name_zh: trim_non_empty(raw.end_unit_name_zh),
        teachers,
        room: raw.room,
        custom_place: trim_non_empty(raw.custom_place),
        kind,
        state: raw.state,
        periods: raw.periods,
        experiment_project,
    };
    session.validate_fields()?;
    Ok(session)
}

fn resolve_effective_time(
    date: Date,
    nominal: &Value,
    actual: Option<&Value>,
) -> Result<ZonedDateTime> {
    // 实验课排课以 `0` 表示未单独指定实际时间，回退到标准时间。
    let preferred = actual.filter(|value| value.as_i64() != Some(0));
    parse_time(date, preferred.unwrap_or(nominal))
}

fn merge_session(
    sessions: &mut Vec<ClassSession>,
    positions: &mut HashMap<SessionMergeKey, usize>,
    session: ClassSession,
) {
    let key = SessionMergeKey {
        class_id: session.class_id,
        schedule_group_id: session.schedule_group_id,
        date: session.date,
        start_ns: session.start_time.timestamp().as_nanosecond(),
        end_ns: session.end_time.timestamp().as_nanosecond(),
        start_unit: session.start_unit,
        end_unit: session.end_unit,
        room_id: session.room.as_ref().map(|room| room.id),
        custom_place: session.custom_place.clone(),
        experiment_project_id: session
            .experiment_project
            .as_ref()
            .map(|project| project.id),
    };

    if let Some(&index) = positions.get(&key) {
        let existing = &mut sessions[index];
        for teacher in session.teachers {
            if !existing
                .teachers
                .iter()
                .any(|item| item.name_zh == teacher.name_zh)
            {
                existing.teachers.push(teacher);
            }
        }
        return;
    }

    positions.insert(key, sessions.len());
    sessions.push(session);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_semester() -> Semester {
        serde_json::from_value(json!({
            "id": 194,
            "code": "2026-2027-1",
            "nameZh": "2026-2027-1",
            "nameEn": null,
            "schoolYear": "2026-2027",
            "season": "AUTUMN",
            "startDate": "2026-09-07",
            "endDate": "2027-01-17",
            "weekStartOnSunday": false,
            "countInTerm": true,
            "weekIndices": (1..=19).collect::<Vec<u32>>()
        }))
        .unwrap()
    }

    fn room(id: i64, name: &str) -> Value {
        json!({
            "id": id,
            "nameZh": name,
            "nameEn": null,
            "building": {"id": 23, "nameZh": "物理馆", "nameEn": null, "code": "物理馆"},
            "campus": {"id": 2, "nameZh": "主校区", "nameEn": null, "code": "01"},
            "seatNumber": null
        })
    }

    fn extended_unit_entry() -> RawCourseTableEntry {
        serde_json::from_value(json!({
            "id": 500_002,
            "code": "888001-001",
            "bizTypeId": 2,
            "timeTableLayoutAssoc": 2,
            "course": {
                "id": 88001, "code": "888001", "nameZh": "晚间加节研讨", "nameEn": null,
                "credits": 1, "periodInfo": {"total": 16}
            },
            "scheduleState": "published",
            "schedules": [{
                "scheduleGroupId": 700_001, "date": "2027-01-20", "originalDate": "2027-01-13",
                "weekday": 3, "startTime": 1955, "endTime": 2135,
                "realStartTime": 1955, "realEndTime": 2135,
                "teacherName": null, "room": null, "customPlace": "线上会议室",
                "startUnit": 13, "endUnit": 14,
                "startUnitNameZh": "第十节", "endUnitNameZh": "第+4节",
                "state": "已完成", "weekIndex": 20, "lessonType": "THEORY", "periods": 2
            }]
        }))
        .unwrap()
    }

    fn sample_unit_layouts() -> BTreeMap<String, Vec<CourseUnit>> {
        serde_json::from_value(json!({
            "1": [
                {"indexNo": 1, "nameZh": "1", "dayPart": "MORNING", "segmentIndex": 1, "startTime": 800, "endTime": 845},
                {"indexNo": 2, "nameZh": "2", "dayPart": "MORNING", "segmentIndex": 1, "startTime": 855, "endTime": 940},
                {"indexNo": 3, "nameZh": "3", "dayPart": "MORNING", "segmentIndex": 2, "startTime": 1010, "endTime": 1055},
                {"indexNo": 4, "nameZh": "4", "dayPart": "MORNING", "segmentIndex": 2, "startTime": 1105, "endTime": 1150},
                {"indexNo": 5, "nameZh": "5", "dayPart": "AFTERNOON", "segmentIndex": 3, "startTime": 1400, "endTime": 1445},
                {"indexNo": 6, "nameZh": "6", "dayPart": "AFTERNOON", "segmentIndex": 3, "startTime": 1455, "endTime": 1540},
                {"indexNo": 7, "nameZh": "7", "dayPart": "AFTERNOON", "segmentIndex": 4, "startTime": 1600, "endTime": 1645},
                {"indexNo": 8, "nameZh": "8", "dayPart": "AFTERNOON", "segmentIndex": 4, "startTime": 1655, "endTime": 1740},
                {"indexNo": 9, "nameZh": "9", "dayPart": "EVENING", "segmentIndex": 5, "startTime": 1900, "endTime": 1945},
                {"indexNo": 10, "nameZh": "10", "dayPart": "EVENING", "segmentIndex": 5, "startTime": 1955, "endTime": 2040}
            ],
            "2": [
                {"indexNo": 1, "nameZh": "第一节", "dayPart": "MORNING", "segmentIndex": 1, "startTime": 800, "endTime": 845},
                {"indexNo": 2, "nameZh": "第二节", "dayPart": "MORNING", "segmentIndex": 1, "startTime": 855, "endTime": 940},
                {"indexNo": 3, "nameZh": "第三节", "dayPart": "MORNING", "segmentIndex": 2, "startTime": 1010, "endTime": 1055},
                {"indexNo": 4, "nameZh": "第四节", "dayPart": "MORNING", "segmentIndex": 2, "startTime": 1105, "endTime": 1150},
                {"indexNo": 5, "nameZh": "第+1节", "dayPart": "NOON", "segmentIndex": 3, "startTime": 1200, "endTime": 1245},
                {"indexNo": 6, "nameZh": "第+2节", "dayPart": "NOON", "segmentIndex": 3, "startTime": 1255, "endTime": 1340},
                {"indexNo": 7, "nameZh": "第五节", "dayPart": "AFTERNOON", "segmentIndex": 4, "startTime": 1400, "endTime": 1445},
                {"indexNo": 8, "nameZh": "第六节", "dayPart": "AFTERNOON", "segmentIndex": 4, "startTime": 1455, "endTime": 1540},
                {"indexNo": 9, "nameZh": "第七节", "dayPart": "AFTERNOON", "segmentIndex": 5, "startTime": 1600, "endTime": 1645},
                {"indexNo": 10, "nameZh": "第八节", "dayPart": "AFTERNOON", "segmentIndex": 5, "startTime": 1655, "endTime": 1740},
                {"indexNo": 11, "nameZh": "第+3节", "dayPart": "AFTERNOON", "segmentIndex": 6, "startTime": 1750, "endTime": 1835},
                {"indexNo": 12, "nameZh": "第九节", "dayPart": "EVENING", "segmentIndex": 7, "startTime": 1900, "endTime": 1945},
                {"indexNo": 13, "nameZh": "第十节", "dayPart": "EVENING", "segmentIndex": 7, "startTime": 1955, "endTime": 2040},
                {"indexNo": 14, "nameZh": "第+4节", "dayPart": "EVENING", "segmentIndex": 8, "startTime": 2050, "endTime": 2135}
            ]
        }))
        .unwrap()
    }

    fn regular_only_raw_entries() -> Vec<RawCourseTableEntry> {
        serde_json::from_value(json!([
            {
                "id": 476_767, "code": "222302-013", "bizTypeId": 2, "timeTableLayoutAssoc": 1,
                "course": {
                    "id": 4171, "code": "222302", "nameZh": "大学物理B", "nameEn": "College Physics B",
                    "credits": 4,
                    "periodInfo": {"total": 64, "weeks": 16, "periodsPerWeek": 4, "theory": 64, "design": null}
                },
                "courseType": {"id": 53, "nameZh": "分类通识课", "nameEn": "Obligatory", "code": "2025002"},
                "cultivateType": {"id": 1, "nameZh": "主修", "nameEn": "Major", "code": "01"},
                "openDepartment": {"id": 14, "nameZh": "物理学院", "nameEn": null, "code": "22"},
                "campus": {"id": 2, "nameZh": "主校区", "nameEn": null, "code": "01"},
                "teacherAssignmentList": ["郜小勇"], "teacherAssignmentEnList": [null],
                "stdCount": 102, "scheduleState": "published", "remark": "27",
                "scheduleText": {
                    "dateTimePlacePersonText": {"textZh": "12周 星期三 1~2节 主校区 物理馆 物_209 郜小勇"}
                },
                "schedules": [{
                    "scheduleGroupId": 613_299, "date": "2026-11-25", "originalDate": null,
                    "weekday": 3, "startTime": 800, "endTime": 940,
                    "realStartTime": 800, "realEndTime": 940,
                    "teacherName": "郜小勇", "room": room(391, "物_209"),
                    "startUnit": 1, "endUnit": 2, "startUnitNameZh": "1", "endUnitNameZh": "2",
                    "state": "未完成", "weekIndex": 12, "lessonType": "THEORY", "periods": 2
                }]
            },
            {
                "id": 464_251, "code": "472022-002", "bizTypeId": 2, "timeTableLayoutAssoc": 1,
                "course": {
                    "id": 16848, "code": "472022", "nameZh": "生物化学 (I)", "nameEn": null,
                    "credits": 3, "periodInfo": {"total": 48, "weeks": 16, "periodsPerWeek": 3, "theory": 48}
                },
                "teacherAssignmentList": ["田庆南", "刘红涛"], "teacherAssignmentEnList": [null, null],
                "scheduleState": "published",
                "schedules": [
                    {
                        "scheduleGroupId": 600_028, "date": "2026-10-12", "weekday": 1,
                        "startTime": 1600, "endTime": 1740, "realStartTime": 1600, "realEndTime": 1740,
                        "teacherName": "刘红涛", "room": room(391, "物_209"),
                        "startUnit": 7, "endUnit": 8, "state": "未完成", "weekIndex": 6,
                        "lessonType": "THEORY", "periods": 2
                    },
                    {
                        "scheduleGroupId": 600_028, "date": "2026-10-12", "weekday": 1,
                        "startTime": 1600, "endTime": 1740, "realStartTime": 1600, "realEndTime": 1740,
                        "teacherName": "田庆南", "room": room(391, "物_209"),
                        "startUnit": 7, "endUnit": 8, "state": "未完成", "weekIndex": 6,
                        "lessonType": "THEORY", "periods": 2
                    },
                    {
                        "scheduleGroupId": 600_028, "date": "2026-11-25", "weekday": 3,
                        "startTime": 800, "endTime": 940, "realStartTime": 800, "realEndTime": 940,
                        "teacherName": "吴秀云", "room": room(423, "北2_305"),
                        "startUnit": 1, "endUnit": 2, "state": "未完成", "weekIndex": 12,
                        "lessonType": "THEORY", "periods": 2
                    }
                ]
            },
            {
                "id": 464_182, "code": "475047-002", "bizTypeId": 2, "timeTableLayoutAssoc": 1,
                "course": {
                    "id": 16856, "code": "475047", "nameZh": "LINUX操作系统上机实验", "nameEn": null,
                    "credits": 1, "periodInfo": {"total": 24, "weeks": 6, "periodsPerWeek": 4, "experiment": 24}
                },
                "teacherAssignmentList": ["杜江峰"], "scheduleState": "unpublish", "schedules": []
            },
            {
                "id": 500_001, "code": "999001-001", "bizTypeId": 2, "timeTableLayoutAssoc": 1,
                "course": {
                    "id": 99001, "code": "999001", "nameZh": "创新实践", "nameEn": null,
                    "credits": 2, "periodInfo": {"total": 32, "practice": 32}
                },
                "scheduleState": "unpublish", "schedules": []
            }
        ]))
        .unwrap()
    }

    fn sample_raw_entries() -> Vec<RawCourseTableEntry> {
        let mut entries = regular_only_raw_entries();
        entries.push(extended_unit_entry());
        entries
    }

    fn sample_experiment_entries() -> Vec<RawCourseTableEntry> {
        serde_json::from_value(json!([
            {
                "id": -4109,
                "code": "4701504703",
                "bizTypeId": null,
                "campus": {"id": null, "nameZh": "主校区", "nameEn": null, "code": null},
                "course": {
                    "id": -2629, "code": "4701504703", "nameZh": "3、bash脚本循环练习",
                    "credits": null, "periodInfo": {"total": 4}
                },
                "scheduleState": "published",
                "schedules": [
                    {
                        "scheduleGroupId": 24735, "date": "2026-10-25", "weekday": 7,
                        "startTime": 800, "endTime": 1150, "realStartTime": 0, "realEndTime": 0,
                        "teacherId": 4026, "personId": 285_854, "teacherName": "杜江峰",
                        "customPlace": "生物信息学实验室（生物馆楼305）", "room": null,
                        "startUnit": 1, "endUnit": 4, "state": null, "weekIndex": 7,
                        "lessonType": null, "periods": null,
                        "expCourseCodeOrNameZhStr": "[475047-002]LINUX操作系统上机实验",
                        "expProjectNameZh": "3、bash脚本循环练习"
                    },
                    {
                        "scheduleGroupId": 24735, "date": "2026-10-11", "weekday": 7,
                        "startTime": 800, "endTime": 1150, "realStartTime": 0, "realEndTime": 0,
                        "teacherId": 4026, "personId": 285_854, "teacherName": "杜江峰",
                        "customPlace": "生物信息学实验室（生物馆楼305）", "room": null,
                        "startUnit": 1, "endUnit": 4, "state": null, "weekIndex": 5,
                        "lessonType": null, "periods": null,
                        "expCourseCodeOrNameZhStr": "[475047-002]LINUX操作系统上机实验",
                        "expProjectNameZh": "1、安装Ubuntu"
                    }
                ]
            },
            {
                "id": -9001,
                "code": "EXP900101",
                "bizTypeId": null,
                "campus": {"id": null, "nameZh": "主校区", "nameEn": null, "code": null},
                "course": {
                    "id": -9101, "code": "EXP900101", "nameZh": "实验一",
                    "credits": null, "periodInfo": {"total": 4}
                },
                "scheduleState": null,
                "schedules": [{
                    "scheduleGroupId": 99001, "date": "2026-10-18", "weekday": 7,
                    "startTime": 1400, "endTime": 1740, "realStartTime": 0, "realEndTime": 0,
                    "teacherName": "独立教师", "customPlace": "开放实验室",
                    "startUnit": 5, "endUnit": 8, "weekIndex": 6,
                    "expCourseCodeOrNameZhStr": "[ORPHAN-01]开放实验课",
                    "expProjectNameZh": "实验一"
                }]
            }
        ]))
        .unwrap()
    }

    fn build_sample_timetable() -> Timetable {
        let mut entries = sample_raw_entries();
        entries.extend(sample_experiment_entries());
        build_timetable(sample_semester(), entries, sample_unit_layouts()).unwrap()
    }

    #[test]
    fn links_experiments_to_parent_classes_and_preserves_unscheduled_classes() {
        let timetable = build_sample_timetable();
        assert_eq!(timetable.classes.len(), 6);
        assert!((timetable.total_credits() - 11.0).abs() < f64::EPSILON);

        let unscheduled: Vec<i64> = timetable
            .unscheduled_classes()
            .into_iter()
            .map(|class| class.id)
            .collect();
        assert_eq!(unscheduled, vec![500_001]);

        let oct11 = timetable.sessions_on("2026-10-11".parse().unwrap());
        assert_eq!(oct11.len(), 1);
        assert_eq!(oct11[0].class().id, 464_182);
        assert_eq!(oct11[0].course().credits, Some(1.0));
        assert_eq!(oct11[0].session().kind, SessionKind::Experiment);
        assert_eq!(
            oct11[0].display_title(),
            "LINUX操作系统上机实验 - 1、安装Ubuntu"
        );

        let orphan = timetable.class(-9001).unwrap();
        assert_eq!(orphan.origin, ClassOrigin::StandaloneExperiment);
        assert_eq!(orphan.course.name_zh, "开放实验课");
        assert_eq!(orphan.course.period_info.total, 4);
    }

    #[test]
    fn merges_coteaching_and_preserves_cross_course_conflicts() {
        let timetable = build_sample_timetable();

        let oct12 = timetable.sessions_on("2026-10-12".parse().unwrap());
        assert_eq!(oct12.len(), 1);
        let teacher_names: Vec<&str> = oct12[0]
            .session()
            .teachers
            .iter()
            .map(|teacher| teacher.name_zh.as_str())
            .collect();
        assert_eq!(teacher_names, vec!["刘红涛", "田庆南"]);

        let week12 = timetable.week(12).unwrap();
        assert_eq!(week12.at(3, 1).unwrap().len(), 2);
        let conflicts = week12.conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].sessions.len(), 2);
    }

    #[test]
    fn accepts_extended_units_extra_weeks_and_roundtrips_clean_json() {
        let timetable = build_sample_timetable();

        let week20 = timetable.week(20).unwrap();
        assert_eq!(week20.max_unit(), 14);
        assert_eq!(timetable.units.len(), 14);
        assert_eq!(timetable.unit(5).unwrap().name_zh, "第+1节");
        assert_eq!(timetable.unit(14).unwrap().end_time.to_string(), "21:35:00");
        assert_eq!(week20.sessions().len(), 1);
        assert_eq!(
            week20.sessions()[0].session().original_date,
            Some("2027-01-13".parse().unwrap())
        );
        assert!(
            timetable
                .sessions
                .windows(2)
                .all(|pair| pair[0].date <= pair[1].date)
        );

        let encoded = serde_json::to_value(&timetable).unwrap();
        assert!(
            encoded["classes"][0]["course"]["periodInfo"]
                .get("designUnit")
                .is_none()
        );
        let restored: Timetable = serde_json::from_value(encoded).unwrap();
        assert_eq!(restored, timetable);
    }

    #[test]
    fn resolves_regular_and_fallback_unit_layouts() {
        let regular = build_timetable(
            sample_semester(),
            regular_only_raw_entries(),
            sample_unit_layouts(),
        )
        .unwrap();
        assert_eq!(regular.units.len(), 10);
        assert_eq!(regular.unit(10).unwrap().start_time.to_string(), "19:55:00");
        assert_eq!(regular.unit(10).unwrap().end_time.to_string(), "20:40:00");

        let empty = build_timetable(sample_semester(), Vec::new(), sample_unit_layouts()).unwrap();
        assert_eq!(empty.units.len(), 10);

        let orphan_only = build_timetable(
            sample_semester(),
            sample_experiment_entries(),
            sample_unit_layouts(),
        )
        .unwrap();
        assert_eq!(orphan_only.units.len(), 10);
    }

    #[test]
    fn timetable_deserialization_rejects_dangling_class_reference() {
        let broken = json!({
            "semester": sample_semester(),
            "classes": [],
            "sessions": [{
                "classId": 404,
                "scheduleGroupId": 1,
                "weekIndex": 1,
                "date": "2026-09-07",
                "weekday": 1,
                "startTime": "2026-09-07T08:00:00+08:00[Asia/Shanghai]",
                "endTime": "2026-09-07T09:40:00+08:00[Asia/Shanghai]",
                "startUnit": 1,
                "endUnit": 2,
                "teachers": [],
                "kind": "theory"
            }]
        });
        assert!(serde_json::from_value::<Timetable>(broken).is_err());

        let bad_schedule: Vec<RawCourseTableEntry> = serde_json::from_value(json!([{
            "id": 476_767,
            "code": "222302-013",
            "bizTypeId": 2,
            "course": {
                "id": 4171, "code": "222302", "nameZh": "大学物理B",
                "periodInfo": {"total": 64}
            },
            "schedules": [{
                "scheduleGroupId": 777_888,
                "date": "2026-09-07",
                "weekday": 9,
                "startTime": 800,
                "endTime": 940,
                "startUnit": 1,
                "endUnit": 2,
                "weekIndex": 1
            }]
        }]))
        .unwrap();
        let err = build_timetable(sample_semester(), bad_schedule, sample_unit_layouts())
            .err()
            .unwrap();
        assert!(err.is_decode());
        assert!(err.message().contains("classId=476767"));
        assert!(err.message().contains("scheduleGroupId=777888"));
    }
}
