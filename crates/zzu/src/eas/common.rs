use std::fmt;

use jiff::{
    Zoned,
    civil::{Date, Time},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::{Error, Result};

/// 带中英文名称与编码的通用教务命名实体（如校区、教学楼、课程性质、开课院系）。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedEntity {
    /// 实体数字 ID。
    pub id: i64,
    /// 中文名称。
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub name_zh: String,
    /// 英文名称。
    #[serde(default)]
    pub name_en: Option<String>,
    /// 教务编码。
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub code: String,
}

/// 一天内的作息时段归属。
///
/// 序列化为统一的 `snake_case` 字符串，同时兼容反序列化教务上游的全大写标识；
/// 遇到未知时段标识时原样保留在 [`Self::Other`] 中。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DayPart {
    /// 上午（上游 `"MORNING"`，序列化为 `"morning"`）。
    Morning,
    /// 中午（上游 `"NOON"`，序列化为 `"noon"`，如午间加节）。
    Noon,
    /// 下午（上游 `"AFTERNOON"`，序列化为 `"afternoon"`）。
    Afternoon,
    /// 晚上（上游 `"EVENING"`，序列化为 `"evening"`）。
    Evening,
    /// 上游返回的其他未知时段标识。
    Other(String),
}

impl DayPart {
    /// 返回序列化使用的规范化字符串标识。
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Morning => "morning",
            Self::Noon => "noon",
            Self::Afternoon => "afternoon",
            Self::Evening => "evening",
            Self::Other(other) => other.as_str(),
        }
    }
}

impl fmt::Display for DayPart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<String> for DayPart {
    fn from(value: String) -> Self {
        match value.trim().to_ascii_uppercase().as_str() {
            "MORNING" => Self::Morning,
            "NOON" => Self::Noon,
            "AFTERNOON" => Self::Afternoon,
            "EVENING" => Self::Evening,
            _ => Self::Other(value),
        }
    }
}

impl Serialize for DayPart {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DayPart {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from(value))
    }
}

/// 单节作息时间与时段定义。
///
/// 表示每日作息表中的一个节次刻度（不绑定具体公历日期），既用于渲染课表纵轴，
/// 也用于将教室物理占用区间投影到标准节次网格。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", try_from = "RawCourseUnit")]
pub struct CourseUnit {
    /// 节次序号（从 1 开始，对应 [`super::ClassSession::start_unit`] 与 [`super::ClassSession::end_unit`]）。
    pub index: u8,
    /// 节次中文名称（常规作息为 `"1"`..`"10"`，含加节作息为 `"第一节"`、`"第+1节"` 等）。
    pub name_zh: String,
    /// 节次英文名称。
    #[serde(default)]
    pub name_en: Option<String>,
    /// 所属时段（上午、中午、下午或晚上）。
    pub day_part: DayPart,
    /// 连排大节分组序号（从 1 开始；同一大节内的相邻小节共享相同序号）。
    pub segment_index: u8,
    /// 本节上课时刻（墙上时钟，序列化为 ISO 8601 `HH:MM:SS` 字符串，如 `"08:00:00"`）。
    pub start_time: Time,
    /// 本节下课时刻（严格晚于 `start_time`）。
    pub end_time: Time,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCourseUnit {
    #[serde(alias = "indexNo")]
    index: u8,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    name_zh: String,
    #[serde(default)]
    name_en: Option<String>,
    day_part: DayPart,
    segment_index: u8,
    start_time: Value,
    end_time: Value,
}

impl TryFrom<RawCourseUnit> for CourseUnit {
    type Error = Error;

    fn try_from(raw: RawCourseUnit) -> Result<Self> {
        if raw.index == 0 {
            return Err(Error::decode_message("节次序号必须大于 0".into()));
        }
        if raw.segment_index == 0 {
            return Err(Error::decode_message("大节分组序号必须大于 0".into()));
        }
        let start_time = parse_wall_time(&raw.start_time)?;
        let end_time = parse_wall_time(&raw.end_time)?;
        if start_time >= end_time {
            return Err(Error::decode_message("节次结束时间必须晚于开始时间".into()));
        }
        let name_zh = trim_non_empty(Some(raw.name_zh)).unwrap_or_else(|| raw.index.to_string());
        Ok(Self {
            index: raw.index,
            name_zh,
            name_en: trim_non_empty(raw.name_en),
            day_part: raw.day_part,
            segment_index: raw.segment_index,
            start_time,
            end_time,
        })
    }
}

/// 按节次序号升序排列作息列表，并校验不存在重复序号。
pub(super) fn normalize_course_units(mut units: Vec<CourseUnit>) -> Result<Vec<CourseUnit>> {
    units.sort_by_key(|unit| unit.index);
    for pair in units.windows(2) {
        if pair[0].index == pair[1].index {
            return Err(Error::decode_message(format!(
                "作息表中存在重复的节次序号：{}",
                pair[0].index
            )));
        }
    }
    Ok(units)
}

/// 将上游 `HHMM`（数字或字符串）或 ISO 8601 时间字符串（如 `"08:00:00"`、`"08:00"`）解析为 [`Time`]。
pub(super) fn parse_wall_time(value: &Value) -> Result<Time> {
    let text = match value {
        Value::String(text) => text.trim().to_owned(),
        Value::Number(number) => number.to_string(),
        _ => {
            return Err(Error::decode_message(
                "时间必须为 HHMM 或 ISO 8601 时间字符串".into(),
            ));
        }
    };
    if text.contains(':') {
        return text.parse::<Time>().map_err(Error::decode);
    }
    if text.is_empty() || text.len() > 4 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::decode_message("时间必须为 HHMM".into()));
    }
    let hhmm: u32 = text
        .parse()
        .map_err(|_| Error::decode_message("时间格式无效".into()))?;
    let hour = i8::try_from(hhmm / 100).map_err(|_| Error::decode_message("小时值无效".into()))?;
    let minute =
        i8::try_from(hhmm % 100).map_err(|_| Error::decode_message("分钟值无效".into()))?;
    Time::new(hour, minute, 0, 0).map_err(Error::decode)
}

/// 将上游 `HHMM`（数字或字符串）或已编码的 RFC 9557 时间字符串解析为 `Asia/Shanghai` 时区的 [`Zoned`]。
pub(super) fn parse_time(date: Date, value: &Value) -> Result<Zoned> {
    if let Value::String(text) = value
        && let Ok(datetime) = text.trim().parse::<Zoned>()
    {
        return Ok(datetime);
    }
    let time = parse_wall_time(value)?;
    date.to_datetime(time)
        .in_tz("Asia/Shanghai")
        .map_err(Error::decode)
}

/// 将 JSON `null` 反序列化为 `T::default()`，避免上游空值字段导致解码中断。
pub(super) fn deserialize_null_default<'de, D, T>(
    deserializer: D,
) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// 去除首尾空白并在为空字符串时归一化为 `None`。
pub(super) fn trim_non_empty(text: Option<String>) -> Option<String> {
    text.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else if trimmed.len() == value.len() {
            Some(value)
        } else {
            Some(trimmed.to_owned())
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn course_unit_parses_upstream_format_and_roundtrips_json() {
        let raw = json!([
            {
                "nameZh": "  ",
                "nameEn": " 1 ",
                "timeTableLayoutAssoc": 1,
                "indexNo": 1,
                "startTime": 800,
                "endTime": 845,
                "dayPart": "MORNING",
                "color": "#EEFF00",
                "changeStartTime": 0,
                "changeEndTime": 0,
                "segmentIndex": 1,
                "name": "1"
            },
            {
                "nameZh": "第+2节",
                "nameEn": "",
                "indexNo": 6,
                "startTime": 1255,
                "endTime": 1340,
                "dayPart": "NOON",
                "segmentIndex": 3
            },
            {
                "nameZh": "8",
                "nameEn": null,
                "indexNo": 8,
                "startTime": "16:55",
                "endTime": "17:40",
                "dayPart": "AFTERNOON",
                "segmentIndex": 4
            },
            {
                "nameZh": "10",
                "nameEn": "10",
                "indexNo": 10,
                "startTime": 1955,
                "endTime": 2040,
                "dayPart": "EVENING",
                "segmentIndex": 5
            },
            {
                "nameZh": "加试",
                "indexNo": 15,
                "startTime": 2145,
                "endTime": 2230,
                "dayPart": "LATE_NIGHT",
                "segmentIndex": 9
            }
        ]);

        let units: Vec<CourseUnit> = serde_json::from_value(raw).unwrap();
        assert_eq!(units.len(), 5);
        assert_eq!(units[0].index, 1);
        assert_eq!(units[0].name_zh, "1");
        assert_eq!(units[0].name_en.as_deref(), Some("1"));
        assert_eq!(units[0].day_part, DayPart::Morning);
        assert_eq!(units[0].start_time.to_string(), "08:00:00");
        assert_eq!(units[0].end_time.to_string(), "08:45:00");

        assert_eq!(units[1].day_part, DayPart::Noon);
        assert_eq!(units[1].name_en, None);
        assert_eq!(units[2].day_part, DayPart::Afternoon);
        assert_eq!(units[3].day_part, DayPart::Evening);
        assert_eq!(units[3].end_time.to_string(), "20:40:00");
        assert_eq!(units[4].day_part, DayPart::Other("LATE_NIGHT".into()));

        let encoded = serde_json::to_value(&units).unwrap();
        assert_eq!(encoded[0]["index"], 1);
        assert_eq!(encoded[0]["dayPart"], "morning");
        assert_eq!(encoded[0]["startTime"], "08:00:00");
        assert!(encoded[0].get("indexNo").is_none());
        assert!(encoded[0].get("color").is_none());

        let restored: Vec<CourseUnit> = serde_json::from_value(encoded).unwrap();
        assert_eq!(restored, units);
    }

    #[test]
    fn course_unit_rejects_invalid_bounds_and_duplicate_indices() {
        for invalid in [
            json!({
                "indexNo": 0, "nameZh": "0", "dayPart": "MORNING",
                "segmentIndex": 1, "startTime": 800, "endTime": 845
            }),
            json!({
                "indexNo": 1, "nameZh": "1", "dayPart": "MORNING",
                "segmentIndex": 0, "startTime": 800, "endTime": 845
            }),
            json!({
                "indexNo": 1, "nameZh": "1", "dayPart": "MORNING",
                "segmentIndex": 1, "startTime": 845, "endTime": 845
            }),
            json!({
                "indexNo": 1, "nameZh": "1", "dayPart": "MORNING",
                "segmentIndex": 1, "startTime": 1055, "endTime": 1010
            }),
            json!({
                "indexNo": 1, "nameZh": "1", "dayPart": "MORNING",
                "segmentIndex": 1, "startTime": 2460, "endTime": 2500
            }),
        ] {
            assert!(serde_json::from_value::<CourseUnit>(invalid).is_err());
        }

        let duplicates: Vec<CourseUnit> = serde_json::from_value(json!([
            {
                "indexNo": 2, "nameZh": "2", "dayPart": "MORNING",
                "segmentIndex": 1, "startTime": 855, "endTime": 940
            },
            {
                "indexNo": 2, "nameZh": "2", "dayPart": "MORNING",
                "segmentIndex": 1, "startTime": 855, "endTime": 940
            }
        ]))
        .unwrap();
        assert!(normalize_course_units(duplicates).is_err());
    }
}
