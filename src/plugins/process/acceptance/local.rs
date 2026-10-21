use super::super::observation::rules;
use super::*;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextRule {
    path: String,
    contains: Vec<String>,
}
pub(super) struct TextChecker;
impl AcceptanceChecker for TextChecker {
    fn identity(&self) -> (&'static str, u32) {
        ("text", 1)
    }
    fn validate(&self, c: &Clause) -> Result<(), String> {
        let r: TextRule = parse(&c.parameters)?;
        files(c, &[&r.path])?;
        if r.contains.is_empty()
            || r.contains.len() > 16
            || r.contains.iter().any(|s| s.is_empty() || s.len() > 1024)
        {
            return Err("text needs 1..16 bounded nonempty requirements".into());
        }
        Ok(())
    }
    fn evaluate(
        &self,
        c: &Clause,
        _: &TaskState,
        s: &CheckSnapshot,
        _: Option<&str>,
    ) -> CheckerOutcome {
        let result = (|| {
            let r: TextRule = parse(&c.parameters)?;
            let text = std::str::from_utf8(s.bytes(&r.path)?).map_err(|_| "text is not UTF-8")?;
            Ok::<_, String>(r.contains.iter().all(|part| text.contains(part)))
        })();
        match result {
            Ok(true) => CheckerOutcome::new("passed", "required UTF-8 text present"),
            Ok(false) => CheckerOutcome::new("failed", "required text absent"),
            Err(e) => CheckerOutcome::new("inconclusive", e),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonRule {
    path: String,
    checks: Vec<rules::Check>,
}
pub(super) struct JsonChecker;
impl AcceptanceChecker for JsonChecker {
    fn identity(&self) -> (&'static str, u32) {
        ("json", 1)
    }
    fn validate(&self, c: &Clause) -> Result<(), String> {
        let r: JsonRule = parse(&c.parameters)?;
        files(c, &[&r.path])?;
        rules::validate_checks(&r.checks).map_err(|e| e.to_string())
    }
    fn evaluate(
        &self,
        c: &Clause,
        _: &TaskState,
        s: &CheckSnapshot,
        _: Option<&str>,
    ) -> CheckerOutcome {
        let result = (|| {
            let r: JsonRule = parse(&c.parameters)?;
            let bytes = s.bytes(&r.path)?;
            if bytes.len() > 256 * 1024 {
                return Err("JSON assertion input exceeds 256 KiB".into());
            }
            let value: Value =
                serde_json::from_slice(bytes).map_err(|_| "invalid or excessively nested JSON")?;
            let rows: Vec<_> = r
                .checks
                .iter()
                .map(|check| check.evaluate(&value))
                .collect();
            Ok::<_, String>(if rows.iter().any(|r| r["status"] == "failed") {
                "failed"
            } else if rows.iter().any(|r| r["status"] != "passed") {
                "inconclusive"
            } else {
                "passed"
            })
        })();
        match result {
            Ok(status) => CheckerOutcome::new(
                status,
                "explicit JSON observation assertions; provenance is observed file bytes",
            ),
            Err(e) => CheckerOutcome::new("inconclusive", e),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordsRule {
    input: String,
    table: String,
    report: String,
    value_field: String,
    total_label: String,
}
pub(super) struct RecordsChecker;
impl AcceptanceChecker for RecordsChecker {
    fn identity(&self) -> (&'static str, u32) {
        ("records", 1)
    }
    fn validate(&self, c: &Clause) -> Result<(), String> {
        let r: RecordsRule = parse(&c.parameters)?;
        files(c, &[&r.input, &r.table, &r.report])?;
        if r.value_field.is_empty()
            || r.value_field.len() > 128
            || r.total_label.is_empty()
            || r.total_label.len() > 128
            || r.total_label.contains(['\r', '\n'])
        {
            return Err("records needs bounded field and single-line total label".into());
        }
        Ok(())
    }
    fn evaluate(
        &self,
        c: &Clause,
        _: &TaskState,
        s: &CheckSnapshot,
        _: Option<&str>,
    ) -> CheckerOutcome {
        match records(c, s) {
            Ok(true) => CheckerOutcome::new(
                "passed",
                "source records preserved; independently computed row count/i64 sum and report total match",
            ),
            Ok(false) => CheckerOutcome::new(
                "failed",
                "rows, row_count, i64 sum or exact report total differ from declared inputs",
            ),
            Err(e) => CheckerOutcome::new("inconclusive", e),
        }
    }
}
fn records(c: &Clause, s: &CheckSnapshot) -> Result<bool, String> {
    let r: RecordsRule = parse(&c.parameters)?;
    for (path, limit) in [
        (&r.input, 1024 * 1024),
        (&r.table, 2 * 1024 * 1024),
        (&r.report, 256 * 1024),
    ] {
        if s.bytes(path)?.len() > limit {
            return Err("record artifact exceeds format byte limit".into());
        }
    }
    let input: Value =
        serde_json::from_slice(s.bytes(&r.input)?).map_err(|_| "invalid input JSON")?;
    let table: Value =
        serde_json::from_slice(s.bytes(&r.table)?).map_err(|_| "invalid table JSON")?;
    let rows = input
        .as_array()
        .ok_or("input must be a JSON record array")?;
    if rows.len() > 10000 {
        return Err("record count exceeds 10000".into());
    }
    let mut total = 0i64;
    for row in rows {
        s.bytes(&r.input)?; // Cooperative cancellation/deadline at each record.
        let n = row
            .get(&r.value_field)
            .and_then(Value::as_i64)
            .ok_or("record values must be exact i64 integers")?;
        total = total.checked_add(n).ok_or("i64 sum overflow")?;
    }
    let report = std::str::from_utf8(s.bytes(&r.report)?).map_err(|_| "report is not UTF-8")?;
    let expected = format!("{} {}", r.total_label, total);
    let totals: Vec<_> = report
        .lines()
        .filter(|line| line.trim().starts_with(&r.total_label))
        .collect();
    Ok(table.get("rows") == Some(&input)
        && table["row_count"].as_u64() == Some(rows.len() as u64)
        && table["total"].as_i64() == Some(total)
        && totals.len() == 1
        && totals[0].trim() == expected)
}
