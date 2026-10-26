use serde_json::Value;

/// First string value found under any of `keys`.
pub fn str_field(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| v.get(k).and_then(Value::as_str))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn u64_field(v: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|k| v.get(k).and_then(Value::as_u64))
}

/// A one-line description of a tool call, from the fields vendors put in `tool_input`.
pub fn tool_detail(tool: &str, input: Option<&Value>) -> Option<String> {
    let input = input?;
    if tool == "AskUserQuestion" {
        let q = input
            .get("questions")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|q| q.get("question"))
            .and_then(Value::as_str);
        return q
            .map(|s| s.to_string())
            .or_else(|| str_field(input, &["question", "prompt"]));
    }
    str_field(
        input,
        &[
            "command",
            "description",
            "file_path",
            "path",
            "pattern",
            "url",
            "prompt",
            "query",
        ],
    )
    .or_else(|| {
        // Fall back to the first short string value, so MCP tools still show something.
        input.as_object().and_then(|o| {
            o.values()
                .find_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(120).collect())
        })
    })
}

/// "Bash: npm test" style detail used for waiting_detail.
pub fn tool_line(tool: &str, input: Option<&Value>) -> Option<String> {
    Some(match tool_detail(tool, input) {
        Some(d) => format!("{tool}: {d}"),
        None => tool.to_string(),
    })
}
