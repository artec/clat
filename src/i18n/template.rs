use serde_json::Value;
use std::collections::BTreeMap;

pub(super) fn format(text: &str, args: &BTreeMap<String, Value>) -> String {
    let mut output = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        output.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let Some(end) = tail.find('}') else {
            output.push_str(&rest[start..]);
            return output;
        };
        let key = &tail[..end];
        if let Some(value) = args.get(key) {
            match value {
                Value::String(s) => output.push_str(s),
                _ => output.push_str(&value.to_string()),
            }
        } else {
            output.push_str(&rest[start..start + end + 2]);
        }
        rest = &tail[end + 1..];
    }
    output.push_str(rest);
    output
}
