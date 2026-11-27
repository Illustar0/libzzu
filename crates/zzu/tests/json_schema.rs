//! JSON Schema 生成契约集成测试。

#![cfg(feature = "schemars")]

use std::collections::BTreeSet;

use schemars::{JsonSchema, schema_for};
use serde_json::Value;
#[cfg(feature = "ecard")]
use serde_json::json;

fn schema_value<T: JsonSchema>() -> Value {
    let schema = schema_for!(T);
    serde_json::to_value(&schema).expect("schema serializes to JSON")
}

fn property_keys(schema: &Value) -> BTreeSet<String> {
    schema["properties"]
        .as_object()
        .expect("object schema has properties")
        .keys()
        .cloned()
        .collect()
}

fn required_keys(schema: &Value) -> BTreeSet<String> {
    schema["required"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item.as_str().expect("required key is string").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn core_error_and_request_context_schemas_match_wire_layout() {
    let ctx_schema = schema_value::<zzu::RequestContext>();
    assert_eq!(
        property_keys(&ctx_schema),
        BTreeSet::from([
            "method".into(),
            "url".into(),
            "statusCode".into(),
            "errorCode".into(),
            "responseBody".into(),
        ])
    );
    assert_eq!(
        required_keys(&ctx_schema),
        BTreeSet::from(["method".into(), "url".into()])
    );

    let err_schema = schema_value::<zzu::Error>();
    assert_eq!(err_schema["title"], "Error");
    let variants = err_schema["oneOf"]
        .as_array()
        .expect("internally tagged Error schema uses oneOf");
    assert_eq!(variants.len(), 8);
    for variant in variants {
        assert_eq!(
            property_keys(variant),
            BTreeSet::from([
                "code".into(),
                "kind".into(),
                "message".into(),
                "context".into(),
            ])
        );
        assert_eq!(
            required_keys(variant),
            BTreeSet::from(["code".into(), "kind".into(), "message".into()])
        );
    }

    let serialize_err_schema = serde_json::to_value(
        schemars::generate::SchemaSettings::default()
            .for_serialize()
            .into_generator()
            .root_schema_for::<zzu::Error>(),
    )
    .unwrap();
    for variant in serialize_err_schema["oneOf"].as_array().unwrap() {
        assert_eq!(
            required_keys(variant),
            BTreeSet::from([
                "code".into(),
                "kind".into(),
                "message".into(),
                "context".into(),
            ])
        );
    }

    let sample_err = zzu::TrustRoots::from_pem_bundle(b"").err().unwrap();
    let encoded = serde_json::to_value(&sample_err).unwrap();
    assert_eq!(
        encoded
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "code".into(),
            "kind".into(),
            "message".into(),
            "context".into(),
        ])
    );
}

#[cfg(feature = "auth")]
#[test]
fn auth_models_generate_valid_schemas_with_base64_portrait_and_zoned_times() {
    let _ = schema_for!(zzu::auth::Service);
    let _ = schema_for!(zzu::auth::UndergradEasService);
    let _ = schema_for!(zzu::auth::Tokens);
    let _ = schema_for!(zzu::auth::MfaStatus);
    let _ = schema_for!(zzu::auth::SmsChallenge);
    let _ = schema_for!(zzu::auth::PersonalInfo);

    let portrait_schema = schema_value::<zzu::auth::Portrait>();
    assert_eq!(
        property_keys(&portrait_schema),
        BTreeSet::from(["mediaType".into(), "data".into()])
    );
    assert_eq!(portrait_schema["properties"]["data"]["type"], "string");

    let portrait = zzu::auth::Portrait {
        media_type: "image/png".into(),
        bytes: vec![0x89, b'P', b'N', b'G'],
    };
    let encoded_portrait = serde_json::to_value(&portrait).unwrap();
    assert!(encoded_portrait["data"].is_string());

    let page_schema = schema_value::<zzu::auth::DevicePage>();
    assert!(page_schema["$defs"]["LoginDevice"].is_object());
    let first_used = &page_schema["$defs"]["LoginDevice"]["properties"]["firstUsedTime"];
    let serialized_first_used = serde_json::to_string(first_used).unwrap();
    assert!(serialized_first_used.contains("zoned-date-time"));
}

#[cfg(feature = "eas")]
#[test]
fn eas_models_preserve_canonical_shapes_open_enums_and_recursive_modules() {
    let _ = schema_for!(zzu::eas::CalendarExportOptions);
    let _ = schema_for!(zzu::eas::Campus);
    let _ = schema_for!(zzu::eas::Building);
    let _ = schema_for!(zzu::eas::RoomType);
    let _ = schema_for!(zzu::eas::ClassroomCatalog);
    let _ = schema_for!(zzu::eas::ClassroomQuery);
    let _ = schema_for!(zzu::eas::ClassroomFetchOptions);
    let _ = schema_for!(zzu::eas::ClassroomOccupancy);
    let _ = schema_for!(zzu::eas::ClassroomPage);
    let _ = schema_for!(zzu::eas::NamedEntity);
    let _ = schema_for!(zzu::eas::Exam);
    let _ = schema_for!(zzu::eas::ProgramCompletion);
    let _ = schema_for!(zzu::eas::TimetableOptions);
    let _ = schema_for!(zzu::eas::Timetable);
    let _ = schema_for!(zzu::eas::Transcript);

    // CourseUnit uses #[serde(try_from = "RawCourseUnit")] + Time fields.
    let unit_schema = schema_value::<zzu::eas::CourseUnit>();
    assert_eq!(
        property_keys(&unit_schema),
        BTreeSet::from([
            "index".into(),
            "nameZh".into(),
            "nameEn".into(),
            "dayPart".into(),
            "segmentIndex".into(),
            "startTime".into(),
            "endTime".into(),
        ])
    );
    assert_eq!(
        unit_schema["properties"]["startTime"]["format"],
        "partial-time"
    );

    // Open string enums use anyOf with snake_case constants and a string fallback.
    for open_schema in [
        schema_value::<zzu::eas::DayPart>(),
        schema_value::<zzu::eas::SemesterSeason>(),
        schema_value::<zzu::eas::CompletionStatus>(),
        schema_value::<zzu::eas::RoomActivityType>(),
        schema_value::<zzu::eas::ScheduleState>(),
        schema_value::<zzu::eas::SessionKind>(),
        schema_value::<zzu::eas::SessionState>(),
        schema_value::<zzu::eas::StudyType>(),
        schema_value::<zzu::eas::GradeState>(),
    ] {
        let branches = open_schema["anyOf"]
            .as_array()
            .expect("open string enum uses anyOf");
        assert!(
            branches
                .iter()
                .any(|branch| branch["type"] == "string" && branch.get("const").is_none()),
            "missing arbitrary string fallback in {open_schema}"
        );
        assert!(
            branches
                .iter()
                .any(|branch| branch.get("const").is_some_and(Value::is_string)),
            "missing documented string constants in {open_schema}"
        );
    }

    let day_part_consts: BTreeSet<_> = schema_value::<zzu::eas::DayPart>()["anyOf"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|branch| branch["const"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        day_part_consts,
        BTreeSet::from([
            "morning".into(),
            "noon".into(),
            "afternoon".into(),
            "evening".into(),
        ])
    );
}

#[cfg(feature = "ecard")]
#[test]
fn ecard_models_align_room_id_recharge_order_form_fields_and_decimals() {
    let _ = schema_for!(zzu::ecard::PaymentCode);
    let _ = schema_for!(zzu::ecard::EnergyAccount);
    let _ = schema_for!(zzu::ecard::RechargeConfig);
    let _ = schema_for!(zzu::ecard::PaymentChannel);
    let _ = schema_for!(zzu::ecard::PaymentDevice);
    let _ = schema_for!(zzu::ecard::PaymentContext);
    let _ = schema_for!(zzu::ecard::TransactionKind);
    let _ = schema_for!(zzu::ecard::TransactionQuery);
    let _ = schema_for!(zzu::ecard::Transaction);

    let room_schema = schema_value::<zzu::ecard::RoomId>();
    assert_eq!(room_schema["title"], "RoomId");
    assert_eq!(room_schema["type"], "string");
    assert_eq!(room_schema["pattern"], r"^[^-]+-[^-]+--[^-]+-[^-]+$");

    let order_json = json!({
        "id": "school-order",
        "amount": 10,
        "checkoutUrl": "https://payment.v.zzu.edu.cn/WapCashDesk/e-pay/payways.html",
        "payment": {
            "orderInfo": {
                "partnerid": "partner",
                "journo": "school-order",
                "merchantno": "merchant",
                "channelno": "103",
                "txamt": "1000"
            },
            "callPaywayid": "",
            "callAccountid": "",
            "extend": null
        }
    });
    let order: zzu::ecard::RechargeOrder = serde_json::from_value(order_json).unwrap();
    let reencoded_order = serde_json::to_value(&order).unwrap();
    let order_schema = schema_value::<zzu::ecard::RechargeOrder>();
    assert_eq!(order_schema["title"], "RechargeOrder");
    assert_eq!(
        property_keys(&order_schema),
        reencoded_order
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(required_keys(&order_schema), property_keys(&order_schema));
    if let Some(defs) = order_schema["$defs"].as_object() {
        assert!(!defs.contains_key("WapPayment"));
        assert!(!defs.contains_key("OrderInfo"));
    }

    let action: zzu::ecard::PaymentAction = serde_json::from_value(json!({
        "kind": "submit_form",
        "url": "https://openapi.alipay.com/gateway.do",
        "fields": [{"name": "biz_content", "value": "{}"}]
    }))
    .unwrap();
    let encoded_action = serde_json::to_value(&action).unwrap();
    assert!(encoded_action["fields"][0].is_object());

    let action_schema = schema_value::<zzu::ecard::PaymentAction>();
    if let Some(defs) = action_schema["$defs"].as_object() {
        assert!(!defs.contains_key("RawFormField"));
    }
    let submit_variant = action_schema["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variant| variant["properties"]["kind"]["const"] == "submit_form")
        .expect("submit_form variant exists");
    let field_item_schema = &submit_variant["properties"]["fields"]["items"];
    assert_eq!(
        property_keys(field_item_schema),
        BTreeSet::from(["name".into(), "value".into()])
    );
    assert_eq!(
        required_keys(field_item_schema),
        BTreeSet::from(["name".into(), "value".into()])
    );
}

#[cfg(feature = "mail")]
#[test]
fn mail_models_generate_valid_schemas() {
    let page_schema = schema_value::<zzu::mail::MailMessagePage>();
    assert_eq!(
        property_keys(&page_schema),
        BTreeSet::from([
            "items".into(),
            "page".into(),
            "pageSize".into(),
            "totalItems".into(),
            "totalPages".into(),
        ])
    );
    let msg_schema = &page_schema["$defs"]["MailMessage"];
    assert_eq!(
        msg_schema["properties"]["receivedTime"]["format"],
        "zoned-date-time"
    );
}

#[cfg(feature = "network")]
#[test]
fn network_models_generate_valid_schemas_without_auth() {
    let _ = schema_for!(zzu::network::IspInfo);
    let _ = schema_for!(zzu::network::PortalEncryption);
    let _ = schema_for!(zzu::network::AuthResult);
    let _ = schema_for!(zzu::network::PortalSourceAddress);

    let portal_schema = schema_value::<zzu::network::PortalInfo>();
    assert_eq!(
        property_keys(&portal_schema),
        BTreeSet::from(["authUrl".into(), "portalServerUrl".into(), "userIp".into(),])
    );
    assert_eq!(portal_schema["properties"]["authUrl"]["format"], "uri");

    let device_schema = schema_value::<zzu::network::OnlineDevice>();
    assert_eq!(
        device_schema["properties"]["loginTime"]["format"],
        "zoned-date-time"
    );
}
