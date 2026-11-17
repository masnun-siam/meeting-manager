//! The reply shape every command returns: `{ok: true, ...}` or `{ok: false, code, message}`,
//! the same as the native host's. A `message` never holds transcript text or secrets.

use std::fmt::Display;

use serde_json::{Value, json};

/// `fields` with `ok: true` added. `fields` must be a JSON object.
pub fn ok(fields: Value) -> Value {
    debug_assert!(fields.is_object(), "reply fields must be a JSON object");
    let mut reply = match fields {
        Value::Object(map) => map,
        other => serde_json::Map::from_iter([("value".to_string(), other)]),
    };
    reply.insert("ok".into(), Value::Bool(true));
    Value::Object(reply)
}

pub fn err(code: &str, message: impl Display) -> Value {
    json!({"ok": false, "code": code, "message": message.to_string()})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_adds_the_flag_and_keeps_the_fields() {
        assert_eq!(ok(json!({"a": 1})), json!({"ok": true, "a": 1}));
        assert_eq!(
            ok(json!({"a": 1, "b": [2]})),
            json!({"ok": true, "a": 1, "b": [2]})
        );
        assert_eq!(ok(json!({})), json!({"ok": true}));
    }

    #[test]
    fn ok_cannot_be_overridden_by_the_fields() {
        assert_eq!(
            ok(json!({"ok": false, "a": 1})),
            json!({"ok": true, "a": 1})
        );
    }

    #[test]
    fn err_has_the_code_and_the_message() {
        assert_eq!(
            err("c", "m"),
            json!({"ok": false, "code": "c", "message": "m"})
        );
        assert_eq!(err("c", 7)["message"], "7");
    }
}

#[cfg(test)]
mod ci_red_probe {
    #[test]
    fn injected_failure_tauri() {
        assert_eq!(1 + 1, 3, "onboarding #186 CI red probe");
    }
}
