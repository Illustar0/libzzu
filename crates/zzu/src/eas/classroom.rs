use std::collections::HashSet;
use std::fmt;

use jiff::{Zoned, civil::Date};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::common::{CourseUnit, deserialize_null_default, normalize_course_units, parse_time};
use crate::{Error, Result};

/// 教室占用分页查询的默认每页条数。
///
/// 教室占用 API 存在严重性能问题，服务端响应耗时会随 `page_size` 增大而非线性增大
/// （`100` 条需约 1 秒以上，`1000` 条需约 25 秒以上）；一般而言, `44` 左右是
/// 单条均摊耗时最低的甜点值。若不了解服务端行为，建议保持默认值。
pub const DEFAULT_CLASSROOM_PAGE_SIZE: u32 = 44;

/// 并发拉取全部教室占用分页时的默认最大并发请求数。
pub const DEFAULT_CLASSROOM_CONCURRENCY: usize = 8;

/// 校区元数据。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Campus {
    /// 校区数字 ID。
    pub id: i64,
    /// 校区编码。
    pub code: String,
    /// 校区中文名称。
    pub name_zh: String,
    /// 校区英文名称。
    pub name_en: Option<String>,
    /// 备注信息。
    pub remark: Option<String>,
    /// 是否启用。
    pub enabled: bool,
}

/// 教学楼元数据。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Building {
    /// 教学楼数字 ID。
    pub id: i64,
    /// 教学楼编码。
    pub code: String,
    /// 教学楼中文名称。
    pub name_zh: String,
    /// 教学楼英文名称。
    pub name_en: Option<String>,
    /// 所属校区 ID（映射自上游 `campusAssoc`）。
    ///
    /// 注意：上游数据中可能存在指向未出现在校区列表中的默认校区 ID（如 `1`）的记录。
    #[serde(alias = "campusAssoc")]
    pub campus_id: i64,
    /// 备注信息。
    pub remark: Option<String>,
    /// 是否启用。
    pub enabled: bool,
}

/// 教室类型元数据（如普通教室、多媒体教室、智慧教室）。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomType {
    /// 教室类型数字 ID。
    pub id: i64,
    /// 教室类型编码。
    pub code: String,
    /// 教室类型中文名称。
    pub name_zh: String,
    /// 教室类型英文名称。
    pub name_en: Option<String>,
    /// 是否启用。
    pub enabled: bool,
}

/// 教室占用查询所需的筛选目录与标准节次作息快照。
///
/// 各列表保持扁平结构，既可用于构建前端筛选项与节次网格，也便于通过
/// [`ClassroomOccupancy::building_id`] 反查教学楼与所属校区信息。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", try_from = "RawClassroomCatalog")]
pub struct ClassroomCatalog {
    /// 全部校区列表。
    pub campuses: Vec<Campus>,
    /// 全部教学楼列表。
    pub buildings: Vec<Building>,
    /// 全部教室类型列表。
    pub room_types: Vec<RoomType>,
    /// 可选楼层列表。
    pub floors: Vec<i32>,
    /// 空教室筛选与节次概览使用的标准作息列表（按节次序号升序排列）。
    pub units: Vec<CourseUnit>,
}

impl ClassroomCatalog {
    pub(super) fn new(
        campuses: Vec<Campus>,
        buildings: Vec<Building>,
        room_types: Vec<RoomType>,
        floors: Vec<i32>,
        units: Vec<CourseUnit>,
    ) -> Result<Self> {
        Ok(Self {
            campuses,
            buildings,
            room_types,
            floors,
            units: normalize_course_units(units)?,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawClassroomCatalog {
    campuses: Vec<Campus>,
    buildings: Vec<Building>,
    room_types: Vec<RoomType>,
    floors: Vec<i32>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    units: Vec<CourseUnit>,
}

impl TryFrom<RawClassroomCatalog> for ClassroomCatalog {
    type Error = Error;

    fn try_from(raw: RawClassroomCatalog) -> Result<Self> {
        ClassroomCatalog::new(
            raw.campuses,
            raw.buildings,
            raw.room_types,
            raw.floors,
            raw.units,
        )
    }
}

/// 教室占用查询条件。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassroomQuery {
    /// 查询公历日期（必填）。
    pub date: Date,
    /// 校区 ID（空表示不限校区）。
    #[serde(default)]
    pub campus_id: Option<i64>,
    /// 教学楼 ID 列表（空表示不限教学楼）。
    #[serde(default)]
    pub building_ids: Vec<i64>,
    /// 教室类型 ID 列表（空表示不限类型）。
    #[serde(default)]
    pub room_type_ids: Vec<i64>,
    /// 楼层列表（空表示不限楼层）。
    #[serde(default)]
    pub floors: Vec<i32>,
    /// 最小座位数（空表示不限下限）。
    #[serde(default)]
    pub min_seats: Option<u32>,
    /// 最大座位数（空表示不限上限，须不小于 `min_seats`）。
    #[serde(default)]
    pub max_seats: Option<u32>,
}

impl ClassroomQuery {
    /// 创建仅指定查询日期的默认查询条件（其余空间筛选均为空）。
    #[must_use]
    pub fn new(date: Date) -> Self {
        Self {
            date,
            campus_id: None,
            building_ids: Vec::new(),
            room_type_ids: Vec::new(),
            floors: Vec::new(),
            min_seats: None,
            max_seats: None,
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if let (Some(min), Some(max)) = (self.min_seats, self.max_seats)
            && min > max
        {
            return Err(Error::invalid_input("最小座位数不能大于最大座位数".into()));
        }
        Ok(())
    }
}

/// 全量并发拉取教室占用时的分片与并发配置。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassroomFetchOptions {
    /// 单次分页请求条数（默认 [`DEFAULT_CLASSROOM_PAGE_SIZE`]，即 `44`）。
    ///
    /// 上游 API 存在严重的性能问题，响应耗时随 `page_size` 增大而非线性增大
    /// （`100` 约 1 秒以上，`1000` 约 25 秒以上），`44` 左右通常是单条均摊耗时最低的甜点值；
    /// 若不了解后端特性，建议保持默认。在高占用率校区可酌情调小以降低单页尾延迟。
    pub page_size: u32,
    /// 拉取剩余分页时的最大并发请求数（默认 [`DEFAULT_CLASSROOM_CONCURRENCY`]，即 `8`）。
    pub concurrency: usize,
}

impl Default for ClassroomFetchOptions {
    fn default() -> Self {
        Self {
            page_size: DEFAULT_CLASSROOM_PAGE_SIZE,
            concurrency: DEFAULT_CLASSROOM_CONCURRENCY,
        }
    }
}

impl ClassroomFetchOptions {
    pub(crate) fn validate(self) -> Result<()> {
        validate_page_size(self.page_size)?;
        if self.concurrency == 0 {
            return Err(Error::invalid_input("并发请求数必须大于 0".into()));
        }
        Ok(())
    }
}

pub(crate) fn validate_page(page: u32) -> Result<()> {
    if page == 0 {
        return Err(Error::invalid_input("页码从 1 开始".into()));
    }
    Ok(())
}

pub(crate) fn validate_page_size(page_size: u32) -> Result<()> {
    if page_size == 0 {
        return Err(Error::invalid_input("每页条数必须大于 0".into()));
    }
    Ok(())
}

/// 教室占用活动类型。
///
/// 序列化为统一的 `snake_case` 字符串，同时兼容反序列化教务上游的原始 `PascalCase` 标识；
/// 遇到未来新增的未知活动类型时通过 [`Self::Other`] 保留原始文本而不报错。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoomActivityType {
    /// 常规课程排课（上游 `"Lesson"`，序列化为 `"lesson"`）。
    Lesson,
    /// 实验项目排课（上游 `"ExpProjectScheduleGroup"`，序列化为 `"experiment"`）。
    Experiment,
    /// 教室借用（上游 `"RoomBorrow"`，序列化为 `"room_borrow"`）。
    RoomBorrow,
    /// 考试安排（上游 `"Exam"`，序列化为 `"exam"`）。
    Exam,
    /// 调课占用（上游 `"ScheduleChange"`，序列化为 `"schedule_change"`）。
    ScheduleChange,
    /// 上游返回的其他未知活动类型，原样保留字符串。
    Other(String),
}

impl RoomActivityType {
    /// 返回序列化使用的规范化字符串标识。
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Lesson => "lesson",
            Self::Experiment => "experiment",
            Self::RoomBorrow => "room_borrow",
            Self::Exam => "exam",
            Self::ScheduleChange => "schedule_change",
            Self::Other(other) => other.as_str(),
        }
    }
}

impl fmt::Display for RoomActivityType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<String> for RoomActivityType {
    fn from(value: String) -> Self {
        match value.as_str() {
            "Lesson" | "lesson" => Self::Lesson,
            "ExpProjectScheduleGroup" | "experiment" => Self::Experiment,
            "RoomBorrow" | "room_borrow" => Self::RoomBorrow,
            "Exam" | "exam" => Self::Exam,
            "ScheduleChange" | "schedule_change" => Self::ScheduleChange,
            _ => Self::Other(value),
        }
    }
}

impl Serialize for RoomActivityType {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RoomActivityType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from(value))
    }
}

/// 教室在指定日期内的单条占用记录。
///
/// 上游接口当前不回填节次字段（`unitStart` / `unitEnd` 恒为 `null`），
/// 因此占用范围以 `Asia/Shanghai` 时区的时间区间 `[start_time, end_time)` 表示。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", try_from = "RawRoomOccupation")]
pub struct RoomOccupation {
    /// 关联活动 ID（实验项目等场景可能为 `None`）。
    pub activity_id: Option<i64>,
    /// 占用活动类型。
    pub activity_type: RoomActivityType,
    /// 占用活动中文名称或说明。
    pub activity_name: String,
    /// 占用活动英文名称或说明。
    pub activity_name_en: Option<String>,
    /// 占用公历日期。
    pub date: Date,
    /// 占用开始时间（`Asia/Shanghai` 时区）。
    pub start_time: Zoned,
    /// 占用结束时间（`Asia/Shanghai` 时区，严格晚于 `start_time`）。
    pub end_time: Zoned,
    /// 授课教师或借用负责人中文姓名（未提供时为空字符串）。
    pub teacher_name: String,
    /// 授课教师或借用负责人英文姓名。
    pub teacher_name_en: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRoomOccupation {
    #[serde(default)]
    activity_id: Option<i64>,
    activity_type: RoomActivityType,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    activity_name: String,
    #[serde(default)]
    activity_name_en: Option<String>,
    date: Date,
    start_time: Value,
    end_time: Value,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    teacher_name: String,
    #[serde(default)]
    teacher_name_en: Option<String>,
}

impl TryFrom<RawRoomOccupation> for RoomOccupation {
    type Error = Error;

    fn try_from(raw: RawRoomOccupation) -> Result<Self> {
        let start_time = parse_time(raw.date, &raw.start_time)?;
        let end_time = parse_time(raw.date, &raw.end_time)?;
        if start_time.timestamp() >= end_time.timestamp() {
            return Err(Error::decode_message(
                "教室占用结束时间必须晚于开始时间".into(),
            ));
        }
        Ok(Self {
            activity_id: raw.activity_id,
            activity_type: raw.activity_type,
            activity_name: raw.activity_name,
            activity_name_en: raw.activity_name_en,
            date: raw.date,
            start_time,
            end_time,
            teacher_name: raw.teacher_name,
            teacher_name_en: raw.teacher_name_en,
        })
    }
}

/// 教室基础信息及其在查询当日的占用列表。
///
/// 上游接口在返回教室对象时未回填 `floor`、`seatsForLesson`、`enabled`、`virtual`、`experiment`
/// （始终返回零值 `0` / `false`），因此不在领域模型中暴露这些无效字段。
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassroomOccupancy {
    /// 教室数字 ID。
    pub id: i64,
    /// 教室编码。
    pub code: String,
    /// 教室中文名称。
    pub name_zh: String,
    /// 教室英文名称。
    pub name_en: Option<String>,
    /// 所属教学楼 ID（可通过 [`ClassroomCatalog::buildings`] 查找教学楼名称）。
    pub building_id: i64,
    /// 所属校区中文名称。
    pub campus_name_zh: String,
    /// 查询当日的占用记录列表（无占用时为空列表）。
    #[serde(
        alias = "roomOccupationInfoVms",
        default,
        deserialize_with = "deserialize_null_default"
    )]
    pub occupations: Vec<RoomOccupation>,
}

impl ClassroomOccupancy {
    /// 判断该教室在半开时间区间 `[start, end)` 内是否完全空闲。
    ///
    /// 相邻边界（例如前一条占用的 `end_time` 恰好等于 `start`）不视为冲突。
    ///
    /// # Errors
    ///
    /// `start` 不早于 `end` 时返回 [`Error::InvalidInput`]。
    pub fn is_free_between(&self, start: &Zoned, end: &Zoned) -> Result<bool> {
        Ok(self.occupations_between(start, end)?.is_empty())
    }

    /// 返回该教室在半开时间区间 `[start, end)` 内存在重叠的全部占用记录。
    ///
    /// # Errors
    ///
    /// `start` 不早于 `end` 时返回 [`Error::InvalidInput`]。
    pub fn occupations_between(&self, start: &Zoned, end: &Zoned) -> Result<Vec<&RoomOccupation>> {
        let start = start.timestamp();
        let end = end.timestamp();
        if start >= end {
            return Err(Error::invalid_input("查询结束时间必须晚于开始时间".into()));
        }
        Ok(self
            .occupations
            .iter()
            .filter(|occupation| {
                occupation.start_time.timestamp() < end && occupation.end_time.timestamp() > start
            })
            .collect())
    }
}

/// 教室占用单页查询结果。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassroomPage {
    /// 当前页返回的教室占用列表。
    pub items: Vec<ClassroomOccupancy>,
    /// 当前页码（从 1 开始）。
    pub page: u32,
    /// 本次查询使用的每页条数。
    pub page_size: u32,
    /// 符合筛选条件的教室总条数。
    pub total_items: u32,
    /// 总页数。
    pub total_pages: u32,
}

impl ClassroomPage {
    /// 判断当前页之后是否还有更多分页。
    #[must_use]
    pub fn has_next(&self) -> bool {
        self.page < self.total_pages
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RawClassroomPage {
    #[serde(default, deserialize_with = "deserialize_null_default")]
    data: Vec<ClassroomOccupancy>,
    #[serde(rename = "_page_")]
    page: RawPageInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPageInfo {
    current_page: u32,
    rows_per_page: u32,
    total_rows: u32,
    total_pages: u32,
}

impl From<RawClassroomPage> for ClassroomPage {
    fn from(raw: RawClassroomPage) -> Self {
        Self {
            items: raw.data,
            page: raw.page.current_page,
            page_size: raw.page.rows_per_page,
            total_items: raw.page.total_rows,
            total_pages: raw.page.total_pages,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RoomsRequest<'a> {
    current_page: u32,
    page_size: u32,
    campus_assoc: Option<i64>,
    building_ids: &'a [i64],
    room_type_ids: &'a [i64],
    floors: &'a [i32],
    min_seat: Option<String>,
    max_seat: Option<String>,
    date: Date,
}

impl<'a> RoomsRequest<'a> {
    pub(super) fn new(query: &'a ClassroomQuery, page: u32, page_size: u32) -> Self {
        Self {
            current_page: page,
            page_size,
            campus_assoc: query.campus_id,
            building_ids: &query.building_ids,
            room_type_ids: &query.room_type_ids,
            floors: &query.floors,
            min_seat: query.min_seats.map(|seats| seats.to_string()),
            max_seat: query.max_seats.map(|seats| seats.to_string()),
            date: query.date,
        }
    }
}

/// 按页序拼接分页结果并按教室 ID 去重保序。
pub(super) fn merge_pages(
    first: ClassroomPage,
    mut rest: Vec<(u32, ClassroomPage)>,
) -> Vec<ClassroomOccupancy> {
    rest.sort_unstable_by_key(|(page_index, _)| *page_index);
    let mut merged = Vec::with_capacity(usize::try_from(first.total_items).unwrap_or(0));
    let mut seen = HashSet::with_capacity(merged.capacity());
    let pages = std::iter::once(first).chain(rest.into_iter().map(|(_, page)| page));
    for page in pages {
        for room in page.items {
            if seen.insert(room.id) {
                merged.push(room);
            }
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn zoned(text: &str) -> Zoned {
        text.parse().unwrap()
    }

    #[test]
    fn building_aliases_campus_assoc_and_roundtrips_camel_case() {
        let raw = json!({
            "id": 5,
            "code": "北1",
            "nameZh": "北1",
            "nameEn": null,
            "campusAssoc": 2,
            "remark": null,
            "enabled": true,
            "transient": false
        });
        let building: Building = serde_json::from_value(raw).unwrap();
        assert_eq!(building.campus_id, 2);

        let serialized = serde_json::to_value(&building).unwrap();
        assert_eq!(serialized["campusId"], 2);
        assert!(serialized.get("campusAssoc").is_none());
        assert!(serialized.get("transient").is_none());

        let restored: Building = serde_json::from_value(serialized).unwrap();
        assert_eq!(restored, building);
    }

    #[test]
    fn classroom_catalog_sorts_units_and_roundtrips_json() {
        let raw = json!({
            "campuses": [{
                "id": 2, "code": "01", "nameZh": "主校区", "nameEn": null,
                "remark": null, "enabled": true
            }],
            "buildings": [{
                "id": 5, "code": "北1", "nameZh": "北1", "nameEn": null,
                "campusAssoc": 2, "remark": null, "enabled": true
            }],
            "roomTypes": [{
                "id": 2, "code": "02", "nameZh": "多媒体教室", "nameEn": null,
                "enabled": true
            }],
            "floors": [1, 2, 3],
            "units": [
                {
                    "indexNo": 10, "nameZh": "10", "nameEn": "10",
                    "dayPart": "EVENING", "segmentIndex": 5,
                    "startTime": 1955, "endTime": 2040
                },
                {
                    "indexNo": 1, "nameZh": "1", "nameEn": "1",
                    "dayPart": "MORNING", "segmentIndex": 1,
                    "startTime": 800, "endTime": 845
                }
            ]
        });
        let catalog: ClassroomCatalog = serde_json::from_value(raw).unwrap();
        let indices: Vec<u8> = catalog.units.iter().map(|unit| unit.index).collect();
        assert_eq!(indices, [1, 10]);
        assert_eq!(catalog.units[1].end_time.to_string(), "20:40:00");

        let serialized = serde_json::to_value(&catalog).unwrap();
        let restored: ClassroomCatalog = serde_json::from_value(serialized).unwrap();
        assert_eq!(restored, catalog);
    }

    #[test]
    fn classroom_occupancy_normalizes_null_occupations_and_ignores_unmodeled_fields() {
        let raw = json!({
            "id": 1999,
            "nameZh": "建材实验室",
            "nameEn": null,
            "code": "011401",
            "buildingId": 368,
            "campusNameZh": "主校区",
            "floor": 0,
            "virtual": false,
            "seatsForLesson": 0,
            "enabled": false,
            "experiment": false,
            "roomOccupationInfoVms": null
        });
        let room: ClassroomOccupancy = serde_json::from_value(raw).unwrap();
        assert_eq!(room.occupations, []);

        let serialized = serde_json::to_value(&room).unwrap();
        assert!(serialized.get("seatsForLesson").is_none());
        assert!(serialized.get("roomOccupationInfoVms").is_none());

        let restored: ClassroomOccupancy = serde_json::from_value(serialized).unwrap();
        assert_eq!(restored, room);
    }

    fn sample_room_with_occupations() -> ClassroomOccupancy {
        let raw = json!({
            "id": 581,
            "nameZh": "东区1_202",
            "nameEn": null,
            "code": "1848372744471842816",
            "buildingId": 1,
            "campusNameZh": "东校区",
            "roomOccupationInfoVms": [
                {
                    "id": 581,
                    "activityId": null,
                    "date": "2026-11-23",
                    "startTime": 800,
                    "endTime": 940,
                    "activityType": "ExpProjectScheduleGroup",
                    "activityName": "实验项目：妇产科学",
                    "activityNameEn": "experiment",
                    "teacherName": "苏玥辉",
                    "teacherNameEn": ""
                },
                {
                    "id": 581,
                    "activityId": 468_530,
                    "date": "2026-11-23",
                    "startTime": 1010,
                    "endTime": 1150,
                    "activityType": "Lesson",
                    "activityName": "课程：神经病学",
                    "activityNameEn": null,
                    "teacherName": "苗旺",
                    "teacherNameEn": "null"
                },
                {
                    "id": 581,
                    "activityId": 4172,
                    "date": "2026-11-23",
                    "startTime": 1400,
                    "endTime": 1445,
                    "activityType": "RoomBorrow",
                    "activityName": "活动名称：研究生专业课",
                    "teacherName": "马欢欢"
                },
                {
                    "id": 581,
                    "activityId": 999,
                    "date": "2026-11-23",
                    "startTime": 1900,
                    "endTime": 2040,
                    "activityType": "Exam",
                    "activityName": "期末考试",
                    "teacherName": ""
                },
                {
                    "id": 581,
                    "activityId": 1000,
                    "date": "2026-11-23",
                    "startTime": 1600,
                    "endTime": 1740,
                    "activityType": "ScheduleChange",
                    "activityName": "调课占用",
                    "teacherName": "李四"
                },
                {
                    "id": 581,
                    "activityId": 1001,
                    "date": "2026-11-23",
                    "startTime": 2100,
                    "endTime": 2130,
                    "activityType": "CustomActivity",
                    "activityName": "其他活动",
                    "teacherName": ""
                }
            ]
        });
        serde_json::from_value(raw).unwrap()
    }

    #[test]
    fn room_occupation_parses_types_times_and_roundtrips_json() {
        let room = sample_room_with_occupations();
        assert_eq!(room.occupations.len(), 6);
        assert_eq!(
            room.occupations[0].activity_type,
            RoomActivityType::Experiment
        );
        assert_eq!(room.occupations[1].activity_type, RoomActivityType::Lesson);
        assert_eq!(
            room.occupations[2].activity_type,
            RoomActivityType::RoomBorrow
        );
        assert_eq!(room.occupations[3].activity_type, RoomActivityType::Exam);
        assert_eq!(
            room.occupations[4].activity_type,
            RoomActivityType::ScheduleChange
        );
        assert_eq!(
            room.occupations[5].activity_type,
            RoomActivityType::Other("CustomActivity".into())
        );
        assert_eq!(
            room.occupations[0].start_time.to_string(),
            "2026-11-23T08:00:00+08:00[Asia/Shanghai]"
        );

        let json = serde_json::to_value(&room).unwrap();
        assert_eq!(json["occupations"][0]["activityType"], "experiment");
        assert_eq!(json["occupations"][1]["activityType"], "lesson");
        assert_eq!(json["occupations"][2]["activityType"], "room_borrow");
        assert_eq!(json["occupations"][3]["activityType"], "exam");
        assert_eq!(json["occupations"][4]["activityType"], "schedule_change");
        assert_eq!(json["occupations"][5]["activityType"], "CustomActivity");
        let restored: ClassroomOccupancy = serde_json::from_value(json).unwrap();
        assert_eq!(restored, room);
    }

    #[test]
    fn room_occupation_evaluates_free_intervals_and_conflicts() {
        let room = sample_room_with_occupations();
        // 相邻边界不冲突：09:40-10:10 课间完全空闲。
        assert!(
            room.is_free_between(
                &zoned("2026-11-23T09:40:00+08:00[Asia/Shanghai]"),
                &zoned("2026-11-23T10:10:00+08:00[Asia/Shanghai]"),
            )
            .unwrap()
        );
        // 跨越部分区间应命中冲突。
        let conflicts = room
            .occupations_between(
                &zoned("2026-11-23T09:00:00+08:00[Asia/Shanghai]"),
                &zoned("2026-11-23T10:30:00+08:00[Asia/Shanghai]"),
            )
            .unwrap();
        assert_eq!(conflicts.len(), 2);
        // 结束时间不晚于开始时间应报 InvalidInput。
        assert!(
            room.is_free_between(
                &zoned("2026-11-23T10:00:00+08:00[Asia/Shanghai]"),
                &zoned("2026-11-23T10:00:00+08:00[Asia/Shanghai]"),
            )
            .unwrap_err()
            .is_invalid_input()
        );
    }

    #[test]
    fn room_occupation_rejects_inverted_time_range() {
        let invalid = json!({
            "date": "2026-11-23",
            "startTime": 1150,
            "endTime": 1010,
            "activityType": "Lesson",
            "activityName": "测试",
            "teacherName": "教师"
        });
        assert!(serde_json::from_value::<RoomOccupation>(invalid).is_err());
    }

    #[test]
    fn query_and_fetch_options_validate_bounds() {
        let date: Date = "2026-11-23".parse().unwrap();
        let mut query = ClassroomQuery::new(date);
        query.min_seats = Some(80);
        query.max_seats = Some(60);
        assert!(query.validate().unwrap_err().is_invalid_input());

        query.max_seats = Some(80);
        assert!(query.validate().is_ok());

        assert!(validate_page(0).unwrap_err().is_invalid_input());
        assert!(validate_page_size(0).unwrap_err().is_invalid_input());
        assert!(
            ClassroomFetchOptions {
                page_size: 0,
                concurrency: 4,
            }
            .validate()
            .unwrap_err()
            .is_invalid_input()
        );
        assert!(
            ClassroomFetchOptions {
                page_size: 44,
                concurrency: 0,
            }
            .validate()
            .unwrap_err()
            .is_invalid_input()
        );
    }

    #[test]
    fn rooms_request_serializes_filters_in_upstream_wire_format() {
        let query = ClassroomQuery {
            date: "2026-11-23".parse().unwrap(),
            campus_id: Some(2),
            building_ids: vec![5, 6],
            room_type_ids: vec![2],
            floors: vec![1, 2],
            min_seats: Some(38),
            max_seats: Some(74),
        };
        let payload = serde_json::to_value(RoomsRequest::new(&query, 2, 44)).unwrap();
        assert_eq!(
            payload,
            json!({
                "currentPage": 2,
                "pageSize": 44,
                "campusAssoc": 2,
                "buildingIds": [5, 6],
                "roomTypeIds": [2],
                "floors": [1, 2],
                "minSeat": "38",
                "maxSeat": "74",
                "date": "2026-11-23"
            })
        );
    }

    #[test]
    fn raw_classroom_page_maps_page_info_and_normalizes_null_data() {
        let raw: RawClassroomPage = serde_json::from_value(json!({
            "data": null,
            "_page_": {
                "currentPage": 3,
                "rowsInPage": 0,
                "rowsPerPage": 44,
                "totalRows": 100,
                "totalPages": 3
            }
        }))
        .unwrap();
        let page = ClassroomPage::from(raw);
        assert_eq!(page.items, []);
        assert_eq!(page.page, 3);
        assert_eq!(page.page_size, 44);
        assert_eq!(page.total_items, 100);
        assert_eq!(page.total_pages, 3);
    }

    fn room(id: i64, name_zh: &str) -> ClassroomOccupancy {
        ClassroomOccupancy {
            id,
            code: id.to_string(),
            name_zh: name_zh.to_owned(),
            name_en: None,
            building_id: 5,
            campus_name_zh: "主校区".to_owned(),
            occupations: Vec::new(),
        }
    }

    fn page(index: u32, items: Vec<ClassroomOccupancy>) -> ClassroomPage {
        ClassroomPage {
            items,
            page: index,
            page_size: 2,
            total_items: 5,
            total_pages: 3,
        }
    }

    #[test]
    fn merge_pages_sorts_out_of_order_pages_before_deduplicating() {
        let first = page(1, vec![room(1, "R1"), room(2, "R2")]);
        let rest = vec![
            (3, page(3, vec![room(4, "R4-dup"), room(5, "R5")])),
            (2, page(2, vec![room(3, "R3"), room(4, "R4")])),
        ];
        let merged = merge_pages(first, rest);
        let names: Vec<&str> = merged.iter().map(|room| room.name_zh.as_str()).collect();
        assert_eq!(names, ["R1", "R2", "R3", "R4", "R5"]);
    }
}
