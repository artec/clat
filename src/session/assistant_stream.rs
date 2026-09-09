//! Released-v2 `AssistantStreamRecord` codec. DSH embeds one compact, timed
//! model attempt in `assistant/message` or `assistant/attempt`; delta runs keep
//! every original boundary, while non-packable chunks remain single records.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum AssistantStreamRecord {
    #[serde(rename = "text-chunks")]
    TextChunks {
        time0: i64,
        index: u64,
        dt: Vec<i64>,
        texts: Vec<String>,
    },
    #[serde(rename = "reasoning-chunks")]
    ReasoningChunks {
        time0: i64,
        index: u64,
        dt: Vec<i64>,
        texts: Vec<String>,
    },
    #[serde(rename = "tool-call-chunks")]
    ToolCallChunks {
        time0: i64,
        index: u64,
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        dt: Vec<i64>,
        args: Vec<String>,
    },
    #[serde(rename = "chunk")]
    Chunk { time: i64, chunk: Value },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TimedChunk {
    pub(crate) time: i64,
    pub(crate) chunk: Value,
}

#[derive(Default)]
pub(crate) struct AssistantStreamAccumulator {
    records: Vec<AssistantStreamRecord>,
}

impl AssistantStreamAccumulator {
    pub(crate) fn push(&mut self, time: i64, chunk: Value) {
        let kind = chunk.get("type").and_then(Value::as_str);
        let index = chunk.get("index").and_then(Value::as_u64);
        match (kind, index) {
            (Some("text-delta"), Some(index)) => {
                let Some(text) = chunk.get("text").and_then(Value::as_str) else {
                    self.records
                        .push(AssistantStreamRecord::Chunk { time, chunk });
                    return;
                };
                if let Some(AssistantStreamRecord::TextChunks {
                    time0,
                    index: prior_index,
                    dt,
                    texts,
                }) = self.records.last_mut()
                    && *prior_index == index
                    && let Some(previous) = member_time(*time0, dt)
                    && let Some(gap) = time.checked_sub(previous)
                {
                    dt.push(gap);
                    texts.push(text.to_owned());
                    return;
                }
                self.records.push(AssistantStreamRecord::TextChunks {
                    time0: time,
                    index,
                    dt: Vec::new(),
                    texts: vec![text.to_owned()],
                });
            }
            (Some("reasoning-delta"), Some(index)) => {
                let Some(text) = chunk.get("text").and_then(Value::as_str) else {
                    self.records
                        .push(AssistantStreamRecord::Chunk { time, chunk });
                    return;
                };
                if let Some(AssistantStreamRecord::ReasoningChunks {
                    time0,
                    index: prior_index,
                    dt,
                    texts,
                }) = self.records.last_mut()
                    && *prior_index == index
                    && let Some(previous) = member_time(*time0, dt)
                    && let Some(gap) = time.checked_sub(previous)
                {
                    dt.push(gap);
                    texts.push(text.to_owned());
                    return;
                }
                self.records.push(AssistantStreamRecord::ReasoningChunks {
                    time0: time,
                    index,
                    dt: Vec::new(),
                    texts: vec![text.to_owned()],
                });
            }
            (Some("tool-call-delta"), Some(index)) => {
                let id = chunk.get("id").and_then(Value::as_str);
                let args = chunk.get("argumentsDelta").and_then(Value::as_str);
                let name = chunk.get("name").and_then(Value::as_str);
                if id.is_none_or(str::is_empty)
                    || args.is_none()
                    || chunk
                        .get("name")
                        .is_some_and(|_| name.is_none_or(str::is_empty))
                {
                    self.records
                        .push(AssistantStreamRecord::Chunk { time, chunk });
                    return;
                }
                let id = id.expect("checked");
                let args = args.expect("checked");
                if let Some(AssistantStreamRecord::ToolCallChunks {
                    time0,
                    index: prior_index,
                    id: prior_id,
                    name: prior_name,
                    dt,
                    args: prior_args,
                }) = self.records.last_mut()
                    && *prior_index == index
                    && prior_id == id
                    && prior_name.as_deref() == name
                    && let Some(previous) = member_time(*time0, dt)
                    && let Some(gap) = time.checked_sub(previous)
                {
                    dt.push(gap);
                    prior_args.push(args.to_owned());
                    return;
                }
                self.records.push(AssistantStreamRecord::ToolCallChunks {
                    time0: time,
                    index,
                    id: id.to_owned(),
                    name: name.map(str::to_owned),
                    dt: Vec::new(),
                    args: vec![args.to_owned()],
                });
            }
            _ => self
                .records
                .push(AssistantStreamRecord::Chunk { time, chunk }),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub(crate) fn take_value(&mut self) -> Value {
        serde_json::to_value(std::mem::take(&mut self.records))
            .expect("assistant stream records are plain JSON")
    }
}

fn member_time(time0: i64, dt: &[i64]) -> Option<i64> {
    dt.iter()
        .try_fold(time0, |time, gap| time.checked_add(*gap))
}

/// Decode and expand all four released-v2 record variants. Expansion doubles
/// as strict shape/timestamp validation while preserving every delta boundary.
pub(crate) fn expand_assistant_stream(value: &Value) -> Result<Vec<TimedChunk>, String> {
    #[cfg(test)]
    EXPANSION_CALLS.with(|calls| calls.set(calls.get() + 1));
    let records = value
        .as_array()
        .ok_or_else(|| "stream must be an array".to_owned())?;
    let mut output = Vec::new();
    for candidate in records {
        let object = candidate
            .as_object()
            .ok_or_else(|| "Assistant stream record must be an object".to_owned())?;
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| "Assistant stream record lacks type".to_owned())?;
        match kind {
            "text-chunks" | "reasoning-chunks" => {
                exact_keys(object, &["type", "time0", "index", "dt", "texts"], kind)?;
                let time0 = signed_integer(object.get("time0"), "time0")?;
                let index = count(object.get("index"), "index")?;
                let dt = integer_array(object.get("dt"), "dt")?;
                let texts = string_array(object.get("texts"), "texts")?;
                validate_run(&dt, texts.len(), time0, kind)?;
                let chunk_kind = if kind == "text-chunks" {
                    "text-delta"
                } else {
                    "reasoning-delta"
                };
                let mut time = time0;
                for (position, text) in texts.into_iter().enumerate() {
                    if position > 0 {
                        time = time
                            .checked_add(dt[position - 1])
                            .ok_or_else(|| format!("{kind} member times overflow"))?;
                    }
                    output.push(TimedChunk {
                        time,
                        chunk: serde_json::json!({
                            "type": chunk_kind,
                            "index": index,
                            "text": text,
                        }),
                    });
                }
            }
            "tool-call-chunks" => {
                let keys: &[&str] = if object.contains_key("name") {
                    &["type", "time0", "index", "dt", "id", "name", "args"]
                } else {
                    &["type", "time0", "index", "dt", "id", "args"]
                };
                exact_keys(object, keys, kind)?;
                let time0 = signed_integer(object.get("time0"), "time0")?;
                let index = count(object.get("index"), "index")?;
                let dt = integer_array(object.get("dt"), "dt")?;
                let args = string_array(object.get("args"), "args")?;
                let id = non_empty_string(object.get("id"), "id")?;
                let name = object
                    .get("name")
                    .map(|value| non_empty_string(Some(value), "name"))
                    .transpose()?;
                validate_run(&dt, args.len(), time0, kind)?;
                let mut time = time0;
                for (position, arguments_delta) in args.into_iter().enumerate() {
                    if position > 0 {
                        time = time
                            .checked_add(dt[position - 1])
                            .ok_or_else(|| format!("{kind} member times overflow"))?;
                    }
                    let mut chunk = serde_json::json!({
                        "type": "tool-call-delta",
                        "index": index,
                        "id": id,
                        "argumentsDelta": arguments_delta,
                    });
                    if let Some(name) = &name {
                        chunk["name"] = Value::String(name.clone());
                    }
                    output.push(TimedChunk { time, chunk });
                }
            }
            "chunk" => {
                exact_keys(object, &["type", "time", "chunk"], kind)?;
                let time = signed_integer(object.get("time"), "time")?;
                let chunk = object
                    .get("chunk")
                    .filter(|value| value.is_object())
                    .cloned()
                    .ok_or_else(|| "raw chunk must be an object".to_owned())?;
                output.push(TimedChunk { time, chunk });
            }
            other => return Err(format!("unsupported Assistant stream record `{other}`")),
        }
    }
    Ok(output)
}

/// Validate a released-v2 embedded stream without materializing its expanded
/// chunks. Cold session admission only needs a structural verdict; building a
/// fresh `TimedChunk` and JSON object for every historical token boundary made
/// large-session open time proportional to the full stream twice over.
pub(crate) fn validate_assistant_stream(value: &Value) -> Result<(), String> {
    let records = value
        .as_array()
        .ok_or_else(|| "stream must be an array".to_owned())?;
    for candidate in records {
        let object = candidate
            .as_object()
            .ok_or_else(|| "Assistant stream record must be an object".to_owned())?;
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| "Assistant stream record lacks type".to_owned())?;
        match kind {
            "text-chunks" | "reasoning-chunks" => {
                exact_keys(object, &["type", "time0", "index", "dt", "texts"], kind)?;
                let time0 = signed_integer(object.get("time0"), "time0")?;
                count(object.get("index"), "index")?;
                let dt = integer_values(object.get("dt"), "dt")?;
                let texts = string_values(object.get("texts"), "texts")?;
                validate_borrowed_run(dt, texts.len(), time0, kind)?;
            }
            "tool-call-chunks" => {
                let keys: &[&str] = if object.contains_key("name") {
                    &["type", "time0", "index", "dt", "id", "name", "args"]
                } else {
                    &["type", "time0", "index", "dt", "id", "args"]
                };
                exact_keys(object, keys, kind)?;
                let time0 = signed_integer(object.get("time0"), "time0")?;
                count(object.get("index"), "index")?;
                non_empty_str(object.get("id"), "id")?;
                if object.contains_key("name") {
                    non_empty_str(object.get("name"), "name")?;
                }
                let dt = integer_values(object.get("dt"), "dt")?;
                let args = string_values(object.get("args"), "args")?;
                validate_borrowed_run(dt, args.len(), time0, kind)?;
            }
            "chunk" => {
                exact_keys(object, &["type", "time", "chunk"], kind)?;
                signed_integer(object.get("time"), "time")?;
                if !object.get("chunk").is_some_and(Value::is_object) {
                    return Err("raw chunk must be an object".to_owned());
                }
            }
            other => return Err(format!("unsupported Assistant stream record `{other}`")),
        }
    }
    Ok(())
}

fn integer_values<'a>(value: Option<&'a Value>, label: &str) -> Result<&'a [Value], String> {
    let values = value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{label} must be an integer array"))?;
    for value in values {
        signed_integer(Some(value), label)?;
    }
    Ok(values)
}

fn string_values<'a>(value: Option<&'a Value>, label: &str) -> Result<&'a [Value], String> {
    let values = value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{label} must be a string array"))?;
    if values.iter().any(|value| !value.is_string()) {
        return Err(format!("{label} must be a string array"));
    }
    Ok(values)
}

fn validate_borrowed_run(
    dt: &[Value],
    members: usize,
    time0: i64,
    label: &str,
) -> Result<(), String> {
    if members == 0 {
        return Err(format!("{label} members must be non-empty"));
    }
    if dt.len() + 1 != members {
        return Err(format!("{label} dt length must be one less than members"));
    }
    let final_time = dt.iter().try_fold(time0, |time, gap| {
        time.checked_add(
            gap.as_i64()
                .expect("integer_values admitted every timestamp delta"),
        )
    });
    let final_time = final_time.ok_or_else(|| format!("{label} member times overflow"))?;
    safe_i64(final_time, &format!("{label} member time"))?;
    Ok(())
}

fn non_empty_str<'a>(value: Option<&'a Value>, label: &str) -> Result<&'a str, String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{label} must be a non-empty string"))
}

#[cfg(test)]
thread_local! {
    static EXPANSION_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn take_expansion_calls_for_test() -> usize {
    EXPANSION_CALLS.with(|calls| calls.replace(0))
}

fn validate_run(dt: &[i64], members: usize, time0: i64, label: &str) -> Result<(), String> {
    if members == 0 {
        return Err(format!("{label} members must be non-empty"));
    }
    if dt.len() + 1 != members {
        return Err(format!("{label} dt length must be one less than members"));
    }
    let final_time =
        member_time(time0, dt).ok_or_else(|| format!("{label} member times overflow"))?;
    safe_i64(final_time, &format!("{label} member time"))?;
    Ok(())
}

fn exact_keys(
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
    label: &str,
) -> Result<(), String> {
    if object.len() != keys.len() || !keys.iter().all(|key| object.contains_key(*key)) {
        return Err(format!(
            "{label} Assistant stream record must contain exactly {}",
            keys.join(", ")
        ));
    }
    Ok(())
}

fn signed_integer(value: Option<&Value>, label: &str) -> Result<i64, String> {
    let value = value
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("{label} must be a safe integer"))?;
    safe_i64(value, label)
}

fn count(value: Option<&Value>, label: &str) -> Result<u64, String> {
    let value = value
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{label} must be a non-negative safe integer"))?;
    if value > 9_007_199_254_740_991 {
        return Err(format!("{label} must be a non-negative safe integer"));
    }
    Ok(value)
}

fn safe_i64(value: i64, label: &str) -> Result<i64, String> {
    if !(-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&value) {
        return Err(format!("{label} must be a safe integer"));
    }
    Ok(value)
}

fn integer_array(value: Option<&Value>, label: &str) -> Result<Vec<i64>, String> {
    value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{label} must be an integer array"))?
        .iter()
        .map(|value| signed_integer(Some(value), label))
        .collect()
}

fn string_array(value: Option<&Value>, label: &str) -> Result<Vec<String>, String> {
    value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{label} must be a string array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{label} must be a string array"))
        })
        .collect()
}

fn non_empty_string(value: Option<&Value>, label: &str) -> Result<String, String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{label} must be a non-empty string"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_all_four_record_variants_without_joining_boundaries() {
        let stream = serde_json::json!([
            {"type":"text-chunks","time0":10,"index":0,"dt":[2],"texts":["a","b"]},
            {"type":"reasoning-chunks","time0":20,"index":1,"dt":[],"texts":["r"]},
            {"type":"tool-call-chunks","time0":30,"index":2,"dt":[1],"id":"c","name":"tool","args":["{","}"]},
            {"type":"chunk","time":40,"chunk":{"type":"finish","reason":{"kind":"stop"}}}
        ]);
        validate_assistant_stream(&stream).expect("valid v2 stream without expansion");
        let expanded = expand_assistant_stream(&stream).expect("valid v2 stream");
        assert_eq!(expanded.len(), 6);
        assert_eq!(expanded[1].time, 12);
        assert_eq!(expanded[1].chunk["text"], "b");
        assert_eq!(expanded[4].chunk["argumentsDelta"], "}");
        assert_eq!(expanded[5].chunk["type"], "finish");
    }

    #[test]
    fn validation_only_path_matches_expansion_verdicts() {
        let cases = [
            serde_json::json!([]),
            serde_json::json!([
                {"type":"text-chunks","time0":10,"index":0,"dt":[2],"texts":["a","b"]},
                {"type":"reasoning-chunks","time0":20,"index":1,"dt":[],"texts":["r"]},
                {"type":"tool-call-chunks","time0":30,"index":2,"dt":[],"id":"c","args":["{}"]},
                {"type":"chunk","time":40,"chunk":{"type":"finish"}}
            ]),
            serde_json::json!([{"type":"text-chunks","time0":10,"index":0,"dt":[],"texts":[]}]),
            serde_json::json!([{"type":"reasoning-chunks","time0":10,"index":0,"dt":[1],"texts":["only"]}]),
            // G1 溢出腿（2026-09-09 SD-C1 审计 Mu-B 不红的缺口）：
            // time0 本身是安全整数，终时越过安全域——删除借用侧终时
            // 检查后 validate 会误放行而 expand 拒绝，判决漂移必须红。
            serde_json::json!([{"type":"text-chunks","time0":9007199254740991i64,"index":0,"dt":[1],"texts":["a","b"]}]),
            serde_json::json!([{"type":"tool-call-chunks","time0":10,"index":0,"dt":[],"id":"","args":["{}"]}]),
            serde_json::json!([{"type":"tool-call-chunks","time0":10,"index":0,"dt":[],"id":"c","name":"","args":["{}"]}]),
            serde_json::json!([{"type":"chunk","time":10,"chunk":"not-an-object"}]),
            serde_json::json!([{"type":"future","time":10}]),
        ];
        for stream in cases {
            assert_eq!(
                validate_assistant_stream(&stream).is_ok(),
                expand_assistant_stream(&stream).is_ok(),
                "validation and expansion drifted for {stream}"
            );
        }
    }

    #[test]
    fn accumulator_compacts_adjacent_deltas_and_keeps_singletons() {
        let mut stream = AssistantStreamAccumulator::default();
        stream.push(
            10,
            serde_json::json!({"type":"text-delta","index":0,"text":"a"}),
        );
        stream.push(
            13,
            serde_json::json!({"type":"text-delta","index":0,"text":"b"}),
        );
        stream.push(
            14,
            serde_json::json!({"type":"finish","reason":{"kind":"stop"}}),
        );
        assert_eq!(
            stream.take_value(),
            serde_json::json!([
                {"type":"text-chunks","time0":10,"index":0,"dt":[3],"texts":["a","b"]},
                {"type":"chunk","time":14,"chunk":{"type":"finish","reason":{"kind":"stop"}}}
            ])
        );
    }
}
