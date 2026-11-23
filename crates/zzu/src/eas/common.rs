use jiff::{
    Zoned,
    civil::{Date, Time},
};
use serde::{Deserialize, Deserializer, Serialize};
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

/// 将上游 `HHMM`（数字或字符串）或已编码的 RFC 9557 时间字符串解析为 `Asia/Shanghai` 时区的 [`Zoned`]。
pub(super) fn parse_time(date: Date, value: &Value) -> Result<Zoned> {
    let text = match value {
        Value::String(text) => text.trim().to_owned(),
        Value::Number(number) => number.to_string(),
        _ => {
            return Err(Error::decode_message(
                "时间必须为 HHMM 或带时区时间字符串".into(),
            ));
        }
    };
    if let Ok(datetime) = text.parse::<Zoned>() {
        return Ok(datetime);
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
    let time = Time::new(hour, minute, 0, 0).map_err(Error::decode)?;
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
