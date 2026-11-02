use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;

use icalendar::{Calendar, Component, Event, EventLike};
use jiff::{
    Zoned,
    civil::{Date, Time},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

/// 具有中英文名称与编码的通用教务实体（如校区、教学楼）。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedEntity {
    /// 实体数字 ID。
    pub id: i64,
    /// 中文名称。
    pub name_zh: String,
    /// 英文名称。
    pub name_en: Option<String>,
    /// 教务编码。
    pub code: String,
}

/// 课程学时与周次统计信息。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeriodInfo {
    /// 课程总学时。
    pub total: u32,
    /// 开课总周数（实验课为 `None`）。
    pub weeks: Option<u32>,
    /// 每周学时数（实验课为 `None`）。
    pub periods_per_week: Option<u32>,
    /// 教务接口返回的未建模扩展字段。
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// 教务课程基础元数据。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Course {
    /// 课程数字 ID。
    pub id: i64,
    /// 课程代码。
    pub code: String,
    /// 课程中文名称。
    pub name_zh: String,
    /// 课程英文名称。
    pub name_en: Option<String>,
    /// 课程学分（实验课为 `None`）。
    pub credits: Option<f64>,
    /// 学时与周次统计信息。
    pub period_info: PeriodInfo,
    /// 教务接口返回的未建模扩展字段。
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// 上课教室及其所属教学楼与校区信息。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Room {
    /// 教室数字 ID。
    pub id: i64,
    /// 教室中文名称。
    pub name_zh: String,
    /// 教室英文名称。
    pub name_en: Option<String>,
    /// 所属教学楼。
    pub building: NamedEntity,
    /// 所属校区。
    pub campus: NamedEntity,
    /// 教务接口返回的未建模扩展字段。
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// 教务学期校历元数据。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Semester {
    /// 学期数字 ID。
    pub id: i64,
    /// 学期代码。
    pub code: String,
    /// 学期中文名称。
    pub name_zh: String,
    /// 学期英文名称。
    pub name_en: Option<String>,
    /// 所属学年标识（如 `"2026-2027"`）。
    pub school_year: String,
    /// 学期起始公历日期。
    pub start_date: Date,
    /// 学期结束公历日期。
    pub end_date: Date,
    /// 教学周是否以周日作为每周第一天。
    pub week_start_on_sunday: bool,
    /// 是否计入常规学期统计。
    pub count_in_term: bool,
    /// 学期季节代码或名称。
    pub season: String,
    /// 本学期包含的教学周序号列表（按校历顺序排列）。
    pub week_indices: Vec<u32>,
    /// 教务接口返回的未建模扩展字段。
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// 教务课表接口返回的单个教学班及其排课列表条目。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CourseTableEntry {
    /// 教学班 ID。
    pub id: i64,
    /// 关联的课程信息。
    pub course: Course,
    /// 该教学班在学期内的所有具体排课记录。
    pub schedules: Vec<Schedule>,
    /// 教务接口返回的未建模扩展字段。
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// 单次排课的时间、地点与授课教师详情。
///
/// 反序列化时会自动校验星期（`1..=7`）、节次（`1..=10` 且起始节次不大于结束节次）、
/// 教学周（非零）及起止时间顺序，并将 `HHMM` 时间统一转换为 `Asia/Shanghai` 时区的 [`Zoned`] 时间。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", try_from = "RawSchedule")]
pub struct Schedule {
    /// 排课组 ID。
    pub schedule_group_id: i64,
    /// 上课公历日期。
    pub date: Date,
    /// 星期序号（`1` 表示周一，`7` 表示周日）。
    pub weekday: u8,
    /// 课表标准开始时间（`Asia/Shanghai` 时区）。
    pub start_time: Zoned,
    /// 课表标准结束时间（`Asia/Shanghai` 时区）。
    pub end_time: Zoned,
    /// 实际开始时间（未单独指定时与 `start_time` 相同）。
    pub real_start_time: Zoned,
    /// 实际结束时间（未单独指定时与 `end_time` 相同）。
    pub real_end_time: Zoned,
    /// 授课教师中文姓名。
    pub teacher_name: String,
    /// 授课教师英文姓名。
    pub teacher_name_en: Option<String>,
    /// 自定义上课地点描述（无固定教室时使用）。
    pub custom_place: Option<String>,
    /// 固定上课教室（未分配固定教室时为 `None`）。
    pub room: Option<Room>,
    /// 起始节次（取值范围 `1..=10`）。
    pub start_unit: u8,
    /// 结束节次（取值范围 `1..=10`，且不小于 `start_unit`）。
    pub end_unit: u8,
    /// 排课状态文本（如 `"已结束"`；实验课为 `None`）。
    pub state: Option<String>,
    /// 所属教学周序号（从 `1` 开始）。
    pub week_index: u32,
    /// 授课类型标识（如 `"THEORY"`；实验课为 `None`）。
    pub lesson_type: Option<String>,
    /// 本次排课占用的学时数（实验课为 `None`）。
    pub periods: Option<u32>,
    /// 教务接口返回的未建模扩展字段。
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSchedule {
    schedule_group_id: i64,
    date: Date,
    weekday: u8,
    start_time: Value,
    end_time: Value,
    real_start_time: Option<Value>,
    real_end_time: Option<Value>,
    teacher_name: String,
    teacher_name_en: Option<String>,
    custom_place: Option<String>,
    room: Option<Room>,
    start_unit: u8,
    end_unit: u8,
    state: Option<String>,
    week_index: u32,
    lesson_type: Option<String>,
    periods: Option<u32>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

impl TryFrom<RawSchedule> for Schedule {
    type Error = Error;

    fn try_from(raw: RawSchedule) -> Result<Self> {
        validate_index(raw.weekday, raw.start_unit)?;
        validate_index(raw.weekday, raw.end_unit)?;
        if raw.start_unit > raw.end_unit || raw.week_index == 0 {
            return Err(Error::decode_message("课程节次或教学周无效".into()));
        }
        let start_time = parse_time(raw.date, &raw.start_time)?;
        let end_time = parse_time(raw.date, &raw.end_time)?;
        // 实验课排课以 `0` 表示未单独指定实际时间。
        let is_specified = |value: &&Value| value.as_i64() != Some(0);
        let real_start_time = raw
            .real_start_time
            .as_ref()
            .filter(is_specified)
            .map(|value| parse_time(raw.date, value))
            .transpose()?
            .unwrap_or_else(|| start_time.clone());
        let real_end_time = raw
            .real_end_time
            .as_ref()
            .filter(is_specified)
            .map(|value| parse_time(raw.date, value))
            .transpose()?
            .unwrap_or_else(|| end_time.clone());
        if start_time >= end_time || real_start_time >= real_end_time {
            return Err(Error::decode_message("课程结束时间必须晚于开始时间".into()));
        }
        Ok(Self {
            schedule_group_id: raw.schedule_group_id,
            date: raw.date,
            weekday: raw.weekday,
            start_time,
            end_time,
            real_start_time,
            real_end_time,
            teacher_name: raw.teacher_name,
            teacher_name_en: raw.teacher_name_en,
            custom_place: raw.custom_place,
            room: raw.room,
            start_unit: raw.start_unit,
            end_unit: raw.end_unit,
            state: raw.state,
            week_index: raw.week_index,
            lesson_type: raw.lesson_type,
            periods: raw.periods,
            extra: raw.extra,
        })
    }
}

fn parse_time(date: Date, value: &Value) -> Result<Zoned> {
    let text = match value {
        Value::String(text) => text.trim().to_owned(),
        Value::Number(number) => number.to_string(),
        _ => return Err(Error::decode_message("课程时间必须为 HHMM".into())),
    };
    if let Ok(datetime) = text.parse::<Zoned>() {
        return Ok(datetime);
    }
    if text.len() > 4 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::decode_message("课程时间必须为 HHMM".into()));
    }
    let value: u32 = text
        .parse()
        .map_err(|_| Error::decode_message("课程时间无效".into()))?;
    let hour =
        i8::try_from(value / 100).map_err(|_| Error::decode_message("课程小时无效".into()))?;
    let minute =
        i8::try_from(value % 100).map_err(|_| Error::decode_message("课程分钟无效".into()))?;
    let time = Time::new(hour, minute, 0, 0).map_err(Error::decode)?;
    date.to_datetime(time)
        .in_tz("Asia/Shanghai")
        .map_err(Error::decode)
}

/// 绑定了课程元数据与单次排课详情的课表项。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Lesson {
    /// 课程元数据。
    pub course: Course,
    /// 本次排课的时间与地点详情。
    pub schedule: Schedule,
}

/// 单个教学周内的排课集合。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeachingWeek {
    /// 教学周序号（从 `1` 开始）。
    pub week_index: u32,
    lessons: Vec<Lesson>,
}

impl TeachingWeek {
    /// 每周包含的天数（周一至周日，共 `7` 天）。
    pub const DAYS: u8 = 7;
    /// 每天包含的标准节次数（第 `1` 至第 `10` 节）。
    pub const UNITS: u8 = 10;

    /// 返回本周内的所有排课记录切片。
    #[must_use]
    pub fn lessons(&self) -> &[Lesson] {
        &self.lessons
    }

    fn lesson_at(&self, weekday: u8, unit: u8) -> Option<&Lesson> {
        self.lessons.iter().rev().find(|lesson| {
            let schedule = &lesson.schedule;
            schedule.weekday == weekday && (schedule.start_unit..=schedule.end_unit).contains(&unit)
        })
    }

    /// 查询指定星期（`1..=7`）与节次（`1..=10`）上的排课；若存在重叠排课则返回最后一条记录。
    ///
    /// # Errors
    ///
    /// 当 `weekday` 不在 `1..=7` 或 `unit` 不在 `1..=10` 范围内时返回 [`Error::InvalidInput`]。
    pub fn get(&self, weekday: u8, unit: u8) -> Result<Option<&Lesson>> {
        validate_index(weekday, unit)?;
        Ok(self.lesson_at(weekday, unit))
    }

    /// 按第 `1` 至第 `10` 节的顺序返回指定星期（`1..=7`）全天的排课槽位。
    ///
    /// # Errors
    ///
    /// 当 `weekday` 不在 `1..=7` 范围内时返回 [`Error::InvalidInput`]。
    pub fn day(&self, weekday: u8) -> Result<Vec<Option<&Lesson>>> {
        (1..=Self::UNITS)
            .map(|unit| self.get(weekday, unit))
            .collect()
    }

    /// 按周一至周日（`1..=7`）的顺序返回指定节次（`1..=10`）在一周内的排课槽位。
    ///
    /// # Errors
    ///
    /// 当 `unit` 不在 `1..=10` 范围内时返回 [`Error::InvalidInput`]。
    pub fn unit(&self, unit: u8) -> Result<Vec<Option<&Lesson>>> {
        (1..=Self::DAYS).map(|day| self.get(day, unit)).collect()
    }

    /// 生成 `7 × 10`（外层按星期 `1..=7`、内层按节次 `1..=10`）的整周二维课表网格。
    #[must_use]
    pub fn grid(&self) -> Vec<Vec<Option<&Lesson>>> {
        (1..=Self::DAYS)
            .map(|day| {
                (1..=Self::UNITS)
                    .map(|unit| self.lesson_at(day, unit))
                    .collect()
            })
            .collect()
    }

    /// 将本周所有排课导出为去重后的 `iCalendar`（[`Calendar`]）日历对象。
    #[must_use]
    pub fn to_calendar(&self) -> Calendar {
        calendar(self.lessons.iter())
    }
}

fn validate_index(weekday: u8, unit: u8) -> Result<()> {
    if !(1..=TeachingWeek::DAYS).contains(&weekday) || !(1..=TeachingWeek::UNITS).contains(&unit) {
        return Err(Error::invalid_input(
            "星期范围为 1..=7，节次范围为 1..=10".into(),
        ));
    }
    Ok(())
}

/// 按校历教学周顺序排列的多周课表集合。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct TeachingWeeks(Vec<TeachingWeek>);

impl TeachingWeeks {
    /// 根据学期教学周序号序列 `week_indices` 与教学班条目 `courses` 组装教学周课表。
    ///
    /// 不在 `week_indices` 中的排课记录会被忽略。
    ///
    /// # Errors
    ///
    /// 当 `week_indices` 中包含 `0` 或存在重复的教学周序号时返回 [`Error::Decode`]。
    pub fn from_courses(week_indices: &[u32], courses: Vec<CourseTableEntry>) -> Result<Self> {
        let mut positions = BTreeMap::new();
        let mut weeks = Vec::with_capacity(week_indices.len());
        for &week in week_indices {
            if week == 0 || positions.insert(week, weeks.len()).is_some() {
                return Err(Error::decode_message("学期教学周不能为 0 或重复".into()));
            }
            weeks.push(TeachingWeek {
                week_index: week,
                lessons: Vec::new(),
            });
        }
        for entry in courses {
            for schedule in entry.schedules {
                if let Some(&position) = positions.get(&schedule.week_index) {
                    weeks[position].lessons.push(Lesson {
                        course: entry.course.clone(),
                        schedule,
                    });
                }
            }
        }
        Ok(Self(weeks))
    }

    /// 将集合中全部教学周的排课导出为去重后的 `iCalendar`（[`Calendar`]）日历对象。
    #[must_use]
    pub fn to_calendar(&self) -> Calendar {
        calendar(self.0.iter().flat_map(|week| &week.lessons))
    }
}

impl Deref for TeachingWeeks {
    type Target = [TeachingWeek];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<TeachingWeek> for TeachingWeeks {
    fn from(week: TeachingWeek) -> Self {
        Self(vec![week])
    }
}

impl IntoIterator for TeachingWeeks {
    type Item = TeachingWeek;
    type IntoIter = std::vec::IntoIter<TeachingWeek>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a TeachingWeeks {
    type Item = &'a TeachingWeek;
    type IntoIter = std::slice::Iter<'a, TeachingWeek>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

fn calendar<'a>(lessons: impl Iterator<Item = &'a Lesson>) -> Calendar {
    let mut calendar = Calendar::new();
    calendar.append_property(("PRODID", "-//libzzu//Teaching Schedule//CN"));
    let mut processed = BTreeSet::new();
    for lesson in lessons {
        let schedule = &lesson.schedule;
        let course = &lesson.course;
        let uid = format!(
            "{}-{}-{}-{}@libzzu",
            course.id,
            schedule.schedule_group_id,
            schedule.real_start_time.timestamp().as_second(),
            schedule.real_end_time.timestamp().as_second()
        );
        if !processed.insert(uid.clone()) {
            continue;
        }
        let location = match &schedule.room {
            Some(room) => format!(
                "{} {} {}",
                room.campus.name_zh, room.building.name_zh, room.name_zh
            ),
            None => schedule.custom_place.clone().unwrap_or_default(),
        };
        let credits = course
            .credits
            .map_or_else(|| "未知".to_owned(), |credits| credits.to_string());
        let description = format!(
            "授课教师: {}\n课程代码: {}\n学分: {}\n节次: 第 {} - {} 节\n教学周: 第 {} 周\n类型: {}",
            schedule.teacher_name,
            course.code,
            credits,
            schedule.start_unit,
            schedule.end_unit,
            schedule.week_index,
            schedule.lesson_type.as_deref().unwrap_or("未知")
        );
        calendar.push(
            Event::new()
                .uid(&uid)
                .summary(&course.name_zh)
                .location(&location)
                .description(&description)
                .add_property(
                    "DTSTART",
                    schedule
                        .real_start_time
                        .timestamp()
                        .strftime("%Y%m%dT%H%M%SZ")
                        .to_string(),
                )
                .add_property(
                    "DTEND",
                    schedule
                        .real_end_time
                        .timestamp()
                        .strftime("%Y%m%dT%H%M%SZ")
                        .to_string(),
                )
                .done(),
        );
    }
    calendar
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn course_table_normalizes_times_and_deduplicates_calendar_events() {
        let schedule = json!({"scheduleGroupId":1,"date":"2026-10-26","weekday":1,
            "startTime":800,"endTime":"0950","realStartTime":800,"realEndTime":950,
            "teacherName":"教师","startUnit":1,"endUnit":2,"state":"NORMAL",
            "weekIndex":3,"lessonType":"THEORY","periods":2});
        let entry: CourseTableEntry = serde_json::from_value(json!({"id":1,
            "course":{"id":2,"code":"CS101","nameZh":"程序设计","credits":2.0,
                "periodInfo":{"total":32,"weeks":16,"periodsPerWeek":2}},
            "schedules":[schedule.clone(),schedule.clone()]}))
        .unwrap();
        assert_eq!(
            entry.schedules[0].start_time.to_string(),
            "2026-10-26T08:00:00+08:00[Asia/Shanghai]"
        );
        let weeks = TeachingWeeks::from_courses(&[1, 3], vec![entry]).unwrap();
        assert!(weeks[0].get(1, 1).unwrap().is_none());
        assert_eq!(
            weeks[1].get(1, 2).unwrap().unwrap().course.name_zh,
            "程序设计"
        );
        assert!(weeks[1].get(0, 1).is_err());
        let calendar = weeks.to_calendar().to_string();
        assert_eq!(calendar.matches("BEGIN:VEVENT").count(), 1);
        assert!(calendar.contains("DTSTART:20261026T000000Z"));
        // DTSTAMP 取渲染时刻，不属于课表内容。
        let events = |calendar: &str| -> Vec<String> {
            calendar
                .lines()
                .filter(|line| !line.starts_with("DTSTAMP"))
                .map(str::to_owned)
                .collect()
        };
        let restored: TeachingWeeks =
            serde_json::from_slice(&serde_json::to_vec(&weeks).unwrap()).unwrap();
        assert_eq!(
            events(&restored.to_calendar().to_string()),
            events(&calendar)
        );
        let mut invalid = schedule;
        invalid["startTime"] = json!(2460);
        assert!(serde_json::from_value::<Schedule>(invalid).is_err());
    }

    #[test]
    fn experiment_entry_keeps_null_fields_through_json_roundtrip() {
        let entry: CourseTableEntry = serde_json::from_value(json!({"id":1,
            "course":{"id":2,"code":"EXP101","nameZh":"物理实验","credits":null,
                "periodInfo":{"total":8,"weeks":null,"periodsPerWeek":null}},
            "schedules":[{"scheduleGroupId":1,"date":"2026-10-26","weekday":1,
                "startTime":800,"endTime":1150,"realStartTime":0,"realEndTime":0,
                "teacherName":"教师","customPlace":"实验室","room":null,"startUnit":1,"endUnit":4,
                "state":null,"weekIndex":3,"lessonType":null,"periods":null}]}))
        .unwrap();
        let weeks = TeachingWeeks::from_courses(&[3], vec![entry]).unwrap();
        let restored: TeachingWeeks =
            serde_json::from_slice(&serde_json::to_vec(&weeks).unwrap()).unwrap();
        let lesson = &restored[0].lessons()[0];
        assert_eq!(lesson.course.credits, None);
        assert_eq!(lesson.course.period_info.weeks, None);
        assert_eq!(lesson.course.period_info.periods_per_week, None);
        assert_eq!(lesson.schedule.state, None);
        assert_eq!(lesson.schedule.lesson_type, None);
        assert_eq!(lesson.schedule.periods, None);
        assert_eq!(lesson.schedule.real_start_time, lesson.schedule.start_time);
        assert_eq!(lesson.schedule.real_end_time, lesson.schedule.end_time);
        assert!(restored.to_calendar().to_string().contains("学分: 未知"));
    }
}
