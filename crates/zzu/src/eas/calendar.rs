use std::collections::{BTreeSet, HashSet};
use std::fmt::Write as _;

use icalendar::{Alarm, Calendar, Component, Event, EventLike, Property, Trigger};
use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

use super::timetable::{SessionView, Timetable};

/// 将 [`Timetable`] 导出为 `iCalendar`（`.ics`）时的筛选与展示配置。
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CalendarExportOptions {
    /// 仅导出指定教学周列表；为 `None` 或空时导出全部周次。
    #[serde(default)]
    pub weeks: Option<Vec<u32>>,
    /// 仅导出指定教学班 ID 列表；为 `None` 或空时导出全部教学班。
    #[serde(default)]
    pub class_ids: Option<Vec<i64>>,
    /// 仅导出不早于该公历日期的排课（含当日）。
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<crate::schema::Date>"))]
    pub start_date: Option<Date>,
    /// 仅导出不晚于该公历日期的排课（含当日）。
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<crate::schema::Date>"))]
    pub end_date: Option<Date>,
    /// 课前提醒提前分钟数列表（例如 `[15]` 表示上课前 15 分钟提醒；空列表不生成提醒）。
    #[serde(default)]
    pub alarm_minutes_before: Vec<u32>,
    /// 日历显示名称（写入 `X-WR-CALNAME`；省略时默认使用学期中文名称）。
    #[serde(default)]
    pub calendar_name: Option<String>,
}

impl CalendarExportOptions {
    fn validate(&self) -> Result<()> {
        if let Some(weeks) = &self.weeks
            && weeks.contains(&0)
        {
            return Err(Error::invalid_input("教学周序号从 1 开始".into()));
        }
        if let (Some(start), Some(end)) = (self.start_date, self.end_date)
            && start > end
        {
            return Err(Error::invalid_input("结束日期不能早于起始日期".into()));
        }
        Ok(())
    }
}

impl Timetable {
    /// 按指定配置将课表导出为标准 `iCalendar`（RFC 5545）日历对象。
    ///
    /// 导出的事件保留合上教师、实验项目标题与课程学分，并按排课唯一标识去重。
    ///
    /// # Errors
    ///
    /// `options.weeks` 包含 `0` 或 `options.start_date > options.end_date` 时返回 [`Error::InvalidInput`]。
    pub fn to_calendar(&self, options: &CalendarExportOptions) -> Result<Calendar> {
        options.validate()?;

        let week_filter: Option<HashSet<u32>> = options
            .weeks
            .as_ref()
            .filter(|weeks| !weeks.is_empty())
            .map(|weeks| weeks.iter().copied().collect());
        let class_filter: Option<HashSet<i64>> = options
            .class_ids
            .as_ref()
            .filter(|ids| !ids.is_empty())
            .map(|ids| ids.iter().copied().collect());
        let alarms: Vec<u32> = options
            .alarm_minutes_before
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        let cal_name = options
            .calendar_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.semester.name_zh);

        let mut calendar = Calendar::empty();
        calendar.append_property(("VERSION", "2.0"));
        calendar.append_property(("PRODID", "-//libzzu//Teaching Schedule//CN"));
        calendar.append_property(("CALSCALE", "GREGORIAN"));
        calendar.append_property(("X-WR-CALNAME", cal_name));
        calendar.append_property(("X-WR-TIMEZONE", "Asia/Shanghai"));

        let mut emitted_uids = BTreeSet::new();
        for session in &self.sessions {
            if let Some(weeks) = &week_filter
                && !weeks.contains(&session.week_index)
            {
                continue;
            }
            if let Some(class_ids) = &class_filter
                && !class_ids.contains(&session.class_id)
            {
                continue;
            }
            if let Some(start) = options.start_date
                && session.date < start
            {
                continue;
            }
            if let Some(end) = options.end_date
                && session.date > end
            {
                continue;
            }
            let Some(view) = self.view(session) else {
                continue;
            };
            let uid = session_uid(self.semester.id, view);
            if !emitted_uids.insert(uid.clone()) {
                continue;
            }
            calendar.push(build_event(&uid, view, &alarms));
        }
        Ok(calendar)
    }
}

fn session_uid(semester_id: i64, view: SessionView<'_>) -> String {
    let session = view.session();
    let project_tag = session
        .experiment_project
        .as_ref()
        .map_or_else(|| "0".to_owned(), |project| project.id.to_string());
    let room_tag = session
        .room
        .as_ref()
        .map_or_else(|| "0".to_owned(), |room| room.id.to_string());
    format!(
        "{semester_id}-{}-{}-{}-{}-{project_tag}-{room_tag}@libzzu",
        session.class_id,
        session.schedule_group_id,
        session.start_time.timestamp().as_second(),
        session.end_time.timestamp().as_second()
    )
}

fn build_event(uid: &str, view: SessionView<'_>, alarms: &[u32]) -> Event {
    let session = view.session();
    let summary = view.display_title();
    let location = view.display_place().unwrap_or_default();
    let description = build_description(view);

    let mut event = Event::new();
    event
        .uid(uid)
        .summary(&summary)
        .location(&location)
        .description(&description)
        .add_property(
            "DTSTART",
            session
                .start_time
                .timestamp()
                .strftime("%Y%m%dT%H%M%SZ")
                .to_string(),
        )
        .add_property(
            "DTEND",
            session
                .end_time
                .timestamp()
                .strftime("%Y%m%dT%H%M%SZ")
                .to_string(),
        );

    for &minutes in alarms {
        let positive = Property::new("TRIGGER", format!("PT{minutes}M"));
        if let Ok(parsed) = Trigger::try_from(&positive)
            && let Some(&duration) = parsed.as_duration()
        {
            let alarm_text = if location.is_empty() {
                summary.to_string()
            } else {
                format!("{summary} @ {location}")
            };
            event.alarm(Alarm::display(&alarm_text, Trigger::before_start(duration)));
        }
    }
    event.done()
}

fn build_description(view: SessionView<'_>) -> String {
    let class = view.class();
    let course = view.course();
    let session = view.session();

    let teachers = if session.teachers.is_empty() {
        "待定".to_owned()
    } else {
        session
            .teachers
            .iter()
            .map(|teacher| teacher.name_zh.as_str())
            .collect::<Vec<_>>()
            .join("、")
    };
    let credits = course
        .credits
        .map_or_else(|| "未知".to_owned(), |value| value.to_string());
    let units = format_unit_range(
        session.start_unit,
        session.end_unit,
        session.start_unit_name_zh.as_deref(),
        session.end_unit_name_zh.as_deref(),
    );

    let mut description = format!(
        "授课教师: {teachers}\n课程代码: {}\n教学班号: {}\n学分: {credits}\n节次: {units}\n教学周: 第 {} 周\n类型: {}",
        course.code,
        class.code,
        session.week_index,
        session.kind.label_zh(),
    );
    if let Some(original_date) = session.original_date {
        let _ = write!(description, "\n原上课日期: {original_date}");
    }
    description
}

fn format_unit_range(
    start: u8,
    end: u8,
    start_name: Option<&str>,
    end_name: Option<&str>,
) -> String {
    match (start_name, end_name) {
        (Some(s), Some(e)) if s != start.to_string() || e != end.to_string() => {
            if s == e {
                s.to_owned()
            } else {
                format!("{s} - {e}")
            }
        }
        _ if start == end => format!("第 {start} 节"),
        _ => format!("第 {start} - {end} 节"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_timetable() -> Timetable {
        serde_json::from_value(json!({
            "semester": {
                "id": 194,
                "code": "2026-2027-1",
                "nameZh": "2026-2027学年第一学期",
                "schoolYear": "2026-2027",
                "season": "autumn",
                "startDate": "2026-09-07",
                "endDate": "2027-01-17",
                "weekStartOnSunday": false,
                "countInTerm": true,
                "weekIndices": [1, 2, 3, 4, 5, 6, 7]
            },
            "classes": [
                {
                    "id": 464_251,
                    "code": "472022-002",
                    "origin": "regular",
                    "course": {
                        "id": 16848,
                        "code": "472022",
                        "nameZh": "生物化学 (I)",
                        "credits": 3.0,
                        "periodInfo": {"total": 48, "weeks": 16, "periodsPerWeek": 3}
                    },
                    "teachers": [{"nameZh": "刘红涛"}, {"nameZh": "田庆南"}],
                    "scheduleState": "published"
                },
                {
                    "id": 464_182,
                    "code": "475047-002",
                    "origin": "regular",
                    "course": {
                        "id": 16856,
                        "code": "475047",
                        "nameZh": "LINUX操作系统上机实验",
                        "credits": 1.0,
                        "periodInfo": {"total": 24, "weeks": 6, "periodsPerWeek": 4}
                    },
                    "teachers": [{"nameZh": "杜江峰"}],
                    "scheduleState": "unpublished"
                }
            ],
            "sessions": [
                {
                    "classId": 464_251,
                    "scheduleGroupId": 600_028,
                    "weekIndex": 6,
                    "date": "2026-10-12",
                    "weekday": 1,
                    "startTime": "2026-10-12T16:00:00+08:00[Asia/Shanghai]",
                    "endTime": "2026-10-12T17:40:00+08:00[Asia/Shanghai]",
                    "startUnit": 7,
                    "endUnit": 8,
                    "teachers": [{"nameZh": "刘红涛"}, {"nameZh": "田庆南"}],
                    "room": {
                        "id": 391,
                        "nameZh": "物_209",
                        "building": {"id": 23, "nameZh": "物理馆", "code": "物理馆"},
                        "campus": {"id": 2, "nameZh": "主校区", "code": "01"}
                    },
                    "kind": "theory",
                    "state": "pending",
                    "periods": 2
                },
                {
                    "classId": 464_182,
                    "scheduleGroupId": 24735,
                    "weekIndex": 7,
                    "date": "2026-10-25",
                    "originalDate": "2026-10-24",
                    "weekday": 7,
                    "startTime": "2026-10-25T08:00:00+08:00[Asia/Shanghai]",
                    "endTime": "2026-10-25T11:50:00+08:00[Asia/Shanghai]",
                    "startUnit": 1,
                    "endUnit": 4,
                    "teachers": [{"nameZh": "杜江峰"}],
                    "customPlace": "生物信息学实验室（生物馆楼305）",
                    "kind": "experiment",
                    "experimentProject": {
                        "id": -2629,
                        "code": "4701504703",
                        "nameZh": "3、bash脚本循环练习",
                        "totalPeriods": 4
                    }
                }
            ]
        }))
        .unwrap()
    }

    #[test]
    fn calendar_preserves_coteachers_experiment_metadata_and_alarms() {
        let timetable = sample_timetable();
        let options = CalendarExportOptions {
            alarm_minutes_before: vec![15],
            ..Default::default()
        };
        let ics = timetable.to_calendar(&options).unwrap().to_string();
        // 按 RFC 5545 3.1 节展开 75 字节折行，再断言逻辑内容。
        let unfolded = ics.replace("\r\n ", "");
        assert_eq!(unfolded.matches("\r\nVERSION:").count(), 1);
        assert_eq!(unfolded.matches("\r\nPRODID:").count(), 1);
        assert_eq!(unfolded.matches("\r\nCALSCALE:").count(), 1);
        assert!(unfolded.contains("\r\nVERSION:2.0\r\n"));
        assert!(unfolded.contains("\r\nPRODID:-//libzzu//Teaching Schedule//CN\r\n"));
        assert!(unfolded.contains("\r\nCALSCALE:GREGORIAN\r\n"));
        assert!(!unfolded.contains("ICALENDAR-RS"));
        assert_eq!(unfolded.matches("BEGIN:VEVENT").count(), 2);
        assert_eq!(unfolded.matches("BEGIN:VALARM").count(), 2);
        assert!(unfolded.contains("X-WR-CALNAME:2026-2027学年第一学期"));
        assert!(unfolded.contains("刘红涛、田庆南"));
        assert!(unfolded.contains("SUMMARY:LINUX操作系统上机实验 - 3、bash脚本循环练习"));
        assert!(unfolded.contains("学分: 1"));
        assert!(unfolded.contains("类型: 实验"));
        assert!(unfolded.contains("原上课日期: 2026-10-24"));
        assert!(unfolded.contains("DTSTART:20261012T080000Z"));
        assert!(unfolded.contains("TRIGGER;RELATED=START:-PT900S"));
    }

    #[test]
    fn calendar_filters_by_week_class_and_date_range_and_validates_options() {
        let timetable = sample_timetable();
        let week6 = timetable
            .to_calendar(&CalendarExportOptions {
                weeks: Some(vec![6]),
                ..Default::default()
            })
            .unwrap()
            .to_string();
        assert_eq!(week6.matches("BEGIN:VEVENT").count(), 1);
        assert!(week6.contains("生物化学 (I)"));

        let by_class = timetable
            .to_calendar(&CalendarExportOptions {
                class_ids: Some(vec![464_182]),
                ..Default::default()
            })
            .unwrap()
            .to_string();
        assert_eq!(by_class.matches("BEGIN:VEVENT").count(), 1);
        assert!(by_class.contains("LINUX操作系统上机实验"));

        let bad_week = CalendarExportOptions {
            weeks: Some(vec![0]),
            ..Default::default()
        };
        assert!(
            timetable
                .to_calendar(&bad_week)
                .unwrap_err()
                .is_invalid_input()
        );

        let bad_dates = CalendarExportOptions {
            start_date: Some("2026-11-01".parse().unwrap()),
            end_date: Some("2026-10-01".parse().unwrap()),
            ..Default::default()
        };
        assert!(
            timetable
                .to_calendar(&bad_dates)
                .unwrap_err()
                .is_invalid_input()
        );
    }
}
