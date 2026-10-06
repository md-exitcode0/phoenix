pub(crate) fn normalize_todo_items(items: &[serde_json::Value]) -> serde_json::Value {
    serde_json::Value::Array(
        items
            .iter()
            .map(|item| match item {
                serde_json::Value::String(task) => serde_json::json!({
                    "task": task,
                    "completed": false,
                }),
                serde_json::Value::Object(object) => {
                    let task = object
                        .get("task")
                        .or_else(|| object.get("title"))
                        .or_else(|| object.get("name"))
                        .and_then(|value| value.as_str())
                        .unwrap_or("Untitled task");
                    let completed = object
                        .get("completed")
                        .and_then(|value| value.as_bool())
                        .or_else(|| {
                            object.get("status").and_then(|value| {
                                value.as_str().map(|status| {
                                    matches!(status, "done" | "complete" | "completed" | "finished")
                                })
                            })
                        })
                        .unwrap_or(false);
                    serde_json::json!({
                        "task": task,
                        "completed": completed,
                    })
                }
                _ => serde_json::json!({
                    "task": "Untitled task",
                    "completed": false,
                }),
            })
            .collect(),
    )
}

pub(crate) fn normalize_tool_input(tool_name: &str, input: serde_json::Value) -> serde_json::Value {
    match (tool_name, input) {
        ("read", serde_json::Value::String(path)) => serde_json::json!({ "path": path }),
        ("glob", serde_json::Value::String(pattern)) => serde_json::json!({ "pattern": pattern }),
        ("grep", serde_json::Value::String(pattern)) => serde_json::json!({ "pattern": pattern }),
        ("bash", serde_json::Value::String(command)) => serde_json::json!({ "command": command }),
        ("todo_write", serde_json::Value::Object(mut object)) => {
            if let Some(todos) = object.get("todos").and_then(|value| value.as_array()) {
                object.insert("todos".to_string(), normalize_todo_items(todos));
            } else if let Some(items) = object.remove("items") {
                let todos = items
                    .as_array()
                    .map(|items| normalize_todo_items(items))
                    .unwrap_or(items);
                object.insert("todos".to_string(), todos);
            }
            serde_json::Value::Object(object)
        }
        ("read", serde_json::Value::Object(mut object)) => {
            if object
                .get("path")
                .and_then(|value| value.as_str())
                .is_some()
            {
                serde_json::Value::Object(object)
            } else if let Some(path) = object
                .remove("file")
                .or_else(|| object.remove("file_path"))
                .or_else(|| object.remove("pathname"))
            {
                serde_json::json!({ "path": path })
            } else {
                serde_json::Value::Object(object)
            }
        }
        ("glob", serde_json::Value::Object(mut object)) => {
            if object
                .get("pattern")
                .and_then(|value| value.as_str())
                .is_some()
            {
                serde_json::Value::Object(object)
            } else if let Some(pattern) = object.remove("glob").or_else(|| object.remove("path")) {
                serde_json::json!({ "pattern": pattern })
            } else {
                serde_json::Value::Object(object)
            }
        }
        ("bash", serde_json::Value::Object(mut object)) => {
            if object
                .get("command")
                .and_then(|value| value.as_str())
                .is_some()
            {
                serde_json::Value::Object(object)
            } else if let Some(command) = object.remove("cmd") {
                serde_json::json!({ "command": command })
            } else {
                serde_json::Value::Object(object)
            }
        }
        (tool_name, serde_json::Value::Object(mut object)) if tool_name == "grep" => {
            if object
                .get("pattern")
                .and_then(|value| value.as_str())
                .is_none()
            {
                if let Some(pattern) = object.remove("query").or_else(|| object.remove("needle")) {
                    object.insert("pattern".to_string(), pattern);
                }
            }
            serde_json::Value::Object(object)
        }
        ("talk", serde_json::Value::String(body)) => serde_json::json!({
            "to": "coder",
            "subject": "Specialist task",
            "body": body,
            "mode": 2
        }),
        ("talk", serde_json::Value::Object(mut object)) => {
            // Prompts say "talk brief" — cheap models often emit `brief` instead of `body`.
            let brief_field = object.remove("brief");

            if object.get("to").and_then(|value| value.as_str()).is_none() {
                if let Some(to) = object
                    .remove("target")
                    .or_else(|| object.remove("agent"))
                    .or_else(|| object.remove("recipient"))
                {
                    object.insert("to".to_string(), to);
                }
            }
            if object.get("to").and_then(|value| value.as_str()).is_none() {
                object.insert("to".to_string(), serde_json::json!("coder"));
            }

            if object
                .get("subject")
                .and_then(|value| value.as_str())
                .is_none()
            {
                if let Some(subject) = object
                    .remove("title")
                    .or_else(|| object.remove("topic"))
                    .or_else(|| object.remove("task"))
                {
                    object.insert("subject".to_string(), subject);
                }
            }
            if object
                .get("subject")
                .and_then(|value| value.as_str())
                .is_none()
            {
                object.insert("subject".to_string(), serde_json::json!("Specialist task"));
            }

            if object
                .get("body")
                .and_then(|value| value.as_str())
                .is_none()
            {
                if let Some(body) = object
                    .remove("message")
                    .or_else(|| object.remove("content"))
                    .or_else(|| object.remove("prompt"))
                    .or_else(|| object.remove("request"))
                {
                    object.insert("body".to_string(), body);
                }
            }
            if object
                .get("body")
                .and_then(|value| value.as_str())
                .is_none()
            {
                if let Some(task) = object.get("task").and_then(|value| value.as_str()) {
                    object.insert("body".to_string(), serde_json::json!(task));
                }
            }

            let body_is_placeholder = |body: &str| {
                let trimmed = body.trim();
                trimmed.is_empty()
                    || trimmed.eq_ignore_ascii_case("specialist task")
                    || trimmed.chars().count() <= 24
            };
            if let Some(brief) = brief_field {
                let brief_text = brief.as_str().unwrap_or("").trim();
                let current_body = object
                    .get("body")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .trim();
                let adopt_brief = !brief_text.is_empty()
                    && (current_body.is_empty()
                        || body_is_placeholder(current_body)
                        || brief_text.chars().count() > current_body.chars().count() + 40);
                if adopt_brief {
                    object.insert("body".to_string(), brief);
                }
            }

            if object
                .get("body")
                .and_then(|value| value.as_str())
                .is_none()
            {
                let subject = object
                    .get("subject")
                    .and_then(|value| value.as_str())
                    .unwrap_or("Specialist task");
                object.insert("body".to_string(), serde_json::json!(subject));
            }

            if object
                .get("mode")
                .and_then(|value| value.as_u64())
                .is_none()
            {
                // Team model (2026-07-02): mode 2 — background handoff through
                // the mesh — is the DEFAULT for every delegation. The specialist
                // runs in the gateway wave and hands its result back, so the
                // orchestrator keeps moving and the team passes work directly
                // (the Work Web: orchestrator → planner → coder → … → back).
                // Mode 1 (synchronous: caller blocks for the reply this turn)
                // remains available when the model emits it explicitly for the
                // rare quick-blocking case. Drop legacy `reply_expected` /
                // `wait_for_reply` aliases so they don't linger as unknown
                // fields; `mode` is the single knob (`reply_expected()` is now
                // derived from it: mode 1 = reply expected, mode 2 = background).
                let _ = object
                    .remove("reply_expected")
                    .or_else(|| object.remove("wait_for_reply"));
                object.insert("mode".to_string(), serde_json::json!(2));
            }

            serde_json::Value::Object(object)
        }
        (_, value) => value,
    }
}
