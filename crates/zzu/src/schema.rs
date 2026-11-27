use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};

macro_rules! formatted_string_schema {
    ($ty:ident, $id:literal, $format:literal) => {
        pub(crate) struct $ty;

        impl JsonSchema for $ty {
            fn inline_schema() -> bool {
                true
            }

            fn schema_name() -> Cow<'static, str> {
                stringify!($ty).into()
            }

            fn schema_id() -> Cow<'static, str> {
                $id.into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "format": $format,
                })
            }
        }
    };
}

#[cfg(any(feature = "eas", feature = "ecard"))]
formatted_string_schema!(Date, "jiff::civil::Date", "date");
#[cfg(feature = "eas")]
formatted_string_schema!(Time, "jiff::civil::Time", "partial-time");
formatted_string_schema!(ZonedDateTime, "jiff::ZonedDateTime", "zoned-date-time");
