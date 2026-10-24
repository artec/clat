//! Native schemas describe every supported shape; implementation still validates.
use serde_json::{Value, json};
pub(super) fn time() -> Value {
    let int = |min: i64, max: i64| json!({"type":"integer","minimum":min,"maximum":max});
    let zone = json!({"type":"string","maxLength":128,"description":"Explicit IANA time zone, such as Asia/Shanghai or America/New_York"});
    let object = |properties: Value| required_object(properties);
    json!({"anyOf":[
        object(json!({"kind":{"type":"string","enum":["delay"]},"delay_ms":int(0,31536000000)})),
        object(json!({"kind":{"type":"string","enum":["at"]},"at_ms":int(-9007199254740991,9007199254740991)})),
        object(json!({"kind":{"type":"string","enum":["interval"]},"anchor_ms":int(-9007199254740991,9007199254740991),"every_ms":int(1000,31622400000)})),
        object(json!({"kind":{"type":"string","enum":["daily"]},"hour":int(0,23),"minute":int(0,59),"timezone":zone})),
        object(json!({"kind":{"type":"string","enum":["weekly"]},"weekday":int(1,7),"hour":int(0,23),"minute":int(0,59),"timezone":zone}))
    ]})
}
pub(super) fn budget() -> Value {
    let int = |min: u64, max: u64| json!({"type":"integer","minimum":min,"maximum":max});
    required_object(
        json!({"max_runs":int(1,10000),"max_tokens":int(1,9007199254740991),
        "max_seconds":int(1,9007199254740991),"run_tokens":int(1,9007199254740991),"run_seconds":int(1,86400),
        "max_paid_web_calls":int(0,u32::MAX.into()),"run_paid_web_calls":int(0,u32::MAX.into())}),
    )
}
pub(super) fn draft() -> Value {
    required_object(
        json!({"instruction":{"type":"string","minLength":1,"maxLength":16384},
        "time":time(),"budget":budget(),"model_profile":{"type":["string","null"],"maxLength":256}}),
    )
}
fn required_object(properties: Value) -> Value {
    let required: Vec<_> = properties.as_object().unwrap().keys().collect();
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
pub(super) fn parse_time(value: Value) -> Result<super::TimeRule, super::ScheduleError> {
    if value["kind"] == "delay" {
        let delay = value["delay_ms"]
            .as_u64()
            .filter(|n| *n <= 31536000000)
            .ok_or_else(|| super::ScheduleError::new("invalid_time"))?;
        if value.as_object().is_none_or(|o| o.len() != 2) {
            return Err(super::ScheduleError::new("invalid_time"));
        }
        return Ok(super::TimeRule::At {
            at_ms: super::now_ms()
                .checked_add(delay as i64)
                .ok_or_else(|| super::ScheduleError::new("invalid_time"))?,
        });
    }
    serde_json::from_value(value).map_err(|_| super::ScheduleError::new("invalid_time"))
}
