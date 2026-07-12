use std::path::PathBuf;

use cddlc_ir::IrModule;
use cddlc_validate::{validate, Value};

fn lower(src: &str) -> IrModule {
    let parsed = cddlc_parser::parse_cddl(src, PathBuf::from("test.cddl")).expect("parse");
    cddlc_ir::lower(&parsed.value, 16, 64).expect("lower").module
}

fn json(s: &str) -> Value {
    let v: serde_json::Value = serde_json::from_str(s).expect("valid json fixture");
    Value::from(v)
}

fn assert_ok(module: &IrModule, ty: &str, value: &Value) {
    if let Err(errors) = validate(module, Some(ty), value) {
        panic!("expected '{ty}' to validate, got errors: {errors:?}");
    }
}

fn assert_fails(module: &IrModule, ty: &str, value: &Value) -> Vec<cddlc_validate::ValidationError> {
    match validate(module, Some(ty), value) {
        Ok(()) => panic!("expected '{ty}' to fail validation, but it passed"),
        Err(errors) => errors,
    }
}

// ── structs.cddl ─────────────────────────────────────────────────────────────

const STRUCTS: &str = include_str!("../../backend-buildtest/schemas/structs.cddl");

#[test]
fn struct_valid() {
    let m = lower(STRUCTS);
    assert_ok(&m, "sensor", &json(r#"{"id":1,"label":"a","value":1.5}"#));
}

#[test]
fn struct_missing_required_field() {
    let m = lower(STRUCTS);
    let errors = assert_fails(&m, "sensor", &json(r#"{"id":1,"label":"a"}"#));
    assert!(errors.iter().any(|e| e.message.contains("missing required field 'value'")), "{errors:?}");
}

#[test]
fn struct_unknown_field_rejected() {
    let m = lower(STRUCTS);
    let errors = assert_fails(&m, "sensor", &json(r#"{"id":1,"label":"a","value":1.5,"extra":true}"#));
    assert!(errors.iter().any(|e| e.message.contains("unknown field")), "{errors:?}");
}

#[test]
fn struct_wrong_field_type() {
    let m = lower(STRUCTS);
    let errors = assert_fails(&m, "sensor", &json(r#"{"id":"nope","label":"a","value":1.5}"#));
    assert!(errors.iter().any(|e| e.path_string() == "$.id"), "{errors:?}");
}

#[test]
fn struct_optional_field_absent_and_present() {
    let m = lower(STRUCTS);
    assert_ok(&m, "reading", &json(r#"{"sensor-id":1,"value":1.0}"#));
    assert_ok(&m, "reading", &json(r#"{"sensor-id":1,"value":1.0,"unit":"C"}"#));
}

#[test]
fn struct_nested_reference_and_path() {
    let m = lower(STRUCTS);
    assert_ok(&m, "device", &json(r#"{"id":1,"sensor":{"id":2,"label":"x","value":1.0}}"#));

    let errors = assert_fails(&m, "device", &json(r#"{"id":1,"sensor":{"id":2,"label":"x","value":"bad"}}"#));
    assert!(errors.iter().any(|e| e.path_string() == "$.sensor.value"), "{errors:?}");
}

// ── aliases.cddl ─────────────────────────────────────────────────────────────

const ALIASES: &str = include_str!("../../backend-buildtest/schemas/aliases.cddl");

#[test]
fn alias_simple() {
    let m = lower(ALIASES);
    assert_ok(&m, "device-id", &json("5"));
    assert_fails(&m, "device-id", &json(r#""five""#));
}

#[test]
fn alias_size_violation() {
    let m = lower(ALIASES);
    let exactly_32 = "x".repeat(32);
    let too_short = "x".repeat(10);

    assert_ok(&m, "device-name", &json(&format!("\"{exactly_32}\"")));
    let errors = assert_fails(&m, "device-name", &json(&format!("\"{too_short}\"")));
    assert!(errors.iter().any(|e| e.message.contains(".size")), "{errors:?}");
}

#[test]
fn alias_range_constraint() {
    let m = lower(ALIASES);
    assert_ok(&m, "temperature", &json("20"));
    assert_ok(&m, "temperature", &json("85"));
    assert_ok(&m, "temperature", &json("-40"));
    assert_fails(&m, "temperature", &json("86"));
    assert_fails(&m, "temperature", &json("-41"));
}

#[test]
fn alias_bstr_base64url() {
    let m = lower(ALIASES);
    // base64url (no padding) for bytes [1,2,3]
    assert_ok(&m, "payload", &json(r#""AQID""#));
    assert_fails(&m, "payload", &json(r#""not base64url!!""#));
}

// ── enums.cddl ───────────────────────────────────────────────────────────────

const ENUMS: &str = include_str!("../../backend-buildtest/schemas/enums.cddl");

#[test]
fn enum_string_choice() {
    let m = lower(ENUMS);
    assert_ok(&m, "status", &json(r#""ok""#));
    assert_ok(&m, "status", &json(r#""warn""#));
    let errors = assert_fails(&m, "status", &json(r#""bad""#));
    assert!(errors.iter().any(|e| e.message.contains("matched no variant")), "{errors:?}");
}

#[test]
fn enum_integer_choice() {
    let m = lower(ENUMS);
    assert_ok(&m, "priority", &json("2"));
    assert_fails(&m, "priority", &json("5"));
}

// ── tagged.cddl ──────────────────────────────────────────────────────────────

const TAGGED: &str = include_str!("../../backend-buildtest/schemas/tagged.cddl");

#[test]
fn tagged_alias_json_degrades_untagged() {
    let m = lower(TAGGED);
    // JSON has no tag concept — a bare uint satisfies a tagged-uint alias.
    assert_ok(&m, "timestamp", &json("1234"));
}

#[test]
fn tagged_alias_cbor_checks_tag_number() {
    let m = lower(TAGGED);
    let good = Value::from(ciborium::Value::Tag(1, Box::new(ciborium::Value::Integer(1234.into()))));
    assert_ok(&m, "timestamp", &good);

    let wrong_tag = Value::from(ciborium::Value::Tag(2, Box::new(ciborium::Value::Integer(1234.into()))));
    let errors = assert_fails(&m, "timestamp", &wrong_tag);
    assert!(errors.iter().any(|e| e.message.contains("expected CBOR tag 1")), "{errors:?}");
}

#[test]
fn tagged_struct_cbor() {
    let m = lower(TAGGED);
    let cbor_struct = ciborium::Value::Tag(100, Box::new(ciborium::Value::Map(vec![
        (ciborium::Value::Text("id".into()), ciborium::Value::Integer(1.into())),
        (ciborium::Value::Text("value".into()), ciborium::Value::Float(1.5)),
    ])));
    assert_ok(&m, "tagged-sensor", &Value::from(cbor_struct));
}

// ── JSON / CBOR canonical-value equivalence ───────────────────────────────────

#[test]
fn json_and_cbor_validate_identically() {
    let m = lower(STRUCTS);

    let from_json = json(r#"{"id":1,"label":"a","value":1.5}"#);
    let from_cbor = Value::from(ciborium::Value::Map(vec![
        (ciborium::Value::Text("id".into()), ciborium::Value::Integer(1.into())),
        (ciborium::Value::Text("label".into()), ciborium::Value::Text("a".into())),
        (ciborium::Value::Text("value".into()), ciborium::Value::Float(1.5)),
    ]));

    assert_eq!(validate(&m, Some("sensor"), &from_json).is_ok(), true);
    assert_eq!(validate(&m, Some("sensor"), &from_cbor).is_ok(), true);
}

// ── iot_sensor.cddl (end to end) ──────────────────────────────────────────────

const IOT_SENSOR: &str = include_str!("../../backend-buildtest/schemas/iot_sensor.cddl");

#[test]
fn iot_sensor_valid_message() {
    let m = lower(IOT_SENSOR);
    let doc = json(r#"{
        "version": 1,
        "device-id": "abcdefghijklmnop",
        "timestamp": 1234567890,
        "readings": [
            {"sensor-type": "temperature", "value": 21.5},
            {"sensor-type": "humidity", "value": 55.0, "unit": "%"}
        ]
    }"#);
    assert_ok(&m, "sensor-message", &doc);
}

#[test]
fn iot_sensor_device_id_wrong_size() {
    let m = lower(IOT_SENSOR);
    let doc = json(r#"{
        "version": 1,
        "device-id": "short",
        "timestamp": 1234567890,
        "readings": []
    }"#);
    let errors = assert_fails(&m, "sensor-message", &doc);
    assert!(errors.iter().any(|e| e.path_string() == "$.device-id"), "{errors:?}");
}

#[test]
fn iot_sensor_nested_enum_failure_has_full_path() {
    let m = lower(IOT_SENSOR);
    let doc = json(r#"{
        "version": 1,
        "device-id": "abcdefghijklmnop",
        "timestamp": 1234567890,
        "readings": [
            {"sensor-type": "not-a-real-type", "value": 21.5}
        ]
    }"#);
    let errors = assert_fails(&m, "sensor-message", &doc);
    assert!(errors.iter().any(|e| e.path_string() == "$.readings[0].sensor-type"), "{errors:?}");
}

// ── .regexp ───────────────────────────────────────────────────────────────────

#[test]
fn regexp_constraint() {
    let m = lower(r#"code = tstr .regexp "^[A-Z]{3}[0-9]{2}$""#);
    assert_ok(&m, "code", &json(r#""ABC12""#));
    assert_fails(&m, "code", &json(r#""abc12""#));
}

// ── .cbor / .cborseq embedded ──────────────────────────────────────────────────

#[test]
fn cbor_embedded_constraint() {
    let m = lower("inner = uint\nwrapper = bstr .cbor inner");

    let mut encoded = Vec::new();
    ciborium::into_writer(&ciborium::Value::Integer(42.into()), &mut encoded).unwrap();
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, &encoded);

    assert_ok(&m, "wrapper", &json(&format!(r#""{b64}""#)));

    let mut bad_encoded = Vec::new();
    ciborium::into_writer(&ciborium::Value::Text("not a uint".into()), &mut bad_encoded).unwrap();
    let bad_b64 = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, &bad_encoded);
    assert_fails(&m, "wrapper", &json(&format!(r#""{bad_b64}""#)));
}

#[test]
fn cborseq_embedded_constraint() {
    let m = lower("inner = uint\nwrapper = bstr .cborseq inner");

    let mut encoded = Vec::new();
    for n in [1u64, 2, 3] {
        ciborium::into_writer(&ciborium::Value::Integer(n.into()), &mut encoded).unwrap();
    }
    let cbor = Value::from(ciborium::Value::Bytes(encoded));
    assert_ok(&m, "wrapper", &cbor);
}

// ── lo..hi range operator (regression for the min-bound fix) ───────────────────

#[test]
fn range_operator_enforces_both_bounds() {
    let m = lower("byte = 0..255");
    assert_ok(&m, "byte", &json("0"));
    assert_ok(&m, "byte", &json("255"));
    let errors = assert_fails(&m, "byte", &json("300"));
    assert!(errors.iter().any(|e| e.message.contains("above the maximum 255")), "{errors:?}");
}
