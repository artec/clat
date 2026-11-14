use super::*;

impl CallbackScope {
    pub(super) fn elicitation(&self, params: &Value) -> Reply {
        if !self.current() {
            return Err(-32800);
        }
        if params["mode"] != "form" || self.interaction.asker.is_none() {
            return Err(-32602);
        }
        let schema = &params["requestedSchema"];
        if schema["type"] != "object" {
            return Err(-32602);
        }
        let fields = schema["properties"]
            .as_object()
            .filter(|fields| fields.len() <= 16)
            .ok_or(-32602)?;
        let required = schema["required"].as_array().map_or(&[][..], Vec::as_slice);
        let message = params["message"]
            .as_str()
            .filter(|message| message.chars().count() <= 500)
            .ok_or(-32602)?;
        if required
            .iter()
            .any(|name| name.as_str().is_none_or(|name| !fields.contains_key(name)))
        {
            return Err(-32602);
        }
        loop {
            let collected = self.collect_fields(fields, required, message);
            if !self.current() {
                return Ok(json!({"action":"cancel"}));
            }
            let Some(values) = collected? else {
                return Ok(json!({"action":"decline"}));
            };
            match self.review_fields(message, &values)? {
                AskAnswer::Selected(value) if value == "send" && self.current() => {
                    return Ok(json!({"action":"accept","content":values}));
                }
                AskAnswer::Selected(value) if value == "edit" && self.current() => continue,
                _ => return Ok(json!({"action": if self.current() {"decline"} else {"cancel"}})),
            }
        }
    }

    fn collect_fields(
        &self,
        fields: &serde_json::Map<String, Value>,
        required: &[Value],
        message: &str,
    ) -> Result<Option<serde_json::Map<String, Value>>, i64> {
        let mut values = serde_json::Map::new();
        for (name, field) in fields {
            let label = field["title"].as_str().unwrap_or(name);
            let question = json!(format!("{message}\n{label}"));
            let optional = !required.iter().any(|value| value == name);
            let options = optional
                .then(|| AskOption {
                    label: "skip".into(),
                    description: None,
                })
                .into_iter()
                .collect();
            let labels = optional
                .then(|| crate::i18n::UiMessage::new("acp.form_skip"))
                .into_iter()
                .collect();
            match self.choose(&question, options, true, labels) {
                AskAnswer::Custom(answer) if answer.len() <= 1024 => {
                    values.insert(name.clone(), form_value(field, answer)?);
                }
                AskAnswer::Selected(value) if optional && value == "skip" => {}
                AskAnswer::Declined if self.current() => return Ok(None),
                _ => return Err(-32602),
            }
        }
        Ok(Some(values))
    }

    fn review_fields(
        &self,
        message: &str,
        values: &serde_json::Map<String, Value>,
    ) -> Result<AskAnswer, i64> {
        let serialized = serde_json::to_string_pretty(values).map_err(|_| -32602)?;
        let review = format!("{message}\n{serialized}");
        if review.chars().count() > 2000 || display_text(&review, 2000) != review {
            return Err(-32602);
        }
        let options = ["send", "edit"]
            .map(|label| AskOption {
                label: label.into(),
                description: None,
            })
            .to_vec();
        let labels = ["acp.form_review", "acp.form_edit"]
            .map(crate::i18n::UiMessage::new)
            .to_vec();
        Ok(self.choose(&json!(review), options, false, labels))
    }
}

fn form_value(field: &Value, raw: String) -> Result<Value, i64> {
    let kind = field["type"].as_str().ok_or(-32602)?;
    let value = if kind == "string" {
        json!(raw)
    } else {
        serde_json::from_str(&raw).map_err(|_| -32602)?
    };
    if !matches!(
        (kind, &value),
        ("string", Value::String(_))
            | ("boolean", Value::Bool(_))
            | ("number", Value::Number(_))
            | ("integer", Value::Number(_))
            | ("array", Value::Array(_))
    ) || (kind == "integer" && !value.is_i64() && !value.is_u64())
        || field["enum"]
            .as_array()
            .is_some_and(|options| !options.contains(&value))
        || field["oneOf"]
            .as_array()
            .is_some_and(|options| !options.iter().any(|option| option["const"] == value))
    {
        return Err(-32602);
    }
    let size = value
        .as_str()
        .map(|s| s.chars().count())
        .or_else(|| value.as_array().map(Vec::len));
    if let Some(size) = size.map(|size| size as u64) {
        let (min, max) = if value.is_string() {
            ("minLength", "maxLength")
        } else {
            ("minItems", "maxItems")
        };
        if field[min].as_u64().is_some_and(|min| size < min)
            || field[max].as_u64().is_some_and(|max| size > max)
        {
            return Err(-32602);
        }
    }
    if let Some(number) = value.as_f64()
        && (field["minimum"].as_f64().is_some_and(|min| number < min)
            || field["maximum"].as_f64().is_some_and(|max| number > max))
    {
        return Err(-32602);
    }
    if let Some(items) = value.as_array() {
        let item_schema = &field["items"];
        if items.iter().any(|item| {
            !item.is_string()
                || item_schema["enum"]
                    .as_array()
                    .is_some_and(|choices| !choices.contains(item))
                || item_schema["anyOf"]
                    .as_array()
                    .is_some_and(|choices| !choices.iter().any(|choice| choice["const"] == *item))
        }) {
            return Err(-32602);
        }
    }
    Ok(value)
}
