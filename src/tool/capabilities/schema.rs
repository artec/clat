//! One public-schema boundary for registry/model/discovery. Annotation cleanup
//! must never rewrite validation data (including objects inside enum/const).
use super::*;

pub(crate) fn public_contract(tool: &dyn Tool) -> Result<ToolDefinition, ToolError> {
    definition(
        tool.public_definition()?,
        &crate::redact::redact_secrets,
        &marked_secret,
    )
}

pub(crate) fn definition(
    mut definition: ToolDefinition,
    redact: &impl Fn(&str) -> String,
    private: &impl Fn(&str) -> bool,
) -> Result<ToolDefinition, ToolError> {
    let mut budget = 65_536;
    schema(
        &mut definition.input_schema,
        redact,
        private,
        0,
        &mut budget,
    )?;
    Ok(definition)
}

pub(crate) fn marked_secret(text: &str) -> bool {
    crate::redact::redact_secrets(text) != text
}

fn unavailable() -> ToolError {
    ToolError::new("tool schema cannot be safely published; registration unavailable")
}

fn consume(depth: usize, budget: &mut usize) -> Result<(), ToolError> {
    if depth > 128 || *budget == 0 {
        return Err(unavailable());
    }
    *budget -= 1;
    Ok(())
}

fn schema(
    value: &mut Value,
    redact: &impl Fn(&str) -> String,
    private: &impl Fn(&str) -> bool,
    depth: usize,
    budget: &mut usize,
) -> Result<(), ToolError> {
    consume(depth, budget)?;
    let Value::Object(fields) = value else {
        return opaque(value, private, depth, budget);
    };
    for (key, value) in fields {
        if private(key) || (crate::redact::credential_name(key) && value.is_string()) {
            return Err(unavailable());
        }
        match key.as_str() {
            "title" | "description" | "$comment" | "markdownDescription" if value.is_string() => {
                *value = Value::String(redact(value.as_str().expect("annotation string")));
            }
            "$defs" | "definitions" | "properties" | "patternProperties" | "dependentSchemas" => {
                schema_map(value, redact, private, depth + 1, budget)?;
            }
            "allOf" | "anyOf" | "oneOf" | "prefixItems" => {
                schemas(value, redact, private, depth + 1, budget)?;
            }
            "items" if value.is_array() => schemas(value, redact, private, depth + 1, budget)?,
            "items"
            | "additionalItems"
            | "additionalProperties"
            | "contains"
            | "not"
            | "if"
            | "then"
            | "else"
            | "propertyNames"
            | "unevaluatedItems"
            | "unevaluatedProperties"
            | "contentSchema" => schema(value, redact, private, depth + 1, budget)?,
            // This legacy keyword contains either property-name arrays or schemas.
            "dependencies" => schema_map(value, redact, private, depth + 1, budget)?,
            _ => opaque(value, private, depth + 1, budget)?,
        }
    }
    Ok(())
}

fn schema_map(
    value: &mut Value,
    redact: &impl Fn(&str) -> String,
    private: &impl Fn(&str) -> bool,
    depth: usize,
    budget: &mut usize,
) -> Result<(), ToolError> {
    let Value::Object(fields) = value else {
        return opaque(value, private, depth, budget);
    };
    for (name, value) in fields {
        if private(name) {
            return Err(unavailable());
        }
        schema(value, redact, private, depth, budget)?;
    }
    Ok(())
}

fn schemas(
    value: &mut Value,
    redact: &impl Fn(&str) -> String,
    private: &impl Fn(&str) -> bool,
    depth: usize,
    budget: &mut usize,
) -> Result<(), ToolError> {
    let Value::Array(values) = value else {
        return opaque(value, private, depth, budget);
    };
    for value in values {
        schema(value, redact, private, depth, budget)?;
    }
    Ok(())
}

fn opaque(
    value: &Value,
    private: &impl Fn(&str) -> bool,
    depth: usize,
    budget: &mut usize,
) -> Result<(), ToolError> {
    consume(depth, budget)?;
    match value {
        Value::String(text) if private(text) => return Err(unavailable()),
        Value::Array(values) => {
            for value in values {
                opaque(value, private, depth + 1, budget)?;
            }
        }
        Value::Object(fields) => {
            for (key, value) in fields {
                if private(key)
                    || (crate::redact::credential_name(key)
                        && value.as_str().is_some_and(|s| !s.is_empty()))
                {
                    return Err(unavailable());
                }
                opaque(value, private, depth + 1, budget)?;
            }
        }
        _ => {}
    }
    Ok(())
}
