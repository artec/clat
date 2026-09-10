//! Bounded form interaction, independent of MCP/WIT encoding.
use super::*;

/// 一个字段的应答结果：值、可选字段的跳过、或整个表单的中止
/// （declined：用户拒绝；cancelled：取消令牌已触发/Esc）。
pub(super) enum FieldAnswer {
    Value(Value),
    Skipped,
    Aborted { cancelled: bool },
}

/// 逐字段提问：数字解析失败重问（≤NUMBER_RETRIES 次）。
pub(super) fn ask_field(
    asker: &Arc<dyn UserAsker>,
    form: &ElicitForm,
    index: usize,
    field: &ElicitField,
    cancel: &CancelToken,
) -> Result<FieldAnswer, PluginHostError> {
    let mut attempts = 0usize;
    loop {
        let question = field_question(form, index, field, attempts);
        let (options, allow_custom) = field_options(field);
        match asker.ask(
            AskQuestion {
                question,
                options,
                allow_custom,
            },
            cancel,
        ) {
            // Declined 是 asker 端口的合并语义（拒绝/取消/断连）：以取
            // 消令牌区分 cancel 与 declined——已取消即 cancel，否则
            // 视为用户拒绝整个表单。
            AskAnswer::Declined => {
                return Ok(FieldAnswer::Aborted {
                    cancelled: cancel.is_cancelled(),
                });
            }
            AskAnswer::Custom(text) => {
                if matches!(field.kind, ElicitFieldKind::Number) {
                    match parse_number(&text) {
                        Some(value) => return Ok(FieldAnswer::Value(value)),
                        None => {
                            attempts += 1;
                            if attempts > NUMBER_RETRIES {
                                return Err(PluginHostError::InvalidAnswer(format!(
                                    "field `{}`: `{}` is not a number",
                                    field.name, text
                                )));
                            }
                            continue;
                        }
                    }
                }
                return Ok(FieldAnswer::Value(Value::String(text)));
            }
            AskAnswer::Selected(label) => {
                if label == SKIP_LABEL {
                    return Ok(FieldAnswer::Skipped);
                }
                return Ok(match field.kind {
                    ElicitFieldKind::Boolean => FieldAnswer::Value(Value::Bool(label == "yes")),
                    ElicitFieldKind::Choice(_)
                    | ElicitFieldKind::Text
                    | ElicitFieldKind::Number => FieldAnswer::Value(Value::String(label)),
                });
            }
        }
    }
}

/// 字段问题文案：首字段带表单总 message 作上下文；重问时附提示。
fn field_question(form: &ElicitForm, index: usize, field: &ElicitField, attempt: usize) -> String {
    let label = field.title.as_deref().unwrap_or(&field.name);
    let mut question = String::new();
    if index == 0 {
        let message = form.message.trim();
        if !message.is_empty() {
            question.push_str(message);
            question.push_str("\n\n");
        }
    }
    question.push_str("— ");
    question.push_str(label);
    if let Some(description) = &field.description {
        question.push_str(": ");
        question.push_str(description);
    }
    if !field.required {
        question.push_str(" (optional)");
    }
    if attempt > 0 {
        question.push_str(" [enter a number]");
    }
    question
}

/// 字段选项（yes/no、枚举值；可选枚举/布尔追加跳过项）。
fn field_options(field: &ElicitField) -> (Vec<AskOption>, bool) {
    let option = |label: &str| AskOption {
        label: label.to_owned(),
        description: None,
    };
    match &field.kind {
        ElicitFieldKind::Boolean => {
            let mut options = vec![option("yes"), option("no")];
            if !field.required {
                options.push(option(SKIP_LABEL));
            }
            (options, false)
        }
        ElicitFieldKind::Choice(values) => {
            let mut options: Vec<AskOption> = values.iter().map(|value| option(value)).collect();
            if !field.required {
                options.push(option(SKIP_LABEL));
            }
            (options, false)
        }
        ElicitFieldKind::Text | ElicitFieldKind::Number => (Vec::new(), true),
    }
}

/// 数字解析：先整后浮，产物保持 JSON number 形态。A4-5（W1-25）：
/// 非有限值（NaN/±inf）拒绝——serde_json 会把非有限 f64 序列化成
/// Null，跨 WIT/JSON 边都是类型违约。
fn parse_number(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(int) = trimmed.parse::<i64>() {
        return Some(json!(int));
    }
    trimmed
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
        .map(|number| json!(number))
}
