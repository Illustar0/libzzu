use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fmt;

use jiff::civil::Date;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{Error, Result};

use super::{
    common::{NamedEntity, deserialize_null_default, trim_non_empty},
    semester::SemesterSeason,
};

/// 课程修读性质（如必修、选修）。
///
/// 序列化为统一的 `snake_case` 字符串，反序列化时兼容中英文文本标识。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StudyType {
    /// 必修（上游 `"必修"` / `"Required"`，序列化为 `"required"`）。
    Required,
    /// 选修（上游 `"选修"` / `"Elective"`，序列化为 `"elective"`）。
    Elective,
    /// 上游返回的其他未知修读性质文本。
    Other(String),
}

impl StudyType {
    /// 返回规范化后的字符串标识。
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Required => "required",
            Self::Elective => "elective",
            Self::Other(other) => other.as_str(),
        }
    }
}

impl fmt::Display for StudyType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<String> for StudyType {
    fn from(value: String) -> Self {
        let trimmed = value.trim();
        if trimmed.eq_ignore_ascii_case("required") || trimmed == "必修" {
            Self::Required
        } else if trimmed.eq_ignore_ascii_case("elective") || trimmed == "选修" {
            Self::Elective
        } else {
            Self::Other(trimmed.to_owned())
        }
    }
}

impl Serialize for StudyType {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for StudyType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from(value))
    }
}

struct RawStudyType(StudyType);

impl<'de> Deserialize<'de> for RawStudyType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct LocalizedText {
            #[serde(default)]
            text_zh: Option<String>,
            #[serde(default)]
            text: Option<String>,
            #[serde(default)]
            text_en: Option<String>,
        }

        match serde_json::Value::deserialize(deserializer)? {
            serde_json::Value::String(text) => Ok(Self(StudyType::from(text))),
            value @ serde_json::Value::Object(_) => {
                let localized: LocalizedText =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                let chosen = trim_non_empty(localized.text_zh)
                    .or_else(|| trim_non_empty(localized.text))
                    .or_else(|| trim_non_empty(localized.text_en))
                    .ok_or_else(|| serde::de::Error::custom("studyType 缺少有效文本"))?;
                Ok(Self(StudyType::from(chosen)))
            }
            _ => Err(serde::de::Error::custom("studyType 必须为字符串或对象")),
        }
    }
}

/// 课程成绩记录的展示与考核状态。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum GradeState {
    /// 常规状态（无评教锁定或待缓考标记，序列化为 `"normal"`）。
    #[default]
    Normal,
    /// 尚未完成网上评教，教务系统隐藏成绩与绩点（上游 `gradeDisplayShowStatus == "surveyLesson"`，序列化为 `"evaluation_required"`）。
    EvaluationRequired,
    /// 已办理缓考且尚未参加考试（上游 `state == "WAIT_FOR_DELAY"`，序列化为 `"deferred_pending"`）。
    DeferredPending,
    /// 上游返回的其他未知状态标识。
    Other(String),
}

impl GradeState {
    /// 返回规范化后的字符串标识。
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Normal => "normal",
            Self::EvaluationRequired => "evaluation_required",
            Self::DeferredPending => "deferred_pending",
            Self::Other(other) => other.as_str(),
        }
    }
}

impl fmt::Display for GradeState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<String> for GradeState {
    fn from(value: String) -> Self {
        match value.trim() {
            "" | "normal" | "NORMAL" => Self::Normal,
            "evaluation_required" | "surveyLesson" => Self::EvaluationRequired,
            "deferred_pending" | "WAIT_FOR_DELAY" => Self::DeferredPending,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl Serialize for GradeState {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for GradeState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from(value))
    }
}

/// 成绩单条目关联的精简学期元数据。
///
/// 教务成绩接口内嵌的学期对象不含教学周数组 `weekIndices`，因此独立于 [`super::Semester`] 建模；
/// 可通过 [`Self::id`] 与 [`super::Semester::id`] 关联。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", try_from = "RawGradeSemester")]
pub struct GradeSemester {
    /// 学期数字 ID。
    pub id: i64,
    /// 学期代码（如 `"2024-2025-1"`）。
    pub code: String,
    /// 学期中文名称。
    pub name_zh: String,
    /// 学期英文名称。
    #[serde(default)]
    pub name_en: Option<String>,
    /// 所属学年（如 `"2024-2025"`）。
    pub school_year: String,
    /// 学期季节。
    pub season: SemesterSeason,
    /// 学期起始公历日期（含）。
    pub start_date: Date,
    /// 学期结束公历日期（含）。
    pub end_date: Date,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawGradeSemester {
    id: i64,
    code: String,
    name_zh: String,
    #[serde(default)]
    name_en: Option<String>,
    school_year: String,
    season: SemesterSeason,
    start_date: Date,
    end_date: Date,
}

impl TryFrom<RawGradeSemester> for GradeSemester {
    type Error = Error;

    fn try_from(raw: RawGradeSemester) -> Result<Self> {
        if raw.start_date > raw.end_date {
            return Err(Error::decode_message(
                "成绩所属学期的结束日期不能早于起始日期".into(),
            ));
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
        })
    }
}

/// 过程性考核细项成绩（如考勤、课堂测验、实验报告）。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessGrade {
    /// 细项名称。
    pub name: String,
    /// 细项成绩文本（如 `"95"`、`"优"`、`"缓考"`）。
    #[serde(default)]
    pub score: Option<String>,
}

/// 课程分项成绩（如平时成绩、实验成绩、期末成绩）。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubGrade {
    /// 分项成绩类别（如 `"平时成绩"`、`"期末成绩"`、`"实验成绩"`）。
    pub kind: NamedEntity,
    /// 分项成绩文本（如 `"91"`、`"缓考"`）。
    #[serde(default)]
    pub score: Option<String>,
    /// 分项成绩英文文本。
    #[serde(default)]
    pub score_en: Option<String>,
    /// 教务端是否将该分项标记为已对学生公开。
    pub is_published: bool,
    /// 该分项下的过程性考核细项列表。
    #[serde(default)]
    pub items: Vec<ProcessGrade>,
}

/// 专业年级绩点排名统计。
///
/// 满足不变量 `1 <= rank <= total`。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", try_from = "RawRankPair")]
pub struct GpaRank {
    /// 当前学生在专业年级中的排名（从 1 开始，不超过 `total`）。
    pub rank: u32,
    /// 参与排名的专业年级总人数（大于 0）。
    pub total: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRankPair {
    rank: u32,
    total: u32,
}

impl TryFrom<RawRankPair> for GpaRank {
    type Error = Error;

    fn try_from(raw: RawRankPair) -> Result<Self> {
        if raw.rank == 0 || raw.total == 0 || raw.rank > raw.total {
            return Err(Error::decode_message(
                "绩点排名必须满足 1 <= rank <= total".into(),
            ));
        }
        Ok(Self {
            rank: raw.rank,
            total: raw.total,
        })
    }
}

/// 单门课程修读成绩记录。
///
/// 已剥离上游响应中逐条重复的学生档案与教学班排课冗余结构；当 [`Self::state`] 为
/// [`GradeState::EvaluationRequired`]（未评教锁定）时，成绩、绩点、通过状态与分项明细均置为空，
/// 避免将上游占位零值误判为挂科。
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CourseGrade {
    /// 修读所属学期。
    pub semester: GradeSemester,
    /// 课程数字 ID（映射自上游 `lesson.courseAssoc`）。
    #[serde(default)]
    pub course_id: Option<i64>,
    /// 课程代码（如 `"CS101"`）。
    pub course_code: String,
    /// 课程中文名称。
    pub course_name_zh: String,
    /// 课程英文名称。
    #[serde(default)]
    pub course_name_en: Option<String>,
    /// 教学班数字 ID（映射自上游 `lesson.id`）。
    #[serde(default)]
    pub class_id: Option<i64>,
    /// 教学班选课课号（如 `"CS101.01"`）。
    #[serde(default)]
    pub class_code: Option<String>,
    /// 课程类别（如必修课、限选课、分类通识课、校任选课）。
    #[serde(default)]
    pub course_type: Option<NamedEntity>,
    /// 课程属性（上游提供时返回）。
    #[serde(default)]
    pub course_property: Option<NamedEntity>,
    /// 培养类别（如主修、辅修）。
    #[serde(default)]
    pub cultivate_type: Option<NamedEntity>,
    /// 课程修读性质（如必修、选修）。
    #[serde(default)]
    pub study_type: Option<StudyType>,
    /// 课程学分（未评教锁定时为 `None`）。
    #[serde(default)]
    pub credits: Option<f64>,
    /// 最终总评成绩文本（如 `"85"`、`"优"`、`"B"`；未出分或未评教锁定时为 `None`）。
    #[serde(default)]
    pub final_score: Option<String>,
    /// 该课程获得的绩点（无绩点制课程、未出分或未评教锁定时为 `None`）。
    #[serde(default)]
    pub grade_point: Option<f64>,
    /// 是否通过考核（未出分或未评教锁定时为 `None`）。
    #[serde(default)]
    pub is_passed: Option<bool>,
    /// 总评成绩是否已发布。
    pub is_published: bool,
    /// 是否为重修记录。
    pub is_retake: bool,
    /// 成绩展示与考核特殊状态（常规、未评教锁定或缓考待考）。
    #[serde(default)]
    pub state: GradeState,
    /// 补考成绩文本（未参加补考或未评教锁定时为 `None`）。
    #[serde(default)]
    pub makeup_score: Option<String>,
    /// 缓考成绩文本（未参加缓考或未评教锁定时为 `None`）。
    #[serde(default)]
    pub deferred_score: Option<String>,
    /// 教务汇总的成绩构成说明（如 `"期末成绩:45;补考:60"`）。
    #[serde(default)]
    pub score_detail: Option<String>,
    /// 分项成绩明细列表。
    #[serde(default)]
    pub sub_grades: Vec<SubGrade>,
}

/// 学生完整成绩单与累计绩点排名快照。
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transcript {
    /// 累计平均学分绩点（尚未产生有效绩点时为 `None`）。
    #[serde(default)]
    pub gpa: Option<f64>,
    /// 专业年级累计绩点排名（未开放排名或尚无有效排名数据时为 `None`）。
    #[serde(default)]
    pub rank: Option<GpaRank>,
    /// 按学期起始日期倒序（最新学期在前）、同学期内按课程代码升序排列的课程成绩列表。
    #[serde(default)]
    pub grades: Vec<CourseGrade>,
}

impl Transcript {
    /// 筛选指定学期 ID 下的全部课程成绩引用，保持原有顺序。
    #[must_use]
    pub fn semester_grades(&self, semester_id: i64) -> Vec<&CourseGrade> {
        self.grades
            .iter()
            .filter(|grade| grade.semester.id == semester_id)
            .collect()
    }

    /// 统计已通过考核的课程总学分（同一 `course_code` 多次修读通过时取最高学分去重，排除未评教锁定记录）。
    #[must_use]
    pub fn earned_credits(&self) -> f64 {
        let mut passed_by_course: BTreeMap<&str, f64> = BTreeMap::new();
        for grade in &self.grades {
            if grade.state == GradeState::EvaluationRequired || grade.is_passed != Some(true) {
                continue;
            }
            if let Some(credits) = grade.credits.filter(|&value| value > 0.0) {
                passed_by_course
                    .entry(&grade.course_code)
                    .and_modify(|existing| *existing = existing.max(credits))
                    .or_insert(credits);
            }
        }
        passed_by_course.into_values().sum()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RawGpaRank {
    #[serde(default)]
    gpa: Option<f64>,
    #[serde(default)]
    rank: Option<u32>,
    #[serde(default)]
    total: Option<u32>,
    #[serde(default = "default_true")]
    flag: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawScoreResult {
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    result_en: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawProcessGrade {
    #[serde(default, deserialize_with = "deserialize_null_default")]
    name: String,
    #[serde(default)]
    result: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSubGrade {
    sub_grade_type: NamedEntity,
    #[serde(default)]
    published: bool,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    result_en: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    process_grade_search_vms: Vec<RawProcessGrade>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawGradeLesson {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    course_assoc: Option<i64>,
    #[serde(default)]
    course_type: Option<NamedEntity>,
    #[serde(default)]
    course_property: Option<NamedEntity>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RawCourseGrade {
    semester: GradeSemester,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    course_code: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    course_name_zh: String,
    #[serde(default)]
    course_name_en: Option<String>,
    #[serde(default)]
    lesson_code: Option<String>,
    #[serde(default)]
    lesson: Option<RawGradeLesson>,
    #[serde(default)]
    cultivate_type: Option<NamedEntity>,
    #[serde(default)]
    study_type: Option<RawStudyType>,
    #[serde(default)]
    credits: Option<f64>,
    #[serde(default)]
    final_grade: Option<String>,
    #[serde(default)]
    gp: Option<f64>,
    #[serde(default)]
    passed: Option<bool>,
    #[serde(default)]
    published: bool,
    #[serde(default)]
    retake: bool,
    #[serde(default)]
    grade_display_show_status: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    makeup_grade: Option<RawScoreResult>,
    #[serde(default)]
    delay_grade: Option<RawScoreResult>,
    #[serde(default)]
    grade_detail: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    sub_grades: Vec<RawSubGrade>,
}

pub(super) fn build_transcript(
    raw_grades: Vec<RawCourseGrade>,
    raw_gpa_rank: Option<RawGpaRank>,
) -> Transcript {
    let (gpa, rank) = match raw_gpa_rank {
        Some(info) => {
            let gpa = info.gpa.filter(|value| value.is_finite());
            let rank = if info.flag {
                match (info.rank, info.total) {
                    (Some(rank), Some(total)) if rank > 0 && total > 0 && rank <= total => {
                        Some(GpaRank { rank, total })
                    }
                    _ => None,
                }
            } else {
                None
            };
            (gpa, rank)
        }
        None => (None, None),
    };

    let mut grades: Vec<CourseGrade> = raw_grades.into_iter().map(into_course_grade).collect();
    grades.sort_by(|left, right| {
        (
            Reverse(left.semester.start_date),
            Reverse(left.semester.id),
            &left.course_code,
            &left.class_code,
        )
            .cmp(&(
                Reverse(right.semester.start_date),
                Reverse(right.semester.id),
                &right.course_code,
                &right.class_code,
            ))
    });

    Transcript { gpa, rank, grades }
}

fn into_course_grade(raw: RawCourseGrade) -> CourseGrade {
    let state = resolve_grade_state(
        raw.grade_display_show_status.as_deref(),
        raw.state.as_deref(),
    );
    let is_locked = state == GradeState::EvaluationRequired;
    let (course_id, class_id, course_type, course_property) = match raw.lesson {
        Some(lesson) => (
            lesson.course_assoc,
            lesson.id,
            lesson.course_type,
            lesson.course_property,
        ),
        None => (None, None, None, None),
    };

    let sub_grades = if is_locked {
        Vec::new()
    } else {
        raw.sub_grades
            .into_iter()
            .map(|sub| SubGrade {
                kind: sub.sub_grade_type,
                score: normalize_score(sub.result),
                score_en: trim_non_empty(sub.result_en),
                is_published: sub.published,
                items: sub
                    .process_grade_search_vms
                    .into_iter()
                    .filter_map(|item| {
                        let name = trim_non_empty(Some(item.name))?;
                        Some(ProcessGrade {
                            name,
                            score: normalize_score(item.result),
                        })
                    })
                    .collect(),
            })
            .collect()
    };

    CourseGrade {
        semester: raw.semester,
        course_id,
        course_code: raw.course_code.trim().to_owned(),
        course_name_zh: raw.course_name_zh.trim().to_owned(),
        course_name_en: trim_non_empty(raw.course_name_en),
        class_id,
        class_code: trim_non_empty(raw.lesson_code),
        course_type,
        course_property,
        cultivate_type: raw.cultivate_type,
        study_type: raw.study_type.map(|item| item.0),
        credits: if is_locked {
            None
        } else {
            raw.credits.filter(|value| value.is_finite())
        },
        final_score: if is_locked {
            None
        } else {
            normalize_score(raw.final_grade)
        },
        grade_point: if is_locked {
            None
        } else {
            raw.gp.filter(|value| value.is_finite())
        },
        is_passed: if is_locked { None } else { raw.passed },
        is_published: !is_locked && raw.published,
        is_retake: raw.retake,
        state,
        makeup_score: if is_locked {
            None
        } else {
            raw.makeup_grade.and_then(score_text)
        },
        deferred_score: if is_locked {
            None
        } else {
            raw.delay_grade.and_then(score_text)
        },
        score_detail: if is_locked {
            None
        } else {
            trim_non_empty(raw.grade_detail)
        },
        sub_grades,
    }
}

fn resolve_grade_state(show_status: Option<&str>, raw_state: Option<&str>) -> GradeState {
    if show_status.is_some_and(|status| status.trim() == "surveyLesson") {
        return GradeState::EvaluationRequired;
    }
    match raw_state.map(str::trim).filter(|state| !state.is_empty()) {
        Some("WAIT_FOR_DELAY") => GradeState::DeferredPending,
        Some(other) => GradeState::from(other.to_owned()),
        None => GradeState::Normal,
    }
}

fn score_text(raw: RawScoreResult) -> Option<String> {
    normalize_score(raw.result).or_else(|| normalize_score(raw.result_en))
}

fn normalize_score(text: Option<String>) -> Option<String> {
    let trimmed = trim_non_empty(text)?;
    if trimmed.contains("MAKEUP") {
        Some("补考".into())
    } else if trimmed.contains("DELAYED") {
        Some("缓考".into())
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn semester_fixture(
        id: i64,
        code: &str,
        year: &str,
        start: &str,
        end: &str,
    ) -> serde_json::Value {
        json!({
            "id": id, "code": code, "nameZh": code, "nameEn": code,
            "schoolYear": year, "season": "AUTUMN", "startDate": start, "endDate": end,
        })
    }

    fn sample_raw_grades() -> Vec<RawCourseGrade> {
        let sem_1 = semester_fixture(101, "2024-2025-1", "2024-2025", "2024-09-09", "2025-01-17");
        let sem_2 = semester_fixture(102, "2025-2026-1", "2025-2026", "2025-09-08", "2026-01-18");
        let sem_3 = semester_fixture(103, "2026-2027-1", "2026-2027", "2026-09-07", "2027-01-17");
        serde_json::from_value(json!([
            {
                "studentCode": "202400000001",
                "cultivateType": {"id": 1, "code": "01", "nameZh": "主修", "nameEn": "Major"},
                "courseNameZh": "程序设计基础",
                "courseNameEn": "Fundamentals of Programming",
                "courseCode": "CS101",
                "lessonCode": "CS101.01",
                "lesson": {
                    "id": 10_001, "courseAssoc": 2001,
                    "courseType": {"id": 6, "code": "04", "nameZh": "必修课", "nameEn": "Obligatory"},
                },
                "semester": sem_1,
                "credits": 4.0, "finalGrade": "55", "gp": 0.0,
                "passed": false, "published": true, "retake": false,
                "gradeDisplayShowStatus": "makeupGrade",
                "gradeDetail": "期末成绩:45;补考:55",
                "makeupGrade": {"result": "55", "resultEn": "55"},
                "studyType": {"text": "必修", "textZh": "必修", "textEn": "Required"},
                "subGrades": [{
                    "published": false, "result": "45",
                    "subGradeType": {"id": 2, "code": "02", "nameZh": "期末成绩", "nameEn": "M"},
                    "processGradeSearchVms": [{"name": "卷面", "result": "45"}]
                }]
            },
            {
                "courseNameZh": "程序设计基础", "courseCode": "CS101", "lessonCode": "CS101.02",
                "lesson": {"id": 10_002, "courseAssoc": 2001},
                "semester": sem_2,
                "credits": 4.0, "finalGrade": "82", "gp": 3.2,
                "passed": true, "published": true, "retake": true,
                "studyType": {"text": "必修", "textZh": "必修", "textEn": "Required"},
                "subGrades": []
            },
            {
                "courseNameZh": "科技史导论", "courseCode": "GE201",
                "semester": sem_2,
                "credits": 2.0, "finalGrade": "优", "gp": null,
                "passed": true, "published": true, "retake": false,
                "studyType": {"text": "选修", "textZh": "选修", "textEn": "Elective"},
                "subGrades": []
            },
            {
                "courseNameZh": "数据结构", "courseCode": "CS201",
                "semester": sem_3,
                "credits": 3.0, "finalGrade": "0", "gp": 0.0,
                "passed": false, "published": true, "retake": false,
                "gradeDisplayShowStatus": "surveyLesson",
                "subGrades": [{
                    "published": true, "result": "90",
                    "subGradeType": {"id": 2, "code": "02", "nameZh": "期末成绩"}
                }]
            }
        ]))
        .unwrap()
    }

    #[test]
    fn transcript_dehydrates_sorts_masks_unevaluated_courses_and_roundtrips() {
        let raw_rank: RawGpaRank = serde_json::from_value(json!({
            "gpa": 3.65,
            "rank": 15,
            "total": 120,
            "flag": true
        }))
        .unwrap();
        let transcript = build_transcript(sample_raw_grades(), Some(raw_rank));

        assert_eq!(transcript.gpa, Some(3.65));
        assert_eq!(
            transcript.rank,
            Some(GpaRank {
                rank: 15,
                total: 120
            })
        );
        assert_eq!(transcript.grades.len(), 4);

        // 最新学期（103）排最前，且未评教课程已屏蔽占位零分与子项。
        let locked = &transcript.grades[0];
        assert_eq!(locked.semester.id, 103);
        assert_eq!(locked.state, GradeState::EvaluationRequired);
        assert_eq!(locked.credits, None);
        assert_eq!(locked.final_score, None);
        assert_eq!(locked.grade_point, None);
        assert_eq!(locked.is_passed, None);
        assert!(!locked.is_published);
        assert_eq!(locked.sub_grades, Vec::new());

        // 同一学期（102）内按课程代码升序排列：CS101 在前，GE201 在后。
        assert_eq!(transcript.grades[1].course_code, "CS101");
        assert!(transcript.grades[1].is_retake);
        assert_eq!(transcript.grades[1].study_type, Some(StudyType::Required));
        assert_eq!(transcript.grades[2].course_code, "GE201");
        assert_eq!(transcript.grades[2].grade_point, None);
        assert_eq!(transcript.grades[2].study_type, Some(StudyType::Elective));

        // 补考记录与分项细项保留。
        let first_attempt = &transcript.grades[3];
        assert_eq!(first_attempt.semester.id, 101);
        assert_eq!(first_attempt.makeup_score.as_deref(), Some("55"));
        assert_eq!(first_attempt.sub_grades.len(), 1);
        assert_eq!(first_attempt.sub_grades[0].score.as_deref(), Some("45"));
        assert_eq!(first_attempt.sub_grades[0].items[0].name, "卷面");

        // 已修得学分：CS101（重修通过 4.0）+ GE201（通过 2.0）= 6.0。
        assert!((transcript.earned_credits() - 6.0).abs() < f64::EPSILON);
        assert_eq!(transcript.semester_grades(102).len(), 2);

        let encoded = serde_json::to_value(&transcript).unwrap();
        let restored: Transcript = serde_json::from_value(encoded).unwrap();
        assert_eq!(restored, transcript);
        assert!(serde_json::from_value::<StudyType>(json!({"textZh": "必修"})).is_err());
    }

    #[test]
    fn rank_and_special_states_handle_disabled_flags_and_invalid_bounds() {
        for hidden_rank in [
            json!({"gpa": 3.2, "rank": 10, "total": 100, "flag": false}),
            json!({"gpa": 3.2, "rank": 0, "total": 100, "flag": true}),
            json!({"gpa": 3.2, "rank": null, "total": null, "flag": true}),
            json!({"gpa": null, "rank": 105, "total": 100, "flag": true}),
        ] {
            let parsed: RawGpaRank = serde_json::from_value(hidden_rank).unwrap();
            let transcript = build_transcript(Vec::new(), Some(parsed));
            assert_eq!(transcript.rank, None);
        }

        assert!(serde_json::from_value::<GpaRank>(json!({"rank": 0, "total": 10})).is_err());
        assert!(serde_json::from_value::<GpaRank>(json!({"rank": 11, "total": 10})).is_err());

        let deferred_raw: Vec<RawCourseGrade> = serde_json::from_value(json!([{
            "courseCode": "PHYS101",
            "courseNameZh": "大学物理",
            "semester": {
                "id": 103,
                "code": "2026-2027-1",
                "nameZh": "2026-2027-1",
                "schoolYear": "2026-2027",
                "season": "AUTUMN",
                "startDate": "2026-09-07",
                "endDate": "2027-01-17"
            },
            "finalGrade": "DELAYED",
            "state": "WAIT_FOR_DELAY",
            "published": false,
            "retake": false
        }]))
        .unwrap();
        let transcript = build_transcript(deferred_raw, None);
        assert_eq!(transcript.grades[0].state, GradeState::DeferredPending);
        assert_eq!(transcript.grades[0].final_score.as_deref(), Some("缓考"));
    }
}
