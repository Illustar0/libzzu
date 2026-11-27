use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::common::{NamedEntity, deserialize_null_default, trim_non_empty};

/// 培养方案模块或课程的修读完成状态。
///
/// 序列化为统一的 `snake_case` 字符串；反序列化时同时兼容中英文原始状态词（如将上游 `"unrepaired"` / `"未修"` 统一归一化为 `"not_taken"`）。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[cfg_attr(
    feature = "schemars",
    derive(schemars::JsonSchema),
    schemars(rename_all = "snake_case")
)]
pub enum CompletionStatus {
    /// 已通过（上游 `"passed"` / `"通过"`，序列化为 `"passed"`）。
    Passed,
    /// 未通过（上游 `"failed"` / `"不通过"` / `"未通过"`，序列化为 `"failed"`）。
    Failed,
    /// 修读中（上游 `"taking"` / `"learning"` / `"在修"` / `"修读中"`，序列化为 `"in_progress"`）。
    InProgress,
    /// 尚未修读（上游 `"unrepaired"` / `"unread"` / `"未修"`，序列化为 `"not_taken"`）。
    #[default]
    NotTaken,
    /// 免修（上游 `"exempt"` / `"skip"` / `"免修"`，序列化为 `"exempt"`）。
    Exempt,
    /// 上游返回的其他未知状态标识。
    #[cfg_attr(feature = "schemars", schemars(untagged))]
    Other(String),
}

impl CompletionStatus {
    /// 返回规范化后的字符串标识。
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::InProgress => "in_progress",
            Self::NotTaken => "not_taken",
            Self::Exempt => "exempt",
            Self::Other(other) => other.as_str(),
        }
    }
}

impl fmt::Display for CompletionStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<String> for CompletionStatus {
    fn from(value: String) -> Self {
        let trimmed = value.trim();
        let lower = trimmed.to_ascii_lowercase();
        match lower.as_str() {
            "passed" | "pass" => Self::Passed,
            "failed" | "fail" => Self::Failed,
            "in_progress" | "taking" | "learning" => Self::InProgress,
            "" | "not_taken" | "unrepaired" | "unread" => Self::NotTaken,
            "exempt" | "skip" => Self::Exempt,
            _ => match trimmed {
                "通过" | "合格" => Self::Passed,
                "不通过" | "未通过" | "不合格" => Self::Failed,
                "在修" | "修读中" => Self::InProgress,
                "未修" => Self::NotTaken,
                "免修" => Self::Exempt,
                _ => Self::Other(trimmed.to_owned()),
            },
        }
    }
}

impl Serialize for CompletionStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CompletionStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from(value))
    }
}

/// 培养方案内或计划外的单门课程修读进度。
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PlanCourse {
    /// 课程数字 ID。
    pub course_id: i64,
    /// 课程代码（如 `"CS101"`）。
    pub course_code: String,
    /// 课程中文名称。
    pub course_name_zh: String,
    /// 课程英文名称。
    #[serde(default)]
    pub course_name_en: Option<String>,
    /// 课程学分。
    #[serde(default)]
    pub credits: Option<f64>,
    /// 课程类别（如必修课、限选课、核心通识课）。
    #[serde(default)]
    pub course_type: Option<NamedEntity>,
    /// 是否为方案内强制必修课程。
    pub is_compulsory: bool,
    /// 推荐修读学期序号列表（从 `1` 起升序排列，如 `[1, 2]` 表示第一、二学期）。
    #[serde(default)]
    pub suggested_terms: Vec<u8>,
    /// 当前修读完成状态。
    pub status: CompletionStatus,
    /// 取得的课程成绩文本（如 `"87"`、`"优"`；未修或未出分时为 `None`）。
    #[serde(default)]
    pub score: Option<String>,
    /// 取得的课程绩点（无绩点制或未出分时为 `None`）。
    #[serde(default)]
    pub grade_point: Option<f64>,
    /// 计划转换说明或备注（如 `"计划外课程转换（校_任选课）"`）。
    #[serde(default)]
    pub remark: Option<String>,
    /// 已生效的替代课程列表。
    #[serde(default)]
    pub substitute_courses: Vec<NamedEntity>,
}

/// 培养方案课程模块节点（支持多级子模块递归嵌套）。
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct PlanModule {
    /// 模块类别（如必修课、限选课、院任选课、校任选课）。
    pub kind: NamedEntity,
    /// 本模块要求修满的学分。
    pub required_credits: f64,
    /// 本模块要求修满的门数（无门数限制时为 `0`）。
    pub required_course_count: u32,
    /// 本模块要求达标的子模块数量（无子模块限制时为 `0`）。
    pub required_sub_module_count: u32,
    /// 已通过审核的学分。
    pub passed_credits: f64,
    /// 当前在修学分。
    pub taking_credits: f64,
    /// 免修学分。
    pub exempt_credits: f64,
    /// 本模块当前整体达标状态。
    pub status: CompletionStatus,
    /// 模块备注说明。
    #[serde(default)]
    pub remark: Option<String>,
    /// 下属子模块列表。
    #[serde(default)]
    pub children: Vec<Self>,
    /// 归属本模块的课程列表。
    #[serde(default)]
    pub courses: Vec<PlanCourse>,
}

/// 单个培养类别（如主修、辅修）的培养方案整体完成进度。
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProgramCompletion {
    /// 培养类别（如主修 `"01"`、辅修 `"02"`）。
    pub cultivate_type: NamedEntity,
    /// 方案毕业要求总学分。
    pub required_credits: f64,
    /// 方案要求达标的顶层模块数。
    pub required_module_count: u32,
    /// 顶层课程模块完成列表。
    #[serde(default)]
    pub modules: Vec<PlanModule>,
    /// 计划外课程及学分转换记录列表。
    #[serde(default)]
    pub out_of_plan_courses: Vec<PlanCourse>,
}

impl ProgramCompletion {
    /// 汇总顶层模块的已通过总学分。
    #[must_use]
    pub fn passed_credits(&self) -> f64 {
        self.modules
            .iter()
            .map(|module| module.passed_credits)
            .sum()
    }

    /// 汇总顶层模块的当前在修总学分。
    #[must_use]
    pub fn taking_credits(&self) -> f64 {
        self.modules
            .iter()
            .map(|module| module.taking_credits)
            .sum()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCourseRef {
    id: i64,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    code: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    name_zh: String,
    #[serde(default)]
    name_en: Option<String>,
    #[serde(default)]
    credits: Option<f64>,
    #[serde(default)]
    course_type: Option<NamedEntity>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPlanCourse {
    course: RawCourseRef,
    #[serde(default)]
    compulsory: bool,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    terms: Vec<String>,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    result_en: Option<String>,
    #[serde(default)]
    result_score: Option<String>,
    #[serde(default)]
    score: Option<Value>,
    #[serde(default)]
    gp: Option<f64>,
    #[serde(default)]
    remark: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    substitute_courses: Vec<NamedEntity>,
}

impl From<RawPlanCourse> for PlanCourse {
    fn from(raw: RawPlanCourse) -> Self {
        let status = resolve_status(raw.result_en, raw.result);
        let score =
            trim_non_empty(raw.result_score).or_else(|| raw.score.and_then(scalar_to_string));
        Self {
            course_id: raw.course.id,
            course_code: raw.course.code.trim().to_owned(),
            course_name_zh: raw.course.name_zh.trim().to_owned(),
            course_name_en: trim_non_empty(raw.course.name_en),
            credits: raw.course.credits.filter(|value| value.is_finite()),
            course_type: raw.course.course_type,
            is_compulsory: raw.compulsory,
            suggested_terms: parse_terms(&raw.terms),
            status,
            score,
            grade_point: raw.gp.filter(|value| value.is_finite()),
            remark: trim_non_empty(raw.remark),
            substitute_courses: raw.substitute_courses,
        }
    }
}

#[derive(Default, Deserialize)]
struct RawModuleRequireInfo {
    #[serde(default, rename = "requiredCredits")]
    credits: f64,
    #[serde(default, rename = "requiredCourseNum")]
    course_num: u32,
    #[serde(default, rename = "requiredSubModuleNum")]
    sub_module_num: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPlanModule {
    #[serde(rename = "type")]
    kind: NamedEntity,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    require_info: RawModuleRequireInfo,
    #[serde(default)]
    passed_credits: f64,
    #[serde(default)]
    taking: f64,
    #[serde(default)]
    skip: f64,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    result_en: Option<String>,
    #[serde(default)]
    remark: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    children: Vec<Self>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    plan_courses: Vec<RawPlanCourse>,
}

impl From<RawPlanModule> for PlanModule {
    fn from(raw: RawPlanModule) -> Self {
        Self {
            kind: raw.kind,
            required_credits: finite_or_zero(raw.require_info.credits),
            required_course_count: raw.require_info.course_num,
            required_sub_module_count: raw.require_info.sub_module_num,
            passed_credits: finite_or_zero(raw.passed_credits),
            taking_credits: finite_or_zero(raw.taking),
            exempt_credits: finite_or_zero(raw.skip),
            status: resolve_status(raw.result_en, raw.result),
            remark: trim_non_empty(raw.remark),
            children: raw.children.into_iter().map(Self::from).collect(),
            courses: raw.plan_courses.into_iter().map(PlanCourse::from).collect(),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTopRequireInfo {
    #[serde(default)]
    credits: f64,
    #[serde(default)]
    sub_module_num: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RawCompletionInfo {
    #[serde(default, deserialize_with = "deserialize_null_default")]
    require_info: RawTopRequireInfo,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    modules: Vec<RawPlanModule>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    outplan_courses: Vec<RawPlanCourse>,
}

pub(super) fn build_program_completion(
    cultivate_type: NamedEntity,
    raw: RawCompletionInfo,
) -> ProgramCompletion {
    ProgramCompletion {
        cultivate_type,
        required_credits: finite_or_zero(raw.require_info.credits),
        required_module_count: raw.require_info.sub_module_num,
        modules: raw.modules.into_iter().map(PlanModule::from).collect(),
        out_of_plan_courses: raw
            .outplan_courses
            .into_iter()
            .map(PlanCourse::from)
            .collect(),
    }
}

fn resolve_status(result_en: Option<String>, result_zh: Option<String>) -> CompletionStatus {
    trim_non_empty(result_en)
        .or_else(|| trim_non_empty(result_zh))
        .map_or(CompletionStatus::NotTaken, CompletionStatus::from)
}

fn parse_terms(raw_terms: &[String]) -> Vec<u8> {
    let mut terms = BTreeSet::new();
    for item in raw_terms {
        let trimmed = item.trim();
        let digits = trimmed
            .strip_prefix("TERM_")
            .or_else(|| trimmed.strip_prefix("term_"))
            .unwrap_or(trimmed);
        if let Ok(term) = digits.parse::<u8>()
            && term > 0
        {
            terms.insert(term);
        }
    }
    terms.into_iter().collect()
}

fn scalar_to_string(value: Value) -> Option<String> {
    match value {
        Value::String(text) => trim_non_empty(Some(text)),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn sample_raw_completion_info() -> RawCompletionInfo {
        serde_json::from_value(json!({
            "requireInfo": { "credits": 160.0, "subModuleNum": 2 },
            "modules": [
                {
                    "type": {"id": 6, "code": "04", "nameZh": "必修课", "nameEn": "Obligatory"},
                    "requireInfo": {"requiredSubModuleNum": 1, "requiredCredits": 120.0, "requiredCourseNum": 0},
                    "passedCredits": 80.0, "taking": 16.0, "skip": 0.0,
                    "result": "不通过", "resultEn": "failed",
                    "children": [{
                        "type": {"id": 61, "code": "0401", "nameZh": "学科基础", "nameEn": null},
                        "requireInfo": {"requiredSubModuleNum": 0, "requiredCredits": 40.0, "requiredCourseNum": 2},
                        "passedCredits": 40.0, "taking": 0.0, "skip": 0.0,
                        "result": "通过", "resultEn": "passed",
                        "children": [], "planCourses": []
                    }],
                    "planCourses": [
                        {
                            "compulsory": true,
                            "course": {
                                "id": 1001, "code": "MATH101", "nameZh": "微积分",
                                "nameEn": "Calculus", "credits": 4.0,
                                "courseType": {"id": 6, "code": "04", "nameZh": "必修课", "nameEn": "Obligatory"}
                            },
                            "terms": ["TERM_2", "TERM_1"],
                            "result": "通过", "resultEn": "passed",
                            "score": 88, "resultScore": "88", "gp": 3.7,
                            "substituteCourses": []
                        },
                        {
                            "compulsory": true,
                            "course": {"id": 1002, "code": "CS301", "nameZh": "编译原理", "credits": 3.0},
                            "terms": ["TERM_6"],
                            "result": "未修", "resultEn": "unrepaired",
                            "score": null, "resultScore": null, "gp": null,
                            "substituteCourses": []
                        }
                    ]
                },
                {
                    "type": {"id": 8, "code": "07", "nameZh": "校任选课", "nameEn": "Elective"},
                    "requireInfo": {"requiredSubModuleNum": 0, "requiredCredits": 40.0, "requiredCourseNum": 0},
                    "passedCredits": 40.0, "taking": 0.0, "skip": 2.0,
                    "result": "通过", "resultEn": "passed",
                    "children": [], "planCourses": []
                }
            ],
            "outplanCourses": [{
                "compulsory": false,
                "course": {"id": 2001, "code": "GE101", "nameZh": "艺术导论", "credits": 2.0},
                "terms": [], "result": "通过", "resultEn": "passed",
                "resultScore": "92", "score": 92, "gp": null,
                "remark": "计划外课程转换（校任选课）",
                "substituteCourses": []
            }]
        }))
        .unwrap()
    }

    #[test]
    fn program_completion_parses_module_tree_normalizes_terms_and_statuses_and_roundtrips() {
        let cultivate = NamedEntity {
            id: 1,
            code: "01".into(),
            name_zh: "主修".into(),
            name_en: Some("Major".into()),
        };
        let completion = build_program_completion(cultivate, sample_raw_completion_info());
        assert!((completion.required_credits - 160.0).abs() < f64::EPSILON);
        assert_eq!(completion.required_module_count, 2);
        assert!((completion.passed_credits() - 120.0).abs() < f64::EPSILON);
        assert!((completion.taking_credits() - 16.0).abs() < f64::EPSILON);

        let first_mod = &completion.modules[0];
        assert_eq!(first_mod.status, CompletionStatus::Failed);
        assert_eq!(first_mod.children.len(), 1);
        assert_eq!(first_mod.children[0].status, CompletionStatus::Passed);
        assert_eq!(first_mod.courses[0].suggested_terms, vec![1, 2]);
        assert_eq!(first_mod.courses[0].score.as_deref(), Some("88"));
        assert_eq!(first_mod.courses[1].status, CompletionStatus::NotTaken);

        assert_eq!(completion.out_of_plan_courses.len(), 1);
        assert_eq!(
            completion.out_of_plan_courses[0].remark.as_deref(),
            Some("计划外课程转换（校任选课）")
        );

        let encoded = serde_json::to_value(&completion).unwrap();
        let restored: ProgramCompletion = serde_json::from_value(encoded).unwrap();
        assert_eq!(restored, completion);
    }

    #[test]
    fn completion_status_maps_chinese_and_english_variants() {
        for (raw, expected) in [
            ("passed", CompletionStatus::Passed),
            ("通过", CompletionStatus::Passed),
            ("failed", CompletionStatus::Failed),
            ("不通过", CompletionStatus::Failed),
            ("taking", CompletionStatus::InProgress),
            ("在修", CompletionStatus::InProgress),
            ("unrepaired", CompletionStatus::NotTaken),
            ("未修", CompletionStatus::NotTaken),
            ("免修", CompletionStatus::Exempt),
            (
                "custom_state",
                CompletionStatus::Other("custom_state".into()),
            ),
        ] {
            assert_eq!(CompletionStatus::from(raw.to_owned()), expected);
        }

        // 上游或规范 JSON 出现字段缺失/类型错误时，应保留具体的字段级错误而非 untagged 泛化报错。
        let broken_upstream = json!({
            "modules": [{
                "type": {"id": 6, "code": "04", "nameZh": "必修课"},
                "children": [{
                    "planCourses": []
                }]
            }]
        });
        let err = serde_json::from_value::<RawCompletionInfo>(broken_upstream)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("missing field `type`"), "{err}");

        let broken_canonical = json!({"courseCode": "CS101"});
        let err = serde_json::from_value::<PlanCourse>(broken_canonical)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("missing field `courseId`"), "{err}");
    }
}
