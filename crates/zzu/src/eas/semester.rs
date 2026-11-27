use std::collections::BTreeSet;
use std::fmt;

use jiff::{ToSpan, civil::Date};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{Error, Result};

use super::common::trim_non_empty;

/// 学期季节标识。
///
/// 序列化为统一的 `snake_case` 字符串，反序列化时兼容教务上游的大写标识；
/// 遇到未列举的取值时通过 [`Self::Other`] 原样保留。
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(rename_all = "snake_case")
)]
pub enum SemesterSeason {
    /// 春季学期（上游 `"SPRING"`，序列化为 `"spring"`）。
    Spring,
    /// 秋季学期（上游 `"AUTUMN"`，序列化为 `"autumn"`）。
    Autumn,
    /// 夏季学期（上游 `"SUMMER"`，序列化为 `"summer"`）。
    Summer,
    /// 冬季学期（上游 `"WINTER"`，序列化为 `"winter"`）。
    Winter,
    /// 上游返回的其他未知季节标识。
    #[cfg_attr(feature = "schemars", schemars(untagged))]
    Other(String),
}

impl SemesterSeason {
    /// 返回规范化后的字符串标识。
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Spring => "spring",
            Self::Autumn => "autumn",
            Self::Summer => "summer",
            Self::Winter => "winter",
            Self::Other(other) => other.as_str(),
        }
    }
}

impl fmt::Display for SemesterSeason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<String> for SemesterSeason {
    fn from(value: String) -> Self {
        match value.trim().to_ascii_uppercase().as_str() {
            "SPRING" => Self::Spring,
            "AUTUMN" => Self::Autumn,
            "SUMMER" => Self::Summer,
            "WINTER" => Self::Winter,
            _ => Self::Other(value),
        }
    }
}

impl Serialize for SemesterSeason {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SemesterSeason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from(value))
    }
}

/// 教务学期校历元数据。
///
/// 支持本地换算教学周：先按 [`Self::week_start_on_sunday`] 将日期对齐到周首日，
/// 再结合 [`Self::start_date`]、[`Self::end_date`] 与 [`Self::week_indices`] 计算教学周序号。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(!try_from)
)]
#[serde(rename_all = "camelCase", try_from = "RawSemester")]
pub struct Semester {
    /// 学期数字 ID。
    pub id: i64,
    /// 学期代码（如 `"2026-2027-1"`）。
    pub code: String,
    /// 学期中文名称。
    pub name_zh: String,
    /// 学期英文名称。
    pub name_en: Option<String>,
    /// 所属学年（如 `"2026-2027"`）。
    pub school_year: String,
    /// 学期季节。
    pub season: SemesterSeason,
    /// 学期起始公历日期（含）。
    #[cfg_attr(feature = "schemars", schemars(with = "crate::schema::Date"))]
    pub start_date: Date,
    /// 学期结束公历日期（含）。
    #[cfg_attr(feature = "schemars", schemars(with = "crate::schema::Date"))]
    pub end_date: Date,
    /// 教学周是否以周日作为每周第一天。
    pub week_start_on_sunday: bool,
    /// 是否计入常规学期统计。
    pub count_in_term: bool,
    /// 本学期包含的教学周序号列表（按校历顺序排列，非零且唯一）。
    pub week_indices: Vec<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSemester {
    id: i64,
    code: String,
    name_zh: String,
    #[serde(default)]
    name_en: Option<String>,
    school_year: String,
    season: SemesterSeason,
    start_date: Date,
    end_date: Date,
    week_start_on_sunday: bool,
    count_in_term: bool,
    week_indices: Vec<u32>,
}

impl TryFrom<RawSemester> for Semester {
    type Error = Error;

    fn try_from(raw: RawSemester) -> Result<Self> {
        if raw.start_date > raw.end_date {
            return Err(Error::decode_message("学期结束日期不能早于起始日期".into()));
        }
        let mut seen = BTreeSet::new();
        for &week in &raw.week_indices {
            if week == 0 || !seen.insert(week) {
                return Err(Error::decode_message(
                    "学期教学周序号不能为 0 或重复".into(),
                ));
            }
        }
        Ok(Self {
            id: raw.id,
            code: raw.code,
            name_zh: raw.name_zh,
            name_en: trim_non_empty(raw.name_en),
            school_year: raw.school_year,
            season: raw.season,
            start_date: raw.start_date,
            end_date: raw.end_date,
            week_start_on_sunday: raw.week_start_on_sunday,
            count_in_term: raw.count_in_term,
            week_indices: raw.week_indices,
        })
    }
}

impl Semester {
    /// 计算公历日期 `date` 在本学期中对应的教学周序号。
    ///
    /// 当 `date` 位于本学期起止日期范围之外，或超出 [`Self::week_indices`] 覆盖范围时返回 `None`。
    #[must_use]
    pub fn week_of(&self, date: Date) -> Option<u32> {
        if date < self.start_date || date > self.end_date {
            return None;
        }
        let start_anchor = week_start(self.start_date, self.week_start_on_sunday)?;
        let date_anchor = week_start(date, self.week_start_on_sunday)?;
        let days = start_anchor.until(date_anchor).ok()?.get_days();
        if days < 0 {
            return None;
        }
        let offset = usize::try_from(days / 7).ok()?;
        self.week_indices.get(offset).copied()
    }

    /// 返回教学周 `week_index` 对应的完整 7 天公历日期闭区间 `(周首日, 周末日)`。
    ///
    /// 周首日依据 [`Self::week_start_on_sunday`] 确定（周一或周日）；若本学期不含该周次则返回 `None`。
    #[must_use]
    pub fn week_date_range(&self, week_index: u32) -> Option<(Date, Date)> {
        let position = self
            .week_indices
            .iter()
            .position(|&week| week == week_index)?;
        let start_anchor = week_start(self.start_date, self.week_start_on_sunday)?;
        let offset_days = i64::try_from(position).ok()?.checked_mul(7)?;
        let first = start_anchor.checked_add(offset_days.days()).ok()?;
        let last = first.checked_add(6.days()).ok()?;
        Some((first, last))
    }

    /// 在学期列表中查找公历日期 `date` 所属的学期与教学周序号。
    ///
    /// 多个学期包含同一日期时，优先匹配 `count_in_term` 为 `true` 的常规学期；假期等非学期日期返回 `None`。
    #[must_use]
    pub fn locate_date(semesters: &[Self], date: Date) -> Option<(&Self, u32)> {
        semesters
            .iter()
            .filter(|semester| semester.count_in_term)
            .find_map(|semester| semester.week_of(date).map(|week| (semester, week)))
            .or_else(|| {
                semesters
                    .iter()
                    .filter(|semester| !semester.count_in_term)
                    .find_map(|semester| semester.week_of(date).map(|week| (semester, week)))
            })
    }
}

fn week_start(date: Date, week_start_on_sunday: bool) -> Option<Date> {
    let offset = if week_start_on_sunday {
        date.weekday().to_sunday_zero_offset()
    } else {
        date.weekday().to_monday_zero_offset()
    };
    date.checked_sub(i64::from(offset).days()).ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_semester(
        id: i64,
        start_date: &str,
        end_date: &str,
        week_start_on_sunday: bool,
        weeks: u32,
    ) -> Semester {
        serde_json::from_value(json!({
            "id": id,
            "code": format!("sem-{id}"),
            "nameZh": format!("学期-{id}"),
            "nameEn": null,
            "schoolYear": "2026-2027",
            "season": "AUTUMN",
            "startDate": start_date,
            "endDate": end_date,
            "weekStartOnSunday": week_start_on_sunday,
            "countInTerm": true,
            "weekIndices": (1..=weeks).collect::<Vec<_>>(),
            "bizTypes": null
        }))
        .unwrap()
    }

    #[test]
    fn week_of_handles_monday_start_cross_year_and_boundaries() {
        // 对应真实 2026-2027-1 学期（id=194）。
        let semester = sample_semester(194, "2026-09-07", "2027-01-17", false, 19);
        assert_eq!(semester.week_of("2026-09-06".parse().unwrap()), None);
        assert_eq!(semester.week_of("2026-09-07".parse().unwrap()), Some(1));
        assert_eq!(semester.week_of("2026-09-13".parse().unwrap()), Some(1));
        assert_eq!(semester.week_of("2026-09-14".parse().unwrap()), Some(2));
        assert_eq!(semester.week_of("2026-11-23".parse().unwrap()), Some(12));
        // 跨年后周一至周日均应保持连续周次，无上游跨年减 1 缺陷。
        assert_eq!(semester.week_of("2027-01-04".parse().unwrap()), Some(18));
        assert_eq!(semester.week_of("2027-01-05".parse().unwrap()), Some(18));
        assert_eq!(semester.week_of("2027-01-10".parse().unwrap()), Some(18));
        assert_eq!(semester.week_of("2027-01-17".parse().unwrap()), Some(19));
        assert_eq!(semester.week_of("2027-01-18".parse().unwrap()), None);
    }

    #[test]
    fn week_of_aligns_partial_first_week_for_both_monday_and_sunday_starts() {
        // 2015-09-01 为周二、周一为周首日，共 22 周（对应真实历史学期 id=8）。
        let tuesday_start = sample_semester(8, "2015-09-01", "2016-01-31", false, 22);
        assert_eq!(
            tuesday_start.week_of("2015-09-01".parse().unwrap()),
            Some(1)
        );
        assert_eq!(
            tuesday_start.week_of("2015-09-06".parse().unwrap()),
            Some(1)
        );
        assert_eq!(
            tuesday_start.week_of("2015-09-07".parse().unwrap()),
            Some(2)
        );
        assert_eq!(
            tuesday_start.week_of("2016-01-31".parse().unwrap()),
            Some(22)
        );

        // 1997-02-01 为周六、周日为周首日，共 27 周（对应真实历史学期 id=50）。
        let sunday_start = sample_semester(50, "1997-02-01", "1997-07-31", true, 27);
        assert_eq!(sunday_start.week_of("1997-02-01".parse().unwrap()), Some(1));
        assert_eq!(sunday_start.week_of("1997-02-02".parse().unwrap()), Some(2));
        assert_eq!(
            sunday_start.week_of("1997-07-31".parse().unwrap()),
            Some(27)
        );
    }

    #[test]
    fn week_date_range_inverts_week_of_and_locates_across_semesters() {
        let autumn = sample_semester(194, "2026-09-07", "2027-01-17", false, 19);
        let spring = sample_semester(214, "2027-02-22", "2027-07-04", false, 19);

        let (start, end) = autumn.week_date_range(12).unwrap();
        assert_eq!(start, "2026-11-23".parse::<Date>().unwrap());
        assert_eq!(end, "2026-11-29".parse::<Date>().unwrap());
        assert_eq!(autumn.week_of(start), Some(12));
        assert_eq!(autumn.week_of(end), Some(12));
        assert_eq!(autumn.week_date_range(20), None);

        let all = [spring, autumn];
        let (matched, week) = Semester::locate_date(&all, "2026-11-23".parse().unwrap()).unwrap();
        assert_eq!(matched.id, 194);
        assert_eq!(week, 12);
        // 寒假间隙不属于任何学期。
        assert!(Semester::locate_date(&all, "2027-02-01".parse().unwrap()).is_none());
    }

    #[test]
    fn semester_normalizes_season_drops_unknown_fields_and_rejects_invalid_data() {
        let semester = sample_semester(194, "2026-09-07", "2027-01-17", false, 19);
        assert_eq!(semester.season, SemesterSeason::Autumn);
        let serialized = serde_json::to_value(&semester).unwrap();
        assert_eq!(serialized["season"], "autumn");
        assert!(serialized.get("bizTypes").is_none());

        let restored: Semester = serde_json::from_value(serialized).unwrap();
        assert_eq!(restored, semester);

        // 非法起止日期与 0 / 重复周次应被拒绝。
        let inverted = json!({
            "id": 1, "code": "x", "nameZh": "x", "schoolYear": "2026-2027",
            "season": "SPRING", "startDate": "2027-02-01", "endDate": "2027-01-01",
            "weekStartOnSunday": false, "countInTerm": true, "weekIndices": [1]
        });
        assert!(serde_json::from_value::<Semester>(inverted).is_err());

        let dup_weeks = json!({
            "id": 1, "code": "x", "nameZh": "x", "schoolYear": "2026-2027",
            "season": "SPRING", "startDate": "2027-01-01", "endDate": "2027-02-01",
            "weekStartOnSunday": false, "countInTerm": true, "weekIndices": [1, 1]
        });
        assert!(serde_json::from_value::<Semester>(dup_weeks).is_err());
    }
}
