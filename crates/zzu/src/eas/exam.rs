use jiff::{ZonedDateTime, civil::Date};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

use super::common::{NamedEntity, deserialize_null_default, parse_time, trim_non_empty};

/// 单场学生考试安排记录。
///
/// 教务移动端接口仅返回当前尚未结束的待考安排；对于尚未排定具体时间或考场的考试，
/// [`Self::date`]、[`Self::start_time`]、[`Self::end_time`] 与 [`Self::place`] 可能为 `None`。
/// 一旦排定起止时间，恒满足 `start_time < end_time` 且 `date`、`weekday` 与 `start_time` 一致。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(!try_from)
)]
#[serde(rename_all = "camelCase", try_from = "RawExam")]
pub struct Exam {
    /// 考试安排记录 ID（上游提供时返回）。
    #[serde(default)]
    pub id: Option<i64>,
    /// 课程代码（上游提供时返回）。
    #[serde(default)]
    pub course_code: Option<String>,
    /// 课程中文名称。
    pub course_name_zh: String,
    /// 课程英文名称。
    #[serde(default)]
    pub course_name_en: Option<String>,
    /// 教学班选课课号（上游提供时返回）。
    #[serde(default)]
    pub class_code: Option<String>,
    /// 考试类别（如期末考试、期中考试、补考、缓考）。
    #[serde(default)]
    pub exam_type: Option<NamedEntity>,
    /// 培养类别（如主修、辅修）。
    #[serde(default)]
    pub cultivate_type: Option<NamedEntity>,
    /// 考试公历日期（尚未排定时为 `None`）。
    #[serde(default)]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<crate::schema::Date>"))]
    pub date: Option<Date>,
    /// 星期序号（`1` 表示周一，`7` 表示周日；`date` 存在时恒有值并与之匹配）。
    #[serde(default)]
    pub weekday: Option<u8>,
    /// 考试开始时间（`Asia/Shanghai` 时区；尚未排定时为 `None`）。
    #[serde(default)]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<crate::schema::ZonedDateTime>")
    )]
    pub start_time: Option<ZonedDateTime>,
    /// 考试结束时间（`Asia/Shanghai` 时区；与 `start_time` 成对出现且严格晚于 `start_time`）。
    #[serde(default)]
    #[cfg_attr(
        feature = "schemars",
        schemars(with = "Option<crate::schema::ZonedDateTime>")
    )]
    pub end_time: Option<ZonedDateTime>,
    /// 考场地点中文描述（尚未排定考场时为 `None`）。
    #[serde(default)]
    pub place: Option<String>,
    /// 考场地点英文描述。
    #[serde(default)]
    pub place_en: Option<String>,
    /// 考场座位号文本（上游独立提供时返回）。
    #[serde(default)]
    pub seat_number: Option<String>,
}

impl Exam {
    /// 判断该场考试是否已排定具体的起止时间。
    #[must_use]
    pub const fn is_scheduled(&self) -> bool {
        self.start_time.is_some() && self.end_time.is_some()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawExam {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    course_code: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    course_name_zh: String,
    #[serde(default)]
    course_name_en: Option<String>,
    #[serde(default, alias = "lessonCode")]
    class_code: Option<String>,
    #[serde(default)]
    exam_type: Option<NamedEntity>,
    #[serde(default)]
    cultivate_type: Option<NamedEntity>,
    #[serde(default, alias = "examDate")]
    date: Option<Date>,
    #[serde(default, alias = "weekDay")]
    weekday: Option<u8>,
    #[serde(default)]
    start_time: Option<Value>,
    #[serde(default)]
    end_time: Option<Value>,
    #[serde(default)]
    place: Option<String>,
    #[serde(default)]
    place_en: Option<String>,
    #[serde(default, alias = "seatNo")]
    seat_number: Option<Value>,
}

impl TryFrom<RawExam> for Exam {
    type Error = Error;

    fn try_from(raw: RawExam) -> Result<Self> {
        let course_name_zh = trim_non_empty(Some(raw.course_name_zh))
            .ok_or_else(|| Error::decode_message("考试记录缺少课程名称 courseNameZh".into()))?;

        let start_value = raw.start_time.filter(is_present_time);
        let end_value = raw.end_time.filter(is_present_time);

        let (date, start_time, end_time) = match (raw.date, start_value, end_value) {
            (Some(date), Some(start), Some(end)) => {
                let start_zoned = parse_time(date, &start)?;
                let end_zoned = parse_time(date, &end)?;
                if start_zoned >= end_zoned {
                    return Err(Error::decode_message("考试结束时间必须晚于开始时间".into()));
                }
                (Some(date), Some(start_zoned), Some(end_zoned))
            }
            (None, Some(start), Some(end)) => {
                let start_zoned = parse_zoned_only(&start)?;
                let end_zoned = parse_zoned_only(&end)?;
                if start_zoned >= end_zoned {
                    return Err(Error::decode_message("考试结束时间必须晚于开始时间".into()));
                }
                (Some(start_zoned.date()), Some(start_zoned), Some(end_zoned))
            }
            (date, None, None) => (date, None, None),
            _ => {
                return Err(Error::decode_message(
                    "考试起止时间必须同时提供或同时为空".into(),
                ));
            }
        };

        if let Some(day) = raw.weekday
            && !(1..=7).contains(&day)
        {
            return Err(Error::decode_message(
                "考试星期序号必须在 1..=7 范围内".into(),
            ));
        }
        let weekday = match date {
            Some(resolved_date) => Some(
                u8::try_from(resolved_date.weekday().to_monday_one_offset())
                    .expect("weekday in 1..=7"),
            ),
            None => raw.weekday,
        };

        let seat_number = parse_optional_scalar(raw.seat_number)?;

        Ok(Self {
            id: raw.id,
            course_code: trim_non_empty(raw.course_code),
            course_name_zh,
            course_name_en: trim_non_empty(raw.course_name_en),
            class_code: trim_non_empty(raw.class_code),
            exam_type: raw.exam_type,
            cultivate_type: raw.cultivate_type,
            date,
            weekday,
            start_time,
            end_time,
            place: trim_non_empty(raw.place),
            place_en: trim_non_empty(raw.place_en),
            seat_number,
        })
    }
}

fn is_present_time(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Number(number) => number.as_i64() != Some(0),
        Value::String(text) => {
            let trimmed = text.trim();
            !trimmed.is_empty() && trimmed != "0"
        }
        _ => true,
    }
}

fn parse_zoned_only(value: &Value) -> Result<ZonedDateTime> {
    let text = value
        .as_str()
        .ok_or_else(|| Error::decode_message("缺少考试日期时，时间必须为带时区字符串".into()))?;
    text.trim().parse::<ZonedDateTime>().map_err(Error::decode)
}

fn parse_optional_scalar(value: Option<Value>) -> Result<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(trim_non_empty(Some(text))),
        Some(Value::Number(number)) => Ok(Some(number.to_string())),
        Some(_) => Err(Error::decode_message(
            "座位号 seatNumber 必须为字符串或整数".into(),
        )),
    }
}

pub(super) fn normalize_exams(mut exams: Vec<Exam>) -> Vec<Exam> {
    exams.sort_by(|left, right| {
        let left_key = (
            left.date.is_none(),
            left.date,
            left.start_time.as_ref().map(ZonedDateTime::timestamp),
            &left.course_name_zh,
            left.id,
        );
        let right_key = (
            right.date.is_none(),
            right.date,
            right.start_time.as_ref().map(ZonedDateTime::timestamp),
            &right.course_name_zh,
            right.id,
        );
        left_key.cmp(&right_key)
    });
    exams
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn exams_normalize_upstream_times_sort_scheduled_first_and_roundtrip() {
        let raw = json!([
            {
                "id": 1003,
                "courseNameZh": "计算机网络",
                "courseCode": "CS301",
                "examType": {"id": 1, "code": "01", "nameZh": "期末考试", "nameEn": "Final"},
                "examDate": null,
                "startTime": null,
                "endTime": null,
                "place": "  "
            },
            {
                "id": 1002,
                "courseNameZh": "操作系统",
                "courseNameEn": "Operating Systems",
                "courseCode": "CS302",
                "lessonCode": "CS302.01",
                "examType": {"id": 1, "code": "01", "nameZh": "期末考试", "nameEn": "Final"},
                "cultivateType": {"id": 1, "code": "01", "nameZh": "主修", "nameEn": "Major"},
                "examDate": "2027-01-08",
                "weekDay": 5,
                "startTime": 1430,
                "endTime": 1630,
                "place": "主校区 1号教学楼 201",
                "placeEn": "Main Campus Bldg 1 Room 201",
                "seatNo": 24
            },
            {
                "id": 1001,
                "courseNameZh": "高等数学",
                "examType": {"id": 2, "code": "02", "nameZh": "补考", "nameEn": "Makeup"},
                "examDate": "2027-01-06",
                "startTime": 800,
                "endTime": 1000,
                "place": "主校区 2号教学楼 105",
                "seatNo": "08"
            }
        ]);

        let parsed: Vec<Exam> = serde_json::from_value(raw).unwrap();
        let exams = normalize_exams(parsed);
        assert_eq!(exams.len(), 3);

        // 已排定时间的考试按时间升序排在最前，待定考试排在末尾。
        assert_eq!(exams[0].id, Some(1001));
        assert_eq!(exams[0].course_name_zh, "高等数学");
        assert!(exams[0].is_scheduled());
        assert_eq!(exams[0].weekday, Some(3));
        assert_eq!(
            exams[0].start_time.as_ref().unwrap().to_string(),
            "2027-01-06T08:00:00+08:00[Asia/Shanghai]"
        );
        assert_eq!(
            exams[0].end_time.as_ref().unwrap().to_string(),
            "2027-01-06T10:00:00+08:00[Asia/Shanghai]"
        );
        assert_eq!(exams[0].seat_number.as_deref(), Some("08"));

        assert_eq!(exams[1].id, Some(1002));
        assert_eq!(exams[1].weekday, Some(5));
        assert_eq!(exams[1].seat_number.as_deref(), Some("24"));

        assert_eq!(exams[2].id, Some(1003));
        assert!(!exams[2].is_scheduled());
        assert_eq!(exams[2].date, None);
        assert_eq!(exams[2].place, None);

        let encoded = serde_json::to_value(&exams).unwrap();
        let restored: Vec<Exam> = serde_json::from_value(encoded).unwrap();
        assert_eq!(restored, exams);
    }

    #[test]
    fn exam_rejects_inverted_or_unpaired_times_and_empty_course_name() {
        for invalid in [
            json!({"courseNameZh": "  "}),
            json!({
                "courseNameZh": "高等数学",
                "examDate": "2027-01-06",
                "startTime": 1000,
                "endTime": 800
            }),
            json!({
                "courseNameZh": "高等数学",
                "examDate": "2027-01-06",
                "startTime": 800,
                "endTime": null
            }),
            json!({
                "courseNameZh": "高等数学",
                "weekDay": 8
            }),
        ] {
            assert!(serde_json::from_value::<Exam>(invalid).is_err());
        }
    }
}
