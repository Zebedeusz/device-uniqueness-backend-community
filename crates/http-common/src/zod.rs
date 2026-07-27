//! zod v4 message fragments, as they appear inside the legacy 400 field
//! errors (golden-captured; both services' fixtures pin them).

/// The zod v4 name for a JSON value's type, as it appears inside the legacy
/// `Invalid input: expected …, received …` messages.
pub fn type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// The received-type name for an optional field (`undefined` when absent).
pub fn received_name(value: Option<&serde_json::Value>) -> &'static str {
    value.map_or("undefined", type_name)
}

/// The zod v4 type-mismatch message: `Invalid input: expected …, received …`.
pub fn invalid_input(expected: &str, received: &str) -> String {
    format!("Invalid input: expected {expected}, received {received}")
}

/// The zod v4 `min(1)` failure message for strings.
pub const TOO_SMALL_MIN1: &str = "Too small: expected string to have >=1 characters";

/// The zod v4 `positive()` failure message for numbers (captured from the
/// usernames search `limit` param).
pub const TOO_SMALL_POSITIVE_NUMBER: &str = "Too small: expected number to be >0";

/// The zod v4 `array().max(n)` failure message (captured from the usernames
/// availability `usernames` field).
pub fn too_big_array(max_items: usize) -> String {
    format!("Too big: expected array to have <={max_items} items")
}

/// The zod v4 default `regex()` failure message: the pattern is rendered in
/// JS `RegExp#toString()` form, e.g.
/// `Invalid string: must match pattern /^([a-z]{6,})$/`.
pub fn invalid_string_pattern(js_pattern: &str) -> String {
    format!("Invalid string: must match pattern {js_pattern}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn names_match_zod_v4() {
        assert_eq!(type_name(&json!(null)), "null");
        assert_eq!(type_name(&json!(5)), "number");
        assert_eq!(type_name(&json!([1])), "array");
        assert_eq!(received_name(None), "undefined");
        assert_eq!(
            invalid_input("string", received_name(Some(&json!(5)))),
            "Invalid input: expected string, received number"
        );
    }
}
