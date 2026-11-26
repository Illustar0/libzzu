use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, de};
use serde_json::Value;

fn parse_decimal_value<E: de::Error>(value: Value) -> Result<Decimal, E> {
    let text = match value {
        Value::String(text) => text,
        Value::Number(number) => number.to_string(),
        _ => return Err(E::custom("金额必须为十进制数字或字符串")),
    };
    let text = text.trim();
    match text.split_once(['e', 'E']) {
        Some((base, _)) => {
            Decimal::from_str_exact(base).and_then(|_| Decimal::from_scientific(text))
        }
        None => Decimal::from_str_exact(text),
    }
    .map_err(E::custom)
}

pub(crate) fn deserialize_decimal<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Decimal, D::Error> {
    parse_decimal_value(Value::deserialize(deserializer)?)
}

pub(crate) fn deserialize_optional_decimal<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Decimal>, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::Null => Ok(None),
        value => parse_decimal_value(value).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_amounts_preserve_decimal_precision_and_scale() {
        assert_eq!(deserialize_optional_decimal(&Value::Null).unwrap(), None);
        assert_eq!(
            deserialize_optional_decimal(&Value::String("0.57".into()))
                .unwrap()
                .unwrap()
                .to_string(),
            "0.57"
        );
        assert!(deserialize_optional_decimal(&Value::String(String::new())).is_err());

        for (json, expected) in [
            ("90071992547409.93", "90071992547409.93"),
            ("\"90071992547409.93\"", "90071992547409.93"),
            ("9.007199254740993e13", "90071992547409.93"),
            ("\"12.3400\"", "12.3400"),
            ("-0.01", "-0.01"),
            ("0.001", "0.001"),
            ("1e-28", "0.0000000000000000000000000001"),
            (
                "79228162514264337593543950335",
                "79228162514264337593543950335",
            ),
        ] {
            let value: Value = serde_json::from_str(json).unwrap();
            let amount = deserialize_decimal(&value).unwrap();
            assert_eq!(amount.to_string(), expected, "{json}");
            assert_eq!(
                serde_json::to_string(&amount).unwrap(),
                format!("\"{expected}\"")
            );
        }
    }

    #[test]
    fn json_amounts_reject_rounding_overflow_and_invalid_types() {
        for json in [
            "true",
            "null",
            "{}",
            "[]",
            "\"\"",
            "\"NaN\"",
            "\"Infinity\"",
            "1e-29",
            "1.00000000000000000000000000001",
            "1.00000000000000000000000000001e0",
            "79228162514264337593543950336",
            "-79228162514264337593543950336",
            "1e1000000000",
        ] {
            let value: Value = serde_json::from_str(json).unwrap();
            assert!(deserialize_decimal(&value).is_err(), "{json}");
        }
    }
}
