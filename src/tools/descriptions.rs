//! Rich tool descriptions for provider native tool-calling (Cursor-style When/When NOT).

use crate::providers::contracts::ToolDefinition;

pub fn tool_definition(name: &str) -> Option<ToolDefinition> {
    Some(match name {
        "read" => read(),
        "write" => write(),
        "str_replace" => str_replace(),
        "grep" => grep(),
        "glob" => glob(),
        "codebase_search" => define_codebase_search(),
        "index_codebase" => index_codebase(),
        "symbol_search" => symbol_search(),
        "callers" => callers(),
        "callees" => callees(),
        "impact" => impact(),
        "file_symbols" => file_symbols(),
        "call_path" => call_path(),
        "list_directory" => list_directory(),
        "bash" => bash(),
        "background_terminal" => background_terminal(),
        "terminal_job" => terminal_job(),
        "remote_runner" => remote_runner(),
        "agent_control" => agent_control(),
        "message_agent" => message_agent(),
        "talk" => talk(),
        "volume_work" => volume_work(),
        "routine" => routine(),
        "work" => work(),
        "web_search" => web_search(),
        "web_fetch" => web_fetch(),
        "transcribe_audio" => transcribe_audio(),
        "web_scrape" => web_scrape(),
        "web_crawl" => web_crawl(),
        "ask_user" => ask_user(),
        "user_update" => user_update(),
        "react" => react(),
        "teach_workflow" => teach_workflow(),
        "todo_write" => todo_write(),
        "vital_memory_write" => vital_memory_write(),
        "create_agent" => create_agent(),
        "agent_provision" => agent_provision(),
        "tools_create" => tools_create(),
        "reverse_skill" => reverse_skill(),
        "recall" => recall(),
        "memory_recall" => memory_recall(),
        "memory_save" => memory_save(),
        "cron" => cron(),
        "credential_list" => credential_list(),
        "credential_generate" => credential_generate(),
        "account_manage" => account_manage(),
        "ask_for_login" => ask_for_login(),
        "ask_for_pass" => ask_for_pass(),
        "pass_use" => pass_use(),
        "design_reference" => design_reference(),
        "design_studio" => design_studio(),
        "design_website" => design_website(),
        "image_gen" => image_gen(),
        "image_analyze" => image_analyze(),
        "motion_graphics" => motion_graphics(),
        "skill" => skill(),
        "skill_install" => skill_install(),
        "skill_search" => skill_search(),
        "composio_search" => composio_search(),
        "composio_schemas" => composio_schemas(),
        "composio_run" => composio_run(),
        "composio_connections" => composio_connections(),
        "mcp_servers" => mcp_servers(),
        "mcp_call" => mcp_call(),
        "final_answer" => final_answer(),
        other => return computer_tool_definition(other).or_else(|| browser_tool_definition(other)),
    })
}

fn credential_list() -> ToolDefinition {
    ToolDefinition {
        name: "credential_list".into(),
        description: "List saved PASSES (logins, payment cards, API keys, tokens, codes, secrets) visible in this conversation (private coworker, current group, and company scopes) as METADATA only: id, kind, title, site, username, and public hints such as a card's brand and last 4. Secret values are never returned. Works while Passes is locked. Use it before pass_use / browser_input_credential; filter by site or kind. If nothing matches, ask the user with ask_for_pass. Never ask for the master password or a secret in prose.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "site": {"type": "string", "description": "Optional site/domain filter (subdomains match)."},
                "kind": {"type": "string", "enum": ["password", "card", "api_key", "token", "verification_code", "secret", "recovery_code"], "description": "Optional pass type filter. Logins are kind `password`."},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}
            }
        }),
    }
}

fn ask_for_pass() -> ToolDefinition {
    ToolDefinition {
        name: "ask_for_pass".into(),
        description: "Ask the user for a credential through a secure, purpose-built inline popup in the conversation, and wait for it. Pick the kind that fits: `login` (site, username/email, password, optional 2FA/TOTP seed — e.g. \"save your Gmail login\"), `card` (number with brand detection, name, expiry, CVC, billing zip), `api_key` (service, key, optional base URL), `token` (another key/token), `verification_code` (a one-time code the user received; used once then deleted), or `secret` (free-form). The user's entry is encrypted straight into Passes — you receive ONLY the new pass's credential_id and public metadata, never the value. Then use it with pass_use. Call ONCE per missing credential; first check credential_list (an existing matching pass is returned automatically unless force_new). Never ask for secrets in chat text and never call ask_user for this.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "kind": {"type": "string", "enum": ["login", "card", "api_key", "token", "verification_code", "secret"]},
                "reason": {"type": "string", "description": "The concrete outcome it unblocks, phrased to finish 'needs your … to …' (e.g. 'send the weekly update from your inbox')."},
                "title": {"type": "string", "description": "Short popup title, e.g. 'Gmail login', 'OpenAI API key', 'Card for the domain renewal'."},
                "site": {"type": "string", "description": "Website or API domain the pass belongs to (gmail.com, api.openai.com). Required for login and api_key. Secrets are only ever filled/sent on this site."},
                "fields": {"type": "array", "items": {"type": "string", "enum": ["site", "username", "password", "totp", "number", "name", "expiry", "cvc", "billing_zip", "service", "key", "base_url", "value", "code"]}, "description": "Which fields to show (defaults by kind). The primary secret is always included."},
                "labels": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Optional custom field labels, e.g. {\"key\": \"Secret key (starts with sk-)\"}."},
                "username_hint": {"type": "string", "description": "Prefill for the username/email when known."},
                "scope": {"type": "string", "enum": ["agent", "group", "company"], "default": "agent", "description": "Who may use the saved pass."},
                "force_new": {"type": "boolean", "default": false, "description": "Ask even if a matching pass exists (e.g. the site rejected it)."}
            },
            "required": ["kind", "reason"]
        }),
    }
}

fn pass_use() -> ToolDefinition {
    ToolDefinition {
        name: "pass_use".into(),
        description: "Use a saved pass by credential_id WITHOUT seeing it. target `browser_field`: Phoenix types the chosen field into the current browser element [index] (same site binding as browser_input_credential). target `http_header`: Phoenix calls an https URL on the pass's own site (or its saved base URL) with the secret in one header (default `Authorization: Bearer {secret}`) and returns the response with the secret scrubbed. `field` picks password (default for logins), username, totp (the current 6-digit 2FA code), number/name/expiry/exp_month/exp_year/cvc/billing_zip (cards), key (API keys), value, or code. If Passes is locked, Phoenix shows the user a one-time unlock card and waits; never ask for the master password yourself.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "credential_id": {"type": "string"},
                "target": {"type": "string", "enum": ["browser_field", "http_header"], "default": "browser_field"},
                "field": {"type": "string"},
                "index": {"type": "integer", "description": "browser_field: element index from the current browser state."},
                "url": {"type": "string", "description": "http_header: full https URL on the pass's site."},
                "method": {"type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE"], "default": "GET"},
                "header": {"type": "string", "default": "Authorization"},
                "format": {"type": "string", "default": "Bearer {secret}", "description": "Header value template; {secret} is substituted by the runtime."},
                "body": {"type": "string", "description": "Optional request body (no secrets; they are never substituted into bodies)."},
                "content_type": {"type": "string"}
            },
            "required": ["credential_id"]
        }),
    }
}

fn credential_generate() -> ToolDefinition {
    ToolDefinition {
        name: "credential_generate".into(),
        description: "Generate a cryptographically strong password and store it directly in Phoenix's encrypted vault. The password NEVER appears in model context or tool output. Use when creating an account; then fill the password field with browser_input_credential using the returned credential id. Default scope is private to you; group is valid only in that active group; company is intentionally shared.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "site": {"type": "string"},
                "label": {"type": "string"},
                "username": {"type": "string"},
                "scope": {"type": "string", "enum": ["agent", "group", "company"], "default": "agent"},
                "length": {"type": "integer", "minimum": 16, "maximum": 128, "default": 24}
            },
            "required": ["site", "label"]
        }),
    }
}

fn account_manage() -> ToolDefinition {
    ToolDefinition {
        name: "account_manage".into(),
        description: "Track an account-creation lifecycle without storing passwords or verification codes. Use begin before creating an account, set_status awaiting_verification when a code/user step is needed, and mark ready ONLY after verifying a signed-in page. Passwords belong in credential_generate/browser_input_credential. Coordinate email verification with a coworker through talk; never place the code in note. Free plans are the default. A paid plan always needs fresh explicit approval.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "begin", "set_status"]},
                "site": {"type": "string"},
                "purpose": {"type": "string"},
                "username": {"type": "string"},
                "credential_id": {"type": "string"},
                "scope": {"type": "string", "enum": ["agent", "group", "company"], "default": "agent"},
                "approval_id": {"type": "string", "description": "One-use receipt returned by ask_for_login when account creation policy is Ask."},
                "account_id": {"type": "string"},
                "status": {"type": "string", "enum": ["creating", "awaiting_verification", "ready", "blocked", "archived"]},
                "verification_kind": {"type": "string", "description": "Required only for awaiting_verification: email, sms, totp, or user_action."},
                "note": {"type": "string", "description": "Operational status only. Never include passwords, recovery material, or verification codes."},
                "include_archived": {"type": "boolean", "default": false},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50}
            },
            "required": ["action"]
        }),
    }
}

fn ask_for_login() -> ToolDefinition {
    ToolDefinition {
        name: "ask_for_login".into(),
        description: "Resolve a genuine site login in the coworker's private managed browser. Call ONCE per blocker. Phoenix automatically tries portable cookies from the actively used local browser first, then returns autonomous FREE-account creation when allowed; it shows the user a compact question only for device-bound/user-only login or a configured approval. The embedded route stays on the site's real origin and supports the browser/OS's genuine password, passkey, security-key, OAuth, and 2FA ceremonies with explicit user presence; Phoenix never invents a WebAuthn assertion or captures the entered secret. Paid plans always pass through the separate fresh purchase approval gate. A completed user login persists the site's browser state in that profile but does NOT retroactively capture the typed password or passkey; never call ask_for_login again merely to save credentials. Follow the returned decision exactly; never drive Zen/Firefox/Chrome with computer_* or repeat a completed/failed route.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "site": {"type": "string", "description": "The site/domain requiring access."},
                "reason": {"type": "string", "description": "Concrete task outcome blocked by login."},
                "username_hint": {"type": "string", "description": "Known account email/username, if any."},
                "methods": {"type": "array", "items": {"type": "string", "enum": ["import_cookies", "user_login", "create_account"]}, "default": ["import_cookies", "user_login", "create_account"]},
                "scope": {"type": "string", "enum": ["agent", "group", "company"], "default": "agent"}
            },
            "required": ["site", "reason"]
        }),
    }
}

fn computer_action_schema() -> serde_json::Value {
    use serde_json::json;
    let variants = [
        ("move", vec!["x", "y", "duration_ms"], vec!["x", "y"]),
        ("click", vec!["x", "y", "button", "double"], vec![]),
        ("double_click", vec!["x", "y", "button"], vec![]),
        ("click_element", vec!["app", "query", "button", "double"], vec!["app", "query"]),
        ("type_into", vec!["app", "query", "text"], vec!["app", "query", "text"]),
        ("type", vec!["text"], vec!["text"]),
        ("key", vec!["combo"], vec!["combo"]),
        ("scroll", vec!["dx", "dy"], vec![]),
        ("drag", vec!["from_x", "from_y", "to_x", "to_y", "duration_ms"], vec!["from_x", "from_y", "to_x", "to_y"]),
        ("wait", vec!["ms"], vec!["ms"]),
    ];
    json!({"anyOf": variants.into_iter().map(|(kind, fields, required)| {
        let mut properties = serde_json::Map::new();
        properties.insert("type".into(), json!({"type":"string", "enum":[kind]}));
        for field in fields {
            let schema = match field {
                "button" => json!({"type":"string", "enum":["left","right","middle"]}),
                "double" => json!({"type":"boolean"}),
                "app" | "query" | "text" | "combo" => json!({"type":"string"}),
                _ => json!({"type":"integer"}),
            };
            properties.insert(field.into(), schema);
        }
        let required = std::iter::once("type").chain(required).collect::<Vec<_>>();
        json!({"type":"object", "properties":properties, "required":required, "additionalProperties":false})
    }).collect::<Vec<_>>()})
}

fn computer_window_action_schema() -> serde_json::Value {
    use serde_json::json;
    let mut schema = computer_action_schema();
    let variants = schema["anyOf"].as_array_mut().unwrap();
    variants.retain(|variant| matches!(variant["properties"]["type"]["enum"][0].as_str(),
        Some("move" | "click" | "double_click" | "type" | "key" | "scroll" | "wait" | "drag")));
    for variant in variants {
        match variant["properties"]["type"]["enum"][0].as_str().unwrap() {
            "move" | "click" | "double_click" | "scroll" => {
                variant["properties"]["x"] = json!({"type":"integer","minimum":0,"maximum":2147483647});
                variant["properties"]["y"] = json!({"type":"integer","minimum":0,"maximum":2147483647});
                variant["required"] = json!(["type","x","y"]);
                if variant["properties"]["type"]["enum"][0] == "move" {
                    variant["properties"].as_object_mut().unwrap().remove("duration_ms");
                }
                if variant["properties"]["type"]["enum"][0] == "scroll" {
                    for axis in ["dx","dy"] {
                        variant["properties"][axis] = json!({"type":"integer","minimum":-30,"maximum":30});
                    }
                    variant["anyOf"] = json!([{"required":["dx"]},{"required":["dy"]}]);
                }
            }
            "drag" => {
                for field in ["from_x", "from_y", "to_x", "to_y"] {
                    variant["properties"][field] = json!({"type":"integer","minimum":0,"maximum":2147483647});
                }
                variant["properties"]["duration_ms"] = json!({"type":"integer","minimum":0,"maximum":10000});
            }
            "wait" => variant["properties"]["ms"] = json!({"type":"integer","minimum":0,"maximum":10000}),
            _ => {}
        }
    }
    schema
}

fn computer_tool_definition(name: &str) -> Option<ToolDefinition> {
    let (description, parameters) = match name {
        "computer_status" => (
            "Diagnose desktop-control health when an operation fails or the backend state is uncertain. Reports bridge reachability and screen size. For ordinary work, open the requested native app or inspect its window directly; a separate health check is not required.",
            serde_json::json!({"type": "object", "properties": {}}),
        ),
        "computer_screenshot" => (
            "Capture the whole desktop to a PNG; a vision caption of what's on screen rides along in the result. When: before deciding any click/type (you act on what you SEE, not what you assume), and after an action to verify it worked. When NOT: between every micro-step when nothing changed.",
            serde_json::json!({"type": "object", "properties": {}}),
        ),
        "computer_read_text" => (
            "OCR the screen: transcribe ALL visible text verbatim, grouped by region (title bar, menus, body, dialogs, status bar). When: you need EXACT strings — error-dialog wording, a code, a file name, table/cell values — that a description would blur. When NOT: locating a clickable element (use computer_locate); a general 'what's on screen' (the computer_screenshot caption covers that).",
            serde_json::json!({"type": "object", "properties": {}}),
        ),
        "computer_move" => (
            "Glide the agent's overlay cursor to (x, y) with smooth easing — the user watches it travel; input lands where it arrives. When: positioning before a click at a target you identified from a screenshot. When NOT: guessing coordinates you never saw.",
            serde_json::json!({"type": "object", "properties": {
                "x": {"type": "integer"}, "y": {"type": "integer"},
                "duration_ms": {"type": "integer", "description": "0 = auto (distance-scaled)"}
            }, "required": ["x", "y"]}),
        ),
        "computer_click" => (
            "Click (left/right/middle, optional double) at (x, y), or at the current agent-cursor position when x/y omitted. The user's own pointer is saved and restored. When: activating a target you verified on the latest screenshot. When NOT: blind coordinates, or anything irreversible without orchestrator-level approval.",
            serde_json::json!({"type": "object", "properties": {
                "x": {"type": "integer"}, "y": {"type": "integer"},
                "button": {"type": "string", "enum": ["left", "right", "middle"]},
                "double": {"type": "boolean"}
            }}),
        ),
        "computer_drag" => (
            "Press-drag-release from (from_x, from_y) to (to_x, to_y). When: moving items, selecting text/regions, sliders. When NOT: plain clicks.",
            serde_json::json!({"type": "object", "properties": {
                "from_x": {"type": "integer"}, "from_y": {"type": "integer"},
                "to_x": {"type": "integer"}, "to_y": {"type": "integer"},
                "duration_ms": {"type": "integer"}
            }, "required": ["from_x", "from_y", "to_x", "to_y"]}),
        ),
        "computer_scroll" => (
            "Scroll at the agent-cursor position; positive dy = down, positive dx = right (units are wheel steps, capped at 30). When: content is off-screen. When NOT: navigating when a key (Home/End/PageDown) would be cleaner — prefer computer_key then.",
            serde_json::json!({"type": "object", "properties": {
                "dx": {"type": "integer"}, "dy": {"type": "integer"}
            }}),
        ),
        "computer_type" => (
            "Type literal text into the currently focused control (max 2000 chars; \\n presses Enter). When: a field you just clicked has focus. When NOT: keyboard shortcuts (use computer_key), or typing into a window you never focused.",
            serde_json::json!({"type": "object", "properties": {
                "text": {"type": "string"}
            }, "required": ["text"]}),
        ),
        "computer_key" => (
            "Send one key or combo: \"enter\", \"tab\", \"esc\", \"ctrl+l\", \"ctrl+shift+t\", \"alt+tab\", \"super\", arrows, f1-f12. When: shortcuts, dialogs, navigation. When NOT: literal text (use computer_type).",
            serde_json::json!({"type": "object", "properties": {
                "combo": {"type": "string"}
            }, "required": ["combo"]}),
        ),
        "computer_act" => (
            "Run a SEQUENCE of native-app UI actions in ONE call, then screenshot once (far fewer round-trips than one action per turn). Each action carries ONLY its own fields (never blank/zero filler): move/click/double_click {x,y[,button]}; click_element {app,query[,button][,double]} and type_into {app,query,text} locate by VISIBLE TEXT at execution time (a11y-grounded — no coordinate reading, no separate locate call); type {text}; key {combo:\"enter\"}; scroll {dx,dy}; drag {from_x,from_y,to_x,to_y}; wait {ms}. e.g. actions:[{\"type\":\"type_into\",\"app\":\"LibreOffice\",\"query\":\"File name\",\"text\":\"report.pdf\"},{\"type\":\"click_element\",\"app\":\"LibreOffice\",\"query\":\"Export\"}]. Browser apps are rejected; website work belongs in the coworker's managed browser_* profile. Runs in order, stops at the first failure and reports which step (element misses include the closest visible texts). The end-of-batch screenshot also costs a vision-model pass — set screenshot:false whenever you already know your next batch and don't need to look (mid-flow typing/navigation); leave it on for the LAST batch or before any decision. When: a known multi-step run on the SAME native-app screen — prefer element-grounded steps over raw coordinates wherever the target has visible text. When NOT: a step whose target depends on the previous result (screenshot/locate between those); the first action on an unfamiliar screen (computer_app_read or screenshot first); any daily browser.",
            serde_json::json!({"type": "object", "properties": {
                "actions": {"type": "array", "minItems": 1, "maxItems": 25,
                    "description": "Ordered actions; include only fields for the chosen action type.",
                    "items": computer_action_schema()},
                "screenshot": {"type": "boolean", "description": "Screenshot once after the batch (default true)."}
            }, "required": ["actions"]}),
        ),
        "computer_list_windows" => (
            "List every open application window — id, title, app class, focused/minimized, geometry. When: a task spans more than one window, or you need to act on a window that isn't in front (find its id, then computer_focus_window). When NOT: single-window tasks where the target is already focused.",
            serde_json::json!({"type": "object", "properties": {}}),
        ),
        "computer_focus_window" => (
            "Bring a specific window to the front by its computer_list_windows `id` (un-minimizes, switches workspace, raises + focuses it) — the deterministic alternative to alt-tab guessing. After it, computer_act/type lands in THAT window. When: the window you must act on is behind others or on another workspace. When NOT: the target window already has focus.",
            serde_json::json!({"type": "object", "properties": {
                "id": {"type": "integer", "description": "Window id from computer_list_windows"}
            }, "required": ["id"]}),
        ),
        "computer_capture_window" => (
            "Capture ONE window's live content by its computer_list_windows `id`. It raises and focuses that window first (waiting for a pause in the user's typing), then shoots it through the compositor's own screenshot path. The image is window-aligned: coordinates read off it are WINDOW-RELATIVE and feed computer_window_act directly. A window still starting up returns a retryable error — wait ~1s and try again, never spam retries. This is the layered-mode eye: prefer it over computer_screenshot whenever the task lives in one window. When NOT: OS-level surfaces spanning windows (panel, overview) — full computer_screenshot then.",
            serde_json::json!({"type": "object", "properties": {
                "id": {"type": "integer", "description": "Window id from computer_list_windows"}
            }, "required": ["id"]}),
        ),
        "computer_window_act" => (
            "Execute 1–25 ordered actions in one GNOME window. Use its id from computer_list_windows and window-relative coordinates from computer_capture_window; screen coordinates belong in computer_act. Use move {x,y} before editor shortcuts to position the pointer without changing selection. Moving the pointer does not commit a text edit or release keyboard capture. Settle the observed field or dialog deliberately; never insert Enter or Escape blindly. End the batch at uncertain creation, selection or mode changes, inspect the captured result, then act on the verified target. Delivered input is not proof that the intended operation happened. Use click {x,y[,button]} or double_click {x,y[,button]}; click with double:true remains supported. Include only fields for each action type. The bridge restores prior focus, stacking and pointer state afterward. Captures the window after the batch by default, attaching pixels directly when native vision is enabled. Keep capture:true before decisions; capture:false is useful only when the next actions are already known. Unavailable on the X11 fallback backend.",
            serde_json::json!({"type": "object", "properties": {
                "id": {"type": "integer", "description": "Window id from computer_list_windows"},
                "actions": {"type": "array", "minItems":1,"maxItems":25, "description": "Ordered window-relative actions; include only the fields for each action's type.", "items": computer_window_action_schema()},
                "capture": {"type": "boolean", "description": "Capture the window after the batch (default true)."}
            }, "required": ["id", "actions"]}),
        ),
        "computer_lower_window" => (
            "Push a window below all others — park the agent's workspace UNDER the user's windows so it never blocks their work. capture/window_act keep working on it while covered; the user reveals it by moving or closing what's on top. When: right after opening the agent's work app in layered mode, or whenever the agent's window ended up on top of the user. When NOT: a window the user asked to SEE (computer_focus_window).",
            serde_json::json!({"type": "object", "properties": {
                "id": {"type": "integer", "description": "Window id from computer_list_windows"}
            }, "required": ["id"]}),
        ),
        "computer_app_targets" => (
            "List running apps reachable through accessibility (AT-SPI). Use the result only for native desktop apps; every app/read/act entrypoint rejects Zen, Firefox, Chrome, Chromium, Brave, Edge, and other daily browsers. Website work belongs in the coworker's private managed browser. When: a task needs an editor, office app, or another open native app. When NOT: launching fresh apps (computer_open), pixel-only work, or any website.",
            serde_json::json!({"type": "object", "properties": {}}),
        ),
        "computer_app_inspect" => (
            "PERCEPTION: read a running app's UI tree via AT-SPI — buttons, tabs, links, text fields AND static text (headings, counts, status lines) with role, name, text, and on-screen `bounds` (cx,cy = the point to click). Accessibility SEES; the mouse ACTS: read here, then computer_click at (cx,cy). When: you need the STRUCTURE (what's clickable, where). For 'what does the screen say' use computer_app_read; for a checklist use computer_app_locate with `queries`. When NOT: apps missing from computer_app_targets; fall back to screenshot + vision computer_locate.",
            serde_json::json!({"type": "object", "properties": {
                "app": {"type": "string", "description": "Native app name from computer_app_targets, e.g. \"LibreOffice\". Browser apps are rejected."},
                "max": {"type": "integer", "description": "Max elements to return (default 120)"}
            }, "required": ["app"]}),
        ),
        "computer_app_locate" => (
            "Find elements by their visible text and return (cx,cy) for the MOUSE — the accessibility-grounded twin of vision computer_locate (exact and layout-independent, no pixel guessing). Matches the accessible NAME and the TEXT CONTENT (static text like \"5 games\" counts). Pass `queries` (a list) to check/locate MANY things in ONE call — a verification checklist is one call, NEVER one call per string. A miss returns the closest visible texts with click points, so a differently-worded element is found on the next call, not guessed at. Then computer_click x=cx y=cy, or use computer_act click_element/type_into to fold the click into a batch. When NOT: element has no accessible text at all (use screenshot + vision computer_locate).",
            serde_json::json!({"type": "object", "properties": {
                "app": {"type": "string", "description": "Native app name from computer_app_targets. Browser apps are rejected."},
                "query": {"type": "string", "description": "Visible text of ONE element, e.g. \"Sign in\""},
                "queries": {"type": "array", "items": {"type": "string"}, "description": "Many texts to check/locate in one call (verification checklist)"}
            }, "required": ["app"]}),
        ),
        "computer_app_read" => (
            "Read EVERYTHING visible in an app in ONE call: the window's text in document order via AT-SPI, buttons/links/fields marked by role. THE perception primitive for verification and 'what does the screen say' — one read answers every presence/wording question at once (checking strings one locate at a time is the signature failure of this lane). When: verifying rendered content, reading a page/dialog/list, grounding before a decision. When NOT: you need click coordinates (computer_app_locate) or pixel-visual judgment like layout/color (computer_screenshot).",
            serde_json::json!({"type": "object", "properties": {
                "app": {"type": "string", "description": "Native app name from computer_app_targets, e.g. \"LibreOffice\". Browser apps are rejected."},
                "max_chars": {"type": "integer", "description": "Cap on returned text (default 6000)"}
            }, "required": ["app"]}),
        ),
        "computer_open" => (
            "Open a NON-BROWSER application by name (for example ExpressVPN or Spotify), or a local non-HTML file/directory with its desktop app. Web URLs, file:// URLs, HTML files, and browser applications are rejected so websites never escape Phoenix; use browser_navigate in the coworker's managed browser instead. Works without the desktop bridge.",
            serde_json::json!({"type": "object", "properties": {
                "target": {"type": "string", "description": "Non-browser application name, absolute local file path, or directory"}
            }, "required": ["target"]}),
        ),
        "computer_locate" => (
            "Find a UI element's exact (x, y) by describing it — a fresh screenshot is taken and a vision grounding pass returns the pixel coordinates (UI-TARS-style). When: you know WHAT to click but cannot read precise coordinates from the last screenshot/caption. Then computer_move/computer_click with the returned point. When NOT: the coordinates are already visible in the caption, or the element is not on screen (navigate first).",
            serde_json::json!({"type": "object", "properties": {
                "target": {"type": "string", "description": "Precise visual description, e.g. 'the blue Submit button below the email field'"}
            }, "required": ["target"]}),
        ),
        "computer_wait" => (
            "Sleep up to 10000 ms for the desktop to settle (app launch, animation, page render) before the next screenshot. When NOT: as a retry loop — two waits without visible progress means report the blocker.",
            serde_json::json!({"type": "object", "properties": {
                "ms": {"type": "integer"}
            }, "required": ["ms"]}),
        ),
        _ => return None,
    };
    Some(ToolDefinition {
        name: name.into(),
        description: description.into(),
        parameters,
    })
}

fn mcp_servers() -> ToolDefinition {
    ToolDefinition {
        name: "mcp_servers".into(),
        description: "Discover the MCP servers the user has connected (local commands or remote URLs) and the tools each exposes, with their arguments. When: before using any external MCP capability — call this first to learn the exact server name, tool names, and argument shapes. When NOT: for connected apps (use composio_search) or built-in Phoenix tools. Takes no arguments.".into(),
        parameters: serde_json::json!({ "type": "object", "properties": {} }),
    }
}

fn mcp_call() -> ToolDefinition {
    ToolDefinition {
        name: "mcp_call".into(),
        description: "Invoke one tool on a configured MCP server (local stdio or remote HTTP). When: acting through an external provider after mcp_servers showed the server, tool, and arguments. When NOT: guessing a server or tool name you didn't confirm with mcp_servers; targeting any system without the user's explicit authorization. Active or offensive tools are for authorized targets only.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "server": {"type": "string", "description": "Configured server name returned by mcp_servers."},
                "tool": {"type": "string", "description": "Tool name on that server, e.g. \"security_recon\"."},
                "arguments": {"type": "object", "description": "Arguments object matching the tool's input schema shown by mcp_servers."}
            },
            "required": ["server", "tool"]
        }),
    }
}

fn design_reference() -> ToolDefinition {
    ToolDefinition {
        name: "design_reference".into(),
        description: "Read Phoenix's embedded design libraries. For actual visual creation or mutation—UI/frontend code, dashboards, landing pages, decks, brand surfaces, generated imagery, or redesigns—load `taste/SKILL.md` FIRST and whole immediately before the work. Taste is the mandatory primary build contract: its Design Read, hierarchy, anti-default discipline, accessibility, and craft gates remain in force for that build. Load at most one exact supplement when the artifact or stack requires it: system references for product UI; process/studio for net-new full-page or redesign work; strict for one matching extraction→build→visual-diff pipeline; immersive for 3D/WebGL; slides for presented decks; or high-agency for React/Tailwind. Do not stack generic or overlapping design/de-slop skills. Supplements never replace or precede Taste. When NOT: conversation, status, explanation, read-only inspection, UI/tool integration research or architecture planning, non-design work, pre-loading the catalog, or loading a supplement as a substitute for Taste.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Library-relative path, e.g. `SKILL.md`, `references/genres/editorial.md`, `references/macrostructures/05-workbench.md`, `taste/SKILL.md` for the primary taste library, or `immersive/SKILL.md` for the 3D/immersive guide. Omit for the file index."}
            }
        }),
    }
}

fn design_studio() -> ToolDefinition {
    let action = serde_json::json!({"type":"object","properties":{"label":{"type":"string","maxLength":160},"target":{"type":"string","maxLength":2048}},"required":["label","target"],"additionalProperties":false});
    let section = serde_json::json!({"type":"object","properties":{
        "id":{"type":"string","maxLength":96},"heading":{"type":"string","maxLength":1024},
        "body":{"type":"array","maxItems":12,"items":{"type":"string","maxLength":4000}},
        "actions":{"type":"array","maxItems":8,"items":action},
        "evidence":{"type":"array","maxItems":16,"items":{"type":"string","maxLength":4000}},
        "user_question":{"type":"string","maxLength":4000},"purpose":{"type":"string","maxLength":4000},
        "dependencies":{"type":"array","maxItems":32,"items":{"type":"string","maxLength":96}},
        "composition":{"type":"string","maxLength":4000},"compact":{"type":"string","maxLength":4000},"motion":{"type":"string","maxLength":4000}
    },"required":["id","heading"],"additionalProperties":false});
    let direction = serde_json::json!({"type":"object","properties":{"accentSeed":{"type":"string"},"neutralSeed":{"type":"string"},"surfaceContrast":{"type":"string","enum":["quiet","defined"]}},"required":["accentSeed","surfaceContrast"],"additionalProperties":false});
    ToolDefinition {
        name:"design_studio".into(),
        description:"Phoenix's local design utilities; no network, model call, file writes or automatic theme changes. Use palette with request.themes.light/dark {accentSeed,neutralSeed?,surfaceContrast:quiet|defined} and optional request.locked theme role maps; returns semantic CSS roles, exact contrast checks, bounded repairs, or blocked locked-color conflicts. Valid locked roles: canvas,surface,surfaceAlt,text,textMuted,divider,controlBorder,accent,accentHover,onAccent,accentText,focusRing. Use typography with a stable project seed to get repeatable non-habitual font candidates; preserve existing brand fonts. Use review_copy or review_blueprint with page {product,category,audience,offer,primary_action,sections}; blueprint sections also need user_question,purpose,dependencies,composition,compact,motion. The tool flags copy/claim/CTA and planning defects. It DOES NOT verify supplied evidence, observe the browser, certify comprehension or approve completion. Follow it with real browser interaction and pixel review. Not needed for ordinary conversation or unrelated code.".into(),
        parameters:serde_json::json!({"type":"object","properties":{
            "action":{"type":"string","enum":["palette","typography","review_copy","review_blueprint"]},
            "seed":{"type":"string","maxLength":1024},
            "request":{"type":"object","properties":{"themes":{"type":"object","properties":{"light":direction.clone(),"dark":direction},"additionalProperties":false},"locked":{"type":"object","properties":{"light":{"type":"object","additionalProperties":{"type":"string"}},"dark":{"type":"object","additionalProperties":{"type":"string"}}},"additionalProperties":false}},"required":["themes"],"additionalProperties":false},
            "page":{"type":"object","properties":{"product":{"type":"string","maxLength":2000},"category":{"type":"string","maxLength":2000},"audience":{"type":"string","maxLength":2000},"offer":{"type":"string","maxLength":2000},"primary_action":{"type":"string","maxLength":2000},"sections":{"type":"array","minItems":1,"maxItems":32,"items":section}},"required":["product","category","audience","offer","primary_action","sections"],"additionalProperties":false}
        },"required":["action"],"additionalProperties":false}),
    }
}

fn design_website() -> ToolDefinition {
    ToolDefinition {
        name: "design_website".into(),
        description: "Start Phoenix's staged design workflow (brief, brand, page, assets, build, preview, review, repair) for a website or page you need to design and build. Call it yourself once you decide the job is designing or building a site, alone in its batch, before writing any site files; the workflow then guides every phase of this turn. Give the complete brief: the business or product, audience, goals, required sections and any facts or preferences the user gave. folder is the absolute path of the site's own folder (created if missing), never Phoenix's source tree. To continue an unfinished run in that folder, call it with resume true. When NOT: questions, feedback, reviews or small edits to an existing page; handle those directly.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "brief": {"type": "string", "description": "The full design request in the user's terms, with every supplied fact and constraint."},
                "folder": {"type": "string", "description": "Absolute path of the site's folder, e.g. /home/<user>/My_Stuff/sites/kornblume."},
                "resume": {"type": "boolean", "description": "true continues the unfinished design run in this folder instead of starting a new one."}
            },
            "required": ["brief", "folder"],
            "additionalProperties": false
        }),
    }
}

fn image_analyze() -> ToolDefinition {
    ToolDefinition {
        name: "image_analyze".into(),
        description: "Inspect an image file on disk. With native vision on the acting model, pixels arrive with your NEXT response; inspect them before reporting a visual conclusion. Otherwise the configured vision model returns an analysis. Pass path and an optional question. With native vision, select task comparison images through reference_paths (up to two whole files): their saved pixels remain beside later observations in this turn. Nonempty reference_paths replaces the retained reference set; a primary-only call updates the current observation without pinning it. The current image comes first; exact duplicate pixels share one labeled attachment. References are not the current screen and do not authorize coordinate actions. Omit crop or use null for the whole image; crop {x,y,width,height} deliberately inspects detail in ORIGINAL PNG/JPEG pixels without changing the file. Inspect the full image before judging overall quality. Use for renders, screenshots, charts and error dialogs. For a live screen or page, capture it first; do not open a viewer just to inspect a file. For generation use image_gen.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Image file path — absolute or workspace-relative (png/jpg/jpeg/webp/gif)."},
                "question": {"type": "string", "description": "Optional: what to find out (\"transcribe all text\", \"which button is highlighted?\"). Omit for a thorough description."}
                ,"reference_paths": {"type":"array","maxItems":2,"items":{"type":"string"},"description":"Explicit whole comparison images for native vision, retained for this turn beside the newest observation. A nonempty selection replaces prior references; omission or an empty list retains them. Their pixels stay as selected, not a live view of changed files."}
                ,"crop": {"type":["object","null"],"description":"Null or omitted means the WHOLE image. Use a region only for a deliberate detail crop in original image pixels. PNG/JPEG only. This does not change the file.","properties":{"x":{"type":"integer","minimum":0},"y":{"type":"integer","minimum":0},"width":{"type":"integer","minimum":1},"height":{"type":"integer","minimum":1}},"required":["x","y","width","height"],"additionalProperties":false}
            },
            "required": ["path"]
        }),
    }
}

fn image_gen() -> ToolDefinition {
    ToolDefinition {
        name: "image_gen".into(),
        description: "Generate an image from a text prompt with the company image-model lane; every named coworker may call it. The image lands in `artifacts/images/` and the tool returns its workspace-relative path. Before design generation, load and apply `design_reference` path `taste/SKILL.md`; art-direct subject, style, palette, lighting, composition, and exclusions. The configured primary provider/model is tried first, followed by the ordered image fallback chain. When NOT: screenshots, analyzing an existing image, or when the tool reports the lane unconfigured—direct the user to Settings → Models & Providers → Image generation instead of pretending another model can create it.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {"type": "string", "description": "What to generate — specific beats vague: subject, style, palette, lighting, composition, negative cues."},
                "name": {"type": "string", "description": "Optional file stem for the PNG (slugified). Default: derived from the prompt."},
                "size": {"type": "string", "description": "Optional size, e.g. 1024x1024 (default), 1536x1024 (landscape), 1024x1536 (portrait)."}
            },
            "required": ["prompt"]
        }),
    }
}

fn skill() -> ToolDefinition {
    ToolDefinition {
        name: "skill".into(),
        description: "List or load installed skills — portable SKILL.md packages holding proven procedures for specific kinds of work (video rendering, deploys, document formats…). When: the runtime context lists a genuinely specialized skill matching the task — load its SKILL.md FIRST and follow it instead of improvising; call with no arguments to list what's installed. When NOT: the task matches no installed skill, re-loading a skill already in context, or loading a writing/style/prose skill merely to format an ordinary chat answer, summary, triage, or research briefing—clear human prose is baseline behavior and does not justify adding a large playbook to every later model round. Progressive disclosure: list is one line per skill; load reference files only when the SKILL.md tells you to.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Skill name from the list. Omit to list all installed skills."},
                "file": {"type": "string", "description": "File inside the skill (e.g. `references/setup.md`). Omit for SKILL.md."}
            }
        }),
    }
}

fn skill_install() -> ToolDefinition {
    ToolDefinition {
        name: "skill_install".into(),
        description: "Install a portable SKILL.md package into ~/.phoenix/skills. Source accepts the id straight from skill_search results (`owner/repo/skill-name`), a GitHub URL, or a local path. Flow: skill_search first, then install a result with 500+ installs — listings under 500 installs are BANNED and this tool refuses them (supply-chain bar); sources skills.sh has never indexed install only with `user_authorized: true`, which you may set ONLY when the user explicitly named that exact source. BE PROACTIVE: when the current task needs a capability or current stack knowledge a skill provides (framework best practices, an API's workflow), find and install it without waiting to be asked — a closed capability gap serves every future task. Always name what you are installing and from where in your visible text BEFORE the call; never install silently, in the background, or unrelated to the task at hand. When NOT: reinstalling an already-installed skill.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "source": {"type": "string", "description": "GitHub URL (repo or /tree/<branch>/<subpath> deep link), owner/repo[/subpath] shorthand, or a local directory path containing SKILL.md."},
                "name": {"type": "string", "description": "Install under this name; also disambiguates when a repo holds several skills."}
            },
            "required": ["source"]
        }),
    }
}

fn motion_graphics() -> ToolDefinition {
    ToolDefinition {
        name: "motion_graphics".into(),
        description: "Phoenix's motion-graphics workflow (the bundled motionmaxxing skill: a senior motion director's playbook, a seek-safe GSAP runtime, deterministic Chrome→ffmpeg rendering, and scripted review gates). When: ANY time motion, animation or video would help the result — launch films, promos, product/feature reveals, brand stings, logo animations, kinetic type, social ads, UI demo videos, animated heroes, loading, intro or transition animations, animated explainers. Prefer it over hand-rolled CSS keyframes, ad-hoc canvas loops, or raw ffmpeg; for in-app micro-interactions, still load its timing (`guide` file `motion`) instead of guessing eases and durations. Flow: `guide` (SKILL.md + Phoenix adapter; then load references on demand with `file`, e.g. `idea`, `world`, `motion`, `ui-demo`, `transitions`, `close`) → `check` (Node 22+/Chrome/ffmpeg/Python; reports what is missing instead of failing) → `start` (copies the film template and runtime into `project`, default `film/`) → write STORYBOARD.md and build `index.html` → `still` (PNG stills at `times`) → `review` (draft render + look.py + lint.mjs: returns G0/G2/G3/G5 and the contact sheet; then open look/sheet.jpg with image_analyze for G1/G4) → `render` (final, with `audio`/`shutter`/`grain`) → `look` (with `expect_audio`). `script` runs any other skill script (brand.mjs to capture a site's brand, fetch_logo.mjs, precedent.py, voice.py for ElevenLabs VO/SFX/music, sync.mjs, mix.py, imagegen.py, blind_review.py) with `args`. Returns output file paths and gate results; a failing gate is fixed, never argued. When NOT: a static image (image_gen) or a static page with no motion.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["guide", "check", "start", "still", "review", "render", "look", "lint", "script"], "description": "Step to run. Default `guide`."},
                "project": {"type": "string", "description": "Film project folder, workspace-relative. Default `film`."},
                "file": {"type": "string", "description": "guide: skill file to load, e.g. `motion`, `idea`, `references/world.md`, `runtime/README.md`, `taste/verdicts.md`. Omit for SKILL.md."},
                "html": {"type": "string", "description": "Composition to render/lint. Default `<project>/index.html`."},
                "video": {"type": "string", "description": "render/review: output video (default `<project>/final.mp4`; review `<project>/draft.mp4`). look: the video to inspect."},
                "times": {"type": "string", "description": "still: comma-separated seconds (default `0.5,2,4`). lint: sample times."},
                "scale": {"type": "number", "description": "Render pixel scale (review defaults to 0.5 for fast drafts)."},
                "fps": {"type": "number"},
                "from": {"type": "number", "description": "Render only from this second (hero beats, slow WebGL)."},
                "to": {"type": "number", "description": "Render only up to this second."},
                "audio": {"type": "string", "description": "render: mixed audio file to mux (workspace path)."},
                "shutter": {"type": "number", "description": "render: shutter angle for motion blur (smooth-clock films only), e.g. 180."},
                "subframes": {"type": "integer", "description": "render: subframes per frame with shutter (default 8)."},
                "grain": {"type": "number", "description": "render: film grain 0.03-0.06 on gradient/photo/3D films."},
                "expect": {"type": "number", "description": "look/review: planned length in seconds (G0 length check)."},
                "expect_audio": {"type": "boolean", "description": "look/review: require a non-silent audio track."},
                "script": {"type": "string", "description": "script: one of providers.sh, brand.mjs, fetch_logo.mjs, precedent.py, imagegen.py, voice.py, sync.mjs, mix.py, render.mjs, look.py, lint.mjs, blind_review.py."},
                "args": {"type": "array", "items": {"type": "string"}, "description": "script: arguments, passed verbatim (workspace-relative paths)."},
                "force": {"type": "boolean", "description": "start: replace an existing index.html with the template."},
                "timeout_secs": {"type": "integer", "description": "Ceiling for this step (max 600). Render long films in --from/--to ranges."}
            }
        }),
    }
}

fn skill_search() -> ToolDefinition {
    ToolDefinition {
        name: "skill_search".into(),
        description: "Search skills.sh — the cross-agent Agent Skills registry (the same index `npx skills add` uses; millions of installs, ranked results). When: the task touches a framework, API, format, or platform you don't have current, specific knowledge of (Next.js best practices, Stripe flows, video rendering, PDF work, deploys) — search HERE FIRST, before improvising and never hunting for skills with web_search. Each qualifying result line carries the exact `source` id to pass to skill_install; results under 500 installs are marked BANNED (skill_install refuses them). Picking among results: prefer high install counts (1K+ = battle-tested) and reputable sources (anthropics/, vercel-labs/, the framework's own org); name your pick and why before installing. When NOT: the skill is already installed (check `skill` list), or the user gave you a direct repo URL.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "What you're looking for, e.g. `remotion`, `pdf`, `stripe`."},
                "limit": {"type": "integer", "description": "Max results (default 8, cap 25)."}
            },
            "required": ["query"]
        }),
    }
}

fn composio_search() -> ToolDefinition {
    ToolDefinition {
        name: "composio_search".into(),
        description: "Use for app/service tasks (Gmail, GitHub, Slack, Notion, X, Reddit, Tavily web-search/research, etc.) — searches the user's connected Composio apps for the right tools by use-case and returns tool slugs plus compact exact schemas. Flow: check the connected-apps list in RUNTIME CONTEXT (AUTHORITATIVE) → composio_search → composio_run. When search includes `tool_schemas` (the normal path), use them and NEVER call composio_schemas again. `composio_schemas` is only a fallback when search did not return a schema or when enumerating a connected/no-auth app search missed. KNOWN LIMIT (verified live): the server-side planner only routes to apps with ACTIVE auth connections — it will NEVER surface no-auth apps (hackernews) or a connection made moments ago, even though they execute fine. If an app on the connected list isn't in the results, do NOT re-search with reworded queries and do NOT conclude it is unavailable: call composio_schemas with {\"toolkit\": \"<app>\"} to enumerate its real tools, then composio_run. Only when the app is NOT connected, or genuinely lacks the action (e.g. Reddit has no inbox/DM reader), reroute to the lane that can do it (browser for interactive/social pages, computer_use for the desktop). When NOT: pure repo work or a fact you already have.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "additionalProperties": true,
            "properties": {
                "queries": {"type": "array", "items": {
                    "type": "object",
                    "properties": {
                        "use_case": {"type": "string", "description": "Normalized English description of ONE app action and its outcome, naming the app when known, e.g. 'create a github issue in a repository'. No personal identifiers here — put those in known_fields."},
                        "known_fields": {"type": "string", "description": "Optional comma-separated key:value hints (stable identifiers only), e.g. 'repo:owner/name' or 'channel_name:general'."}
                    },
                    "required": ["use_case"]
                }, "description": "One query object per independent app action; include hidden prerequisites as their own queries (e.g. 'get the issue' before 'update the issue')."},
                "session": {"type": "object", "properties": {
                    "generate_id": {"type": "boolean", "description": "true on the FIRST search of a new workflow."},
                    "id": {"type": "string", "description": "Existing session id to continue a workflow."}
                }, "description": "Always pass: {\"generate_id\": true} for a new workflow, or {\"id\": \"…\"} to continue one. Reuse the returned session_id in composio_schemas/composio_run."}
            },
            "required": ["queries"]
        }),
    }
}

fn composio_schemas() -> ToolDefinition {
    ToolDefinition {
        name: "composio_schemas".into(),
        description: "Get exact input schemas for specific Composio tools by passing `tool_slugs` from composio_search. Fallback discovery: pass `toolkit` (lowercase app name) only when search missed a connected/no-auth app; this returns a compact SLUG INDEX, not every schema. Choose one slug, then call composio_schemas once more with only that exact slug. Never request a whole toolkit when search already supplied `tool_schemas`, and never invent tool slugs.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "additionalProperties": true,
            "properties": {
                "tool_slugs": {"type": "array", "items": {"type": "string"}, "description": "Tool slugs from composio_search, e.g. ['GITHUB_CREATE_AN_ISSUE']. Omit when using toolkit."},
                "toolkit": {"type": "string", "description": "App/toolkit slug to enumerate instead of tool_slugs — lowercase app name, e.g. 'hackernews', 'gmail'. Returns ALL of the app's tools with schemas."},
                "session_id": {"type": "string", "description": "Session id from composio_search, if any."}
            }
        }),
    }
}

fn composio_run() -> ToolDefinition {
    ToolDefinition {
        name: "composio_run".into(),
        description: "Execute one or more Composio app tools discovered via composio_search, against the user's REAL connected accounts (their mail, repos, posts). Run independent tools in one call. When multiple accounts are connected, put the exact connected account id from composio_search at the SAME LEVEL as tool_slug: {tool_slug, account, arguments}; never place account/account_id inside arguments and never use an app's user_id as the connected-account selector. Binding rules: only what the user explicitly asked; before any irreversible/outward action (send, post, delete, publish) state in your visible text exactly what will go out BEFORE calling; report results honestly. Preserve external completion state: when rescheduling, renaming, or enriching an existing item, omit checkbox/completed fields rather than setting them false. Only when the user explicitly asked to reopen or mark a completed item incomplete may a Notion update include user_confirmed_reopen_completed:true beside tool_slug; Phoenix strips that local acknowledgement before execution. When NOT: speculative calls or anything the user didn't clearly request.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "additionalProperties": true,
            "properties": {
                "tools": {"type": "array", "description": "Logically independent tools to execute. `account` selects a connected account and is outside `arguments`.", "items": {"type": "object", "properties": {
                    "tool_slug": {"type": "string"},
                    "account": {"type": "string", "description": "Connected account id from composio_search, beside tool_slug—not an app argument."},
                    "user_confirmed_reopen_completed": {"type": "boolean", "description": "Phoenix-local safety acknowledgement. Set true only when the user explicitly asked to reopen or mark an existing completed Notion item incomplete; never set it merely to reschedule, rename, or enrich an item."},
                    "arguments": {"type": "object", "description": "Only the app action's exact schema fields."}
                }, "required": ["tool_slug", "arguments"]}},
                "thought": {"type": "string", "description": "One-sentence rationale."},
                "sync_response_to_workbench": {"type": "boolean", "enum": [false], "description": "Always false in Phoenix. Request bounded pages or compact fields instead; Composio's remote workbench filesystem is not part of the local agent workspace."},
                "session_id": {"type": "string", "description": "Session id from composio_search, if any."}
            },
            "required": ["tools", "sync_response_to_workbench"]
        }),
    }
}

fn composio_connections() -> ToolDefinition {
    ToolDefinition {
        name: "composio_connections".into(),
        description: "List or create the user's Composio app connections. Use to confirm which apps are connected or to start connecting a new one (returns a sign-in link the USER must open). composio_search already reports connection status, so reach here mainly to connect a missing app. Each toolkits entry is `{\"name\": \"<toolkit>\", \"action\": \"list\"|\"add\"|\"rename\"|\"remove\"}` — e.g. `{\"toolkits\": [{\"name\": \"github\", \"action\": \"list\"}]}`.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "additionalProperties": true,
            "properties": {
                "toolkits": {"type": "array", "description": "Connection operations. Each item: {\"name\": toolkit id (e.g. `github`, `gmail`), \"action\": one of `list`, `add`, `rename`, `remove`}.", "items": {"type": "object", "properties": {"name": {"type": "string"}, "action": {"type": "string", "enum": ["list", "add", "rename", "remove"]}}, "required": ["name", "action"]}},
                "session_id": {"type": "string", "description": "Session id from composio_search, if any."}
            },
            "required": ["toolkits"]
        }),
    }
}

/// The structured-finish tool. Calling it ends the turn with a validated
/// `FinalResponse` — the provider's function-calling machinery guarantees the
/// shape, which removes the fragile free-text JSON envelope parse + repair path.
fn user_update() -> ToolDefinition {
    ToolDefinition {
        name: "user_update".into(),
        description: "Send one intentional mid-work update to the user while continuing the task. At the start of a new user request, briefly confirm in your own words what you understood and will do, specific to this request; no canned phrases. An immediately complete answer can go straight to final_answer. When the user sends a message while you are working, reply to it with one short update in your own words before continuing. Other updates are rare: only a material result, important change, or necessary confirmation. Routine narration, reasoning, drafts, repeated paraphrases and per-tool status stay private. Deliver the completed result once with final_answer without repeating earlier updates. For a question or required approval use ask_user instead. Never include secrets.".into(),
        parameters: serde_json::json!({"type":"object","properties":{"message":{"type":"string","minLength":1,"maxLength":1200,"description":"A short, concrete update the user needs to see."}},"required":["message"],"additionalProperties":false}),
    }
}

fn final_answer() -> ToolDefinition {
    ToolDefinition {
        name: "final_answer".into(),
        description: "Complete your current assignment. Your final answer is for the user and appears in your own conversation. The user already read successful user_update messages: add the result and new information instead of repeating or paraphrasing those updates. Retain earlier facts only when necessary for a complete requested deliverable. Earlier updates fold under this reply. When a coworker asked for this work in the background, it is NOT sent back to them: before finishing, tell the asker yourself with one short message_agent (no reply expected) carrying the key result. A blocking (mode-1) request still returns your result to the waiting coworker. In a personal conversation you own the request—Phoenix is not a supervisor or mandatory relay. Call exactly once, last, after necessary tools finish. Put the complete answer in final_markdown; never emit a JSON envelope as plain text.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string", "description": "One-line summary of the outcome."},
                "final_markdown": {"type": "string", "description": "The complete, user-facing answer in Markdown. This is what the user reads. Address every part of the request."},
                "changes_made": {"type": "array", "items": {"type": "string"}, "description": "Concrete changes made this turn (files edited, actions taken). Empty for pure analysis/answers."},
                "verification": {"type": "array", "items": {"type": "string"}, "description": "How the work was verified (tests run, sources checked). Empty if not applicable."}
            },
            "required": ["final_markdown"]
        }),
    }
}

fn read() -> ToolDefinition {
    ToolDefinition {
        name: "read".into(),
        description: "Read text or PDF contents from the workspace, with optional line paging. Do not read binary images or native project files (.png, .jpg, .blend) as text: use image_analyze for image pixels, list_directory to verify file presence/size, and the native application to inspect its project. PDFs are extracted headlessly and never opened in a shared desktop window, so independent workers can read separate files safely. A full page is up to 2000 lines; the summary reports the total line count and the offset for the next page when a file is longer. To view a specific range (e.g. lines 1760-1820), pass offset+limit — NEVER shell out to `bash sed`/`head`/`tail` to read a file slice; this tool does it in one call. When: known path, or a grep/glob/symbol_search hit you will use. When NOT: blind repo tour; skimming a whole file just to locate one function (`file_symbols` outlines it in one call, then read the exact range); substitute for grep; third re-read without new questions. Batch with glob/grep.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Relative path within the workspace root."},
                "question": {"type": "string", "description": "What you expect this read to answer; stored with the file hash as a reusable evidence receipt."},
                "offset": {"type": "integer", "description": "1-based line to start at. Omit to read from the top."},
                "limit": {"type": "integer", "description": "Max lines to return from offset (default 2000, cap 4000)."},
                "line_numbers": {"type": "boolean", "description": "Use true for code review or exact line citations; adds source line labels. Default false returns verbatim text for edit anchors. Never guess line numbers from unnumbered output."}
            },
            "required": ["path"]
        }),
    }
}

fn write() -> ToolDefinition {
    ToolDefinition {
        name: "write".into(),
        description: "Create or overwrite a file. When: new file or >~20% rewrite. When NOT: small edits (use str_replace). Before overwriting an EXISTING file in Talk or Workspace mode, read that exact path in this session; the runtime rejects blind or stale edits. Requires overwrite=true for existing files. Keep one call's content under about 50 KB: build a bigger file in parts (write the first section, then add the rest with str_replace), so each step stays quick and a failed reply loses little. Missing parent directories are created automatically. NEW Markdown defaults to a private working note under ~/.phoenix/artifacts so plans, research scratch, and handoffs cannot litter the user's repository. Set purpose=project_source only for a real repository file (README, ADR, checked-in docs), or purpose=deliverable only when the user asked for a Markdown artifact. Existing Markdown is always edited in place.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Relative path."},
                "content": {"type": "string", "description": "Full file content."},
                "overwrite": {"type": "boolean", "description": "Must be true to replace an existing file."},
                "purpose": {"type": "string", "enum": ["working_note", "project_source", "deliverable"], "default": "working_note", "description": "Classify a NEW Markdown file. Omit for transient notes; be explicit only when the workspace file itself is required."}
            },
            "required": ["path", "content"]
        }),
    }
}

fn str_replace() -> ToolDefinition {
    ToolDefinition {
        name: "str_replace".into(),
        description: "Exact-string replacement in a file (Coder). When: surgical edits; old_str must match whitespace exactly. Read that exact file in this session immediately before its first edit in Talk or Workspace mode; the runtime rejects blind or stale edits. When NOT: new files; large rewrites; guessing after a failed match. Use the error's current-text excerpt for an exact retry, or read the indicated region if more context is needed. allow_multiple for repeated occurrences.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "old_str": {"type": "string"},
                "new_str": {"type": "string"},
                "allow_multiple": {"type": "boolean", "description": "Replace all matches if true."}
            },
            "required": ["path", "old_str", "new_str"]
        }),
    }
}

fn grep() -> ToolDefinition {
    ToolDefinition {
        name: "grep".into(),
        description: "Ripgrep search in workspace; returns path:line:match. When: literal strings, config values, error messages, comments, non-code text; multiple patterns per objective. When NOT: finding where a function/type is defined or used — that's `symbol_search`/`callers`/`callees` against the prebuilt index, ONE call instead of a grep-then-read tour; also NOT one vague pattern for multi-part work, or bash grep/cat. Use targeted regex. Pass `context` to see surrounding lines in the same call (instead of a follow-up read), `files_only` to just locate which files contain it, `glob` to scope by file type.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regex pattern."},
                "path": {"type": "string", "description": "Optional directory or file scope."},
                "glob": {"type": "string", "description": "Optional file filter, e.g. '*.rs' or 'src/**/*.ts'."},
                "context": {"type": "integer", "description": "Lines of context on each side of a match (0-10). Saves a follow-up read."},
                "ignore_case": {"type": "boolean", "description": "Case-insensitive match. Default false."},
                "files_only": {"type": "boolean", "description": "List only the files that contain a match, not the lines."}
            },
            "required": ["pattern"]
        }),
    }
}

fn define_codebase_search() -> ToolDefinition {
    ToolDefinition {
        name: "codebase_search".into(),
        description: "Find code by meaning (BM25 over chunks). When: how/where/what questions; unknown symbol location; explore feature flow. When NOT: exact string/symbol (use grep); single known file (use read). Reuse user query wording. Warns if >500 files.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Natural-language search; prefer the user's exact wording."},
                "target_directories": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Optional directory scopes relative to workspace, e.g. [\"phoenix_agent/src\"]."
                },
                "limit": {"type": "integer", "description": "Max chunks to return (default 12, max 25)."}
            },
            "required": ["query"]
        }),
    }
}

fn index_codebase() -> ToolDefinition {
    ToolDefinition {
        name: "index_codebase".into(),
        description: "Refresh Phoenix's local structural code index. The runtime already indexes once at coding-turn start and reindexes once after completed edits. Use this manually only when the index is missing or demonstrably stale; never call it after every edit. User-visible activity reads 'Indexing codebase'.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    }
}

fn symbol_search() -> ToolDefinition {
    ToolDefinition {
        name: "symbol_search".into(),
        description: "Find symbols (functions, types, traits, impls) across the project by name/meaning via Phoenix's codebase index. When: locating where something is defined before reading; 'where is X handled'. When NOT: exact full-text string (use grep); reading a known file (use read). Cheaper than reading files — query the index for structure, read files for detail.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Symbol name or meaning-style query."},
                "limit": {"type": "integer", "description": "Max results (default 12, max 40)."}
            },
            "required": ["query"]
        }),
    }
}

fn callers() -> ToolDefinition {
    ToolDefinition {
        name: "callers".into(),
        description: "List every indexed symbol that calls a given function/method. When: BEFORE editing a symbol — see who depends on it; assessing blast radius. When NOT: you only need the definition (use symbol_search). Returns path:line for each caller.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Bare symbol name, not a path."},
                "limit": {"type": "integer", "description": "Max callers (default 25, max 100)."}
            },
            "required": ["name"]
        }),
    }
}

fn callees() -> ToolDefinition {
    ToolDefinition {
        name: "callees".into(),
        description: "List the indexed symbols a given function/method calls. When: understanding what a function does without reading it fully; tracing a flow downward. When NOT: you need full implementation detail (read the file).".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Bare symbol name."},
                "limit": {"type": "integer", "description": "Max callees (default 25, max 100)."}
            },
            "required": ["name"]
        }),
    }
}

fn impact() -> ToolDefinition {
    ToolDefinition {
        name: "impact".into(),
        description: "Transitive closure of indexed callers — everything affected if a symbol changes, ranked by call distance. When: before a risky refactor or signature change, to size the blast radius. When NOT: trivial local edits. The single highest-value pre-edit check.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Bare symbol name to assess."}
            },
            "required": ["name"]
        }),
    }
}

fn call_path() -> ToolDefinition {
    ToolDefinition {
        name: "call_path".into(),
        description: "Shortest indexed static call path between two symbols — 'how does X reach Y' as an ordered hop chain with file:line per hop. When: tracing a flow across the codebase, verifying a suspected dependency, or explaining how a request reaches storage. When NOT: single-hop questions (use callers/callees) or blast-radius sizing (use impact). Honest limits: static call edges only — dynamic dispatch, trait objects, callbacks, and cross-process hops are invisible; an empty result is 'no static path', not 'no relationship'.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "from": {"type": "string", "description": "Start symbol (bare name)."},
                "to": {"type": "string", "description": "Target symbol (bare name)."}
            },
            "required": ["from", "to"]
        }),
    }
}

fn file_symbols() -> ToolDefinition {
    ToolDefinition {
        name: "file_symbols".into(),
        description: "Compact indexed symbol outline of one file — names, kinds, line spans, signatures. When: orienting in a file before reading it fully; cheaper than read for structure. When NOT: you need the actual code body (use read).".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Workspace-relative file path."}
            },
            "required": ["path"]
        }),
    }
}

fn glob() -> ToolDefinition {
    ToolDefinition {
        name: "glob".into(),
        description: "Discover files by glob pattern. Patterns are workspace-relative by default (plans/**/*.md, src/**/*.rs). Under Full Access, a parent-relative prefix such as ../Sibling/**/*.rs is allowed and resolved with the same safety policy as read/list_directory; Workspace mode requires approval before crossing the project boundary. Absolute patterns and parent segments after a wildcard are rejected. When NOT: stop at listing without reading matches; replace grep for content search.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Workspace-relative glob, or an approved parent-relative glob under Full Access."}
            },
            "required": ["pattern"]
        }),
    }
}

fn list_directory() -> ToolDefinition {
    ToolDefinition {
        name: "list_directory".into(),
        description: "List one directory with basic metadata. When: unfamiliar folder, once. When NOT: deep inspection (read/grep); repeated listing same path.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory relative to workspace root."}
            },
            "required": ["path"]
        }),
    }
}

fn bash() -> ToolDefinition {
    ToolDefinition {
        name: "bash".into(),
        description: "Run shell in workspace for execution, not code browsing. When: build, test, lint, format, git status/diff/log, package-manager commands, environment diagnostics. When NOT: cat/head/tail/grep/rg/ls/find/sed/wc or other file-reading/search/listing commands — use read, grep, glob, list_directory, codebase_search, and symbol graph tools instead. Destructive commands are blocked; servers/watchers/interactive commands are killed at the timeout. Set cwd param — do NOT embed cd in command (Windsurf). Commands are killed after 180s by default; set timeout_secs (max 600) for legitimately slow builds. Set runner only to an id returned by remote_runner when the user intentionally configured a pre-provisioned SSH workspace; Phoenix never syncs local files implicitly.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Shell command. No destructive patterns."},
                "cwd": {"type": "string", "description": "Working directory relative to workspace root. Default '.'."},
                "timeout_secs": {"type": "integer", "description": "Runtime ceiling in seconds. Default 180, max 600. Raise only for slow builds/tests."},
                "runner": {"type": "string", "description": "Optional configured remote-runner id. Use remote_runner to list trusted targets first."}
            },
            "required": ["command"]
        }),
    }
}

fn background_terminal() -> ToolDefinition {
    let mut definition=bash();
    definition.name="background_terminal".into();
    definition.description="Start a managed terminal command in the background using this conversation's existing workspace and permission mode. Returns a job id immediately; a RUNNING/start receipt is not success or completion. Use for builds/tests or other bounded shell work that should outlive the current answer. After starting it you may give the user a final answer saying what is still running. Phoenix resumes this same conversation automatically only when the actual process exits and its durable result is ready; do not poll in a loop, invent completion, create a coworker, or send another user prompt. Use terminal_job for status or cancellation. Same command/cwd/runner rules and 600-second maximum as bash; backgrounding does not widen permissions or authorize another action.".into();
    definition
}

fn terminal_job() -> ToolDefinition {
    ToolDefinition {
        name:"terminal_job".into(),
        description:"Read the actual status/output or request cancellation of a background_terminal job owned by this conversation and acting agent. A cancelling receipt is not confirmed termination; read status if confirmation is needed. Completion is based on the managed process result, never interim assistant text. Completed commands are not rerun by this tool.".into(),
        parameters:serde_json::json!({"type":"object","properties":{
            "job_id":{"type":"string"},"action":{"type":"string","enum":["status","cancel"]}},
            "required":["job_id","action"]}),
    }
}

fn remote_runner() -> ToolDefinition {
    ToolDefinition {
        name: "remote_runner".into(),
        description: "List user-configured remote execution targets and their non-secret trust metadata. This never connects, registers a host, reveals a private key path, or changes configuration. Use a returned id in bash.runner only when the user's task intentionally calls for remote compute and the remote workspace is already provisioned.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {}
        }),
    }
}

fn talk() -> ToolDefinition {
    ToolDefinition {
        name: "talk".into(),
        description: "Ask an active coworker by immutable role ID or human name when their private context, authority, or distinct judgment materially improves the outcome. Coworkers are equal peers; Phoenix is not a supervisor. Do not delegate a simple tool operation or create a review/acknowledgement ceremony. Mode 1 requests needed information or work and returns its answer to you. Mode 2 sends a one-way task or notice without reciprocal acknowledgements. Return your own completed assignment with final_answer, not another talk. Group dependencies determine when selected coworkers run; do not duplicate already-owned work. Include the relevant goal, evidence, deliverable, constraints and approvals, because access to private transcripts is scoped.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "intent": {"type":"string", "enum":["question","task"], "description":"Use question for a genuine unresolved question, required when asking the caller of your current assignment. Finished deliverables and review verdicts use final_answer, never a new question."},
                "to": {"type": "string", "minLength": 1, "description": "Active coworker role/id or human name, resolved at execution time. Use the role ids and names from YOUR TEAM in your context (role ids such as orchestrator, coder, researcher, frontend, critic, marketing). Responsibility aliases such as operations, security, documents, CRM, and publishing also resolve. Hidden craft specialists and custom coworkers resolve the same way."},
                "subject": {"type": "string", "description": "Log title only."},
                "body": {"type": "string", "description": "Objective, scope, evidence, deliverable, verification, constraints."},
                "mode": {"type": "integer", "enum": [1, 2], "description": "1 = a real answer is required and returns once to this sender; 2 = one-way task/notice with no acknowledgement loop."},
                "room_mode": {"type": "string", "enum": ["request", "assign", "broadcast", "escalate"], "description": "Group rooms only. request (default) = one teammate outside this room. assign = group leader posts a visible assignment that wakes the named member. broadcast = leader posts to the room and wakes the named member or everyone (to: \"everyone\"). escalate = a member reaches the leader; the leader escalates to the user with ask_user."},
                "plan_item_id": {"type": "string", "description": "Group rooms only: mission-board plan item this assignment covers."}
            },
            "required": ["to", "subject", "body", "mode"]
        }),
    }
}

fn message_agent() -> ToolDefinition {
    ToolDefinition {
        name: "message_agent".into(),
        description: "Send non-blocking context, a correction, or a notice to one or more coworkers while continuing your own turn. This is communication, not delegated ownership: a running recipient is steered at its next safe model boundary, while an idle recipient keeps the message in its durable inbox for its next turn. Use talk when the recipient must start scoped work and return a result. Address individuals with `to`, or one company group with `group`; group delivery expands active members in stable directory order. High and urgent messages jump ordinary mailbox/steer traffic. Attachments are durable references and may include images. Do not send acknowledgements or duplicate a message already accepted. If your reply tells the user you will contact a coworker, this call (or talk) must happen in the same turn; a delivery failure is returned as an error and must not be described as sent.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "to": {"type": "array", "items": {"type": "string", "minLength": 1}, "maxItems": 32, "description": "Coworker ids or human names."},
                "group": {"type": "string", "description": "Optional group id or name. Its active members are expanded in directory order."},
                "subject": {"type": "string", "minLength": 1, "maxLength": 240},
                "body": {"type": "string", "minLength": 1},
                "priority": {"type": "string", "enum": ["low", "normal", "high", "urgent"], "default": "normal"},
                "attachments": {"type": "array", "maxItems": 16, "items": {
                    "type": "object",
                    "properties": {
                        "uri": {"type": "string", "minLength": 1},
                        "name": {"type": "string"},
                        "media_type": {"type": "string", "description": "For example image/png or application/pdf."}
                    },
                    "required": ["uri"]
                }}
            },
            "required": ["subject", "body"],
            "anyOf": [
                {"required": ["to"]},
                {"required": ["group"]}
            ]
        }),
    }
}

fn agent_control() -> ToolDefinition {
    ToolDefinition {
        name: "agent_control".into(),
        description: "Inspect and control detached coworker jobs without starting a second hidden conversation. list/status returns running jobs, completed-but-unabsorbed results, and the ordered live journal; message steers a running coworker, with high/urgent priority delivered first at the next safe model boundary; stop cancels the selected live job; resume starts a continuation in that coworker's same durable session using the supplied subject/body. Use this for lifecycle control, not ordinary delegation.".into(),
        parameters: serde_json::json!({
            "type":"object",
            "properties":{
                "action":{"type":"string","enum":["list","status","message","stop","resume"]},
                "agent":{"type":"string","description":"Required for status/message/stop/resume."},
                "subject":{"type":"string","description":"Required for message/resume."},
                "body":{"type":"string","description":"Required for message/resume."},
                "priority":{"type":"string","enum":["low","normal","high","urgent"],"default":"normal"},
                "attachments":{"type":"array","maxItems":16,"items":{"type":"object","properties":{"uri":{"type":"string","minLength":1},"name":{"type":"string"},"media_type":{"type":"string"}},"required":["uri"]}}
            },
            "required":["action"]
        }),
    }
}

fn volume_work() -> ToolDefinition {
    ToolDefinition {
        name: "volume_work".into(),
        description: "Spawn ephemeral generic workers for a genuinely high-volume set of independent, similarly shaped jobs. Workers inherit the caller's permission mode, current authoritative completion/correction evidence, vault/account owner, and authenticated browser profile. Each worker keeps independent tabs, browser target, terminal, desktop process context, and lifecycle, so parallel work cannot mutate a sibling's active tab; browser cookies and site storage come from the spawning coworker's live private session, while vault secrets stay encrypted and are revealed only inside the native credential broker. If the desktop bridge is unavailable, an isolated headless profile receives only a portable-cookie fallback. Prefer `talk` to a named coworker whenever durable role memory, specialist judgment, coordination, or ownership matters. Never use this for a single task or sequentially dependent items. Workers have no per-item wall-clock deadline; the parent/user Stop action remains cancellable and cleans up every temporary session, desktop, browser, and worker context. Results return in request order.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "objective": {"type": "string", "minLength": 1, "maxLength": 4000, "description": "Shared batch objective and integration criterion."},
                "jobs": {"type": "array", "minItems": 2, "maxItems": 32, "items": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "minLength": 1, "maxLength": 96},
                        "task": {"type": "string", "minLength": 1, "description": "One self-contained independent job."},
                        "context": {"type": "string", "description": "Item-specific evidence. Workers do not receive the full private transcript, but the runtime separately injects the parent's bounded authoritative completion/correction snapshot."},
                        "expected_output": {"type": "string", "description": "Optional exact result shape or acceptance evidence."}
                    },
                    "required": ["id", "task"]
                }},
                "max_concurrency": {"type": "integer", "minimum": 1, "maximum": 32, "description": "Optional per-call ceiling; Settings may lower it."}
            },
            "required": ["objective", "jobs"]
        }),
    }
}

fn work() -> ToolDefinition {
    let evidence_requirements = serde_json::json!({
        "type":"array", "maxItems":256,
        "items":{
            "type":"object",
            "properties":{
                "kind":{"type":"string","minLength":1},
                "description":{"type":"string","minLength":1},
                "minimum_receipts":{"type":"integer","minimum":0,"maximum":256},
                "required":{"type":"boolean"}
            },
            "required":["kind","description","minimum_receipts","required"],
            "allOf":[{
                "if":{"properties":{"required":{"const":true}},"required":["required"]},
                "then":{"properties":{"minimum_receipts":{"minimum":1}}}
            }]
        },
        "description":"Commit requirements. Every item needs all four fields; a required kind needs at least one verified receipt. Receipt kind must match exactly."
    });
    ToolDefinition {
        name: "work".into(),
        description: "Read or change the CURRENT SESSION'S shared company work graph. This is coordination state, never transcript search, prior-run recovery, general memory, or a source for news/research evidence. Use inspect only when joining or monitoring a concrete current-session work item; it may omit node_id to list that run. status requires a non-empty node_id. propose defines an outcome and acceptance evidence; claim/join registers a candidate approach; publish registers a content-addressed artifact; challenge disputes a claim with evidence; decide records a peer decision; release returns work to the ready pool; note records a company-visible fact. skill_checkpoint proves an activated playbook step. propose_learning stages a memory/skill/runtime improvement. For long-lived execution, action=workflow exposes the complete durable lifecycle: create_goal → open_run → define_node → reconcile/tick → heartbeat/evidence → transition through prepare/plan/execute/review/commit. Leases are fenced, budgets and evidence are checked at commit, and goal concurrency controls scheduling. Use talk for conversation and work only for durable commitments.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["inspect", "workflow", "propose", "claim", "join", "status", "release", "publish", "challenge", "decide", "note", "skill_checkpoint", "propose_learning", "promote_learning"], "description": "Pick one action. Ordinary single-owner tasks do not need work at all. inspect is read-only. status MUTATES an existing node and therefore always requires node_id plus state."},
                "workflow_action": {"type": "string", "enum": ["install_plan", "create_goal", "open_run", "define_node", "inspect", "reconcile", "tick", "heartbeat", "wait_for", "resolve_wait", "record_evidence", "transition", "resume", "reroute"], "description": "For action=workflow. Prefer install_plan for a new named graph: workflow_payload {idempotency_key,contract:{title,objective,budget,evidence_requirements?,concurrency?,metadata?,ownership?},assignments:[{key,owner_agent_id,title,outcome,dependencies?:[assignment key]}]}. Installs goal/run/1–62 tasks atomically; returns goal_id,run_id,assignments key-to-node-ID map. All tasks start in execute phase with contract budget/evidence requirements. Does not start workers. Reuse the key only for an exact retry. For create_goal, omit goal_id and supply a stable idempotency_key; the runtime generates the identifier. Explicit identifiers require goal_, run_, or node_ prefixes, respectively. Every evidence_requirements item is an object with ALL four fields: kind (string), description (string), minimum_receipts (integer >=1), required (boolean); never pass strings or omit kind. inspect is read-only; reconcile advances readiness; tick claims within concurrency limits. resume/reroute require explicit recovery authority and a paused run; inspect the run first. They do not reopen an active or completed run."},
                "workflow_run_id": {"type": "string", "description": "Required workflow run identifier for inspect/reconcile/tick and resume/reroute. For resume/reroute, put it in this top-level field, not inside workflow_payload."},
                "workflow_worker_id": {"type": "string", "description": "Optional assertion of the current immutable coworker id for workflow_action=tick. It cannot select or impersonate another worker."},
                "workflow_payload": {"type": "object", "properties": {
                    "contract": {"type": "object", "properties": {
                        "title": {"type":"string"},
                        "objective": {"type":"string"},
                        "evidence_requirements": evidence_requirements.clone(),
                        "metadata": {"type":"object","additionalProperties":{"type":"string"}},
                        "ownership": {"type":"object","properties":{
                            "scope":{"type":"string","enum":["agent","group","company"]},
                            "owner_agent_id":{"type":"string","description":"Immutable active coworker ID."},
                            "group_id":{"type":["string","null"]}
                        },"description":"Omit for the caller's private scope. Group scope requires an active group_id and an owner who belongs to it; other scopes omit group_id."},
                        "budget": {"type":"object", "properties": {
                            "max_input_tokens":{"type":["integer","null"],"minimum":1},
                            "max_output_tokens":{"type":["integer","null"],"minimum":1},
                            "max_total_tokens":{"type":["integer","null"],"minimum":1},
                            "max_cost_micros":{"type":["integer","null"],"minimum":1},
                            "max_wall_seconds":{"type":["integer","null"],"minimum":1},
                            "max_iterations":{"type":["integer","null"],"minimum":1}
                        },"additionalProperties":false,"description":"Omitted dimensions are unbounded. Use the user's real limits; max_total_tokens, when supplied alongside input/output limits, must cover their sum."},
                        "concurrency": {"type":"object","properties": {
                            "max_queued_nodes":{"type":"integer","minimum":1,"maximum":10000},
                            "max_active_workers":{"type":"integer","minimum":1,"maximum":64},
                            "max_active_per_run":{"type":"integer","minimum":1,"maximum":64},
                            "max_claims_per_tick":{"type":"integer","minimum":1,"maximum":64},
                            "max_nodes_per_run":{"type":"integer","minimum":1,"maximum":50000}
                        },"required":["max_queued_nodes","max_active_workers","max_active_per_run","max_claims_per_tick","max_nodes_per_run"],"additionalProperties":false,
                        "description":"Optional. Omit to use scheduler defaults. If supplied, it is an object with all five fields, never a worker-count integer. Per-run and per-tick active limits cannot exceed max_active_workers."}
                    },"required":["title","objective","budget"],"additionalProperties":false},
                    "evidence_requirements": evidence_requirements,
                    "evidence": {"type":"array","maxItems":256,"items":{
                        "type":"object","properties":{
                            "receipt_id":{"type":"string","pattern":"^receipt_[A-Za-z0-9_-]*$","maxLength":256},
                            "kind":{"type":"string","minLength":1},
                            "summary":{"type":"string","minLength":1},
                            "uri":{"type":["string","null"]},
                            "content_hash":{"type":["string","null"]},
                            "verified":{"type":"boolean"},
                            "recorded_at":{"type":"string","description":"RFC3339 timestamp of the observation."}
                        },"required":["receipt_id","kind","summary","verified","recorded_at"]
                    },"description":"For transition: replaces the assignment's evidence list. Omit to retain recorded receipts. Only distinct verified receipts of the exact required kind count; a filename or claim alone is not verification."},
                    "phase": {"type": "string", "enum": ["prepare", "plan", "execute", "review", "commit"]},
                    "state": {"type": "string", "enum": ["pending", "ready", "leased", "running", "review", "waiting_user", "waiting_peer", "blocked", "succeeded", "failed", "canceled", "stale"], "description": "Durable workflow state; distinct from the top-level work status state. Use running during execution, review for review, succeeded only when commit evidence is satisfied."},
                    "restart_state": {"type": "string", "enum": ["fresh", "recovering", "requeued", "waiting", "terminal"]}
                }, "description": "Typed lifecycle payload. create_goal: GoalRequest {goal_id?, idempotency_key?, contract:{title,objective,budget,evidence_requirements?,concurrency?,metadata?,ownership?:{scope:agent|group|company,owner_agent_id,group_id?}}}; omitted ownership defaults to the calling agent's private scope. open_run: RunRequest {run_id?,idempotency_key?,goal_id,budget?,next_wake_at?}. define_node: NodeSpec {node_id?,owner_agent_id?,idempotency_key?,run_id,parent_id?,title,outcome,phase,dependencies?,budget?,evidence_requirements?}. Set owner_agent_id to the immutable coworker ID for each assigned task; only that active group member may claim it. Required for new group assignments; omission preserves pooled ownership only outside groups. heartbeat: LeaseToken {node_id,lease_id,worker_id,fencing_token}. wait_for: {lease:LeaseToken,receipt:{receipt_id,responder,question}} releases capacity until that exact receipt is answered; responder is an immutable coworker ID. Use ask_user for user questions; this tool does not create a popup. resolve_wait: {node_id,receipt_id,answer}; your authenticated identity must match the expected peer, and cannot impersonate the user. A resolved answer makes the same task ready, not completed. record_evidence: {lease:LeaseToken,receipt:{receipt_id,kind,summary,uri?,content_hash?,verified,recorded_at}}. transition: {node_id,idempotency_key,lease?,phase,state,restart_state:fresh|recovering|requeued|waiting|terminal,reason,next_wake_at?,result?,usage?,evidence?}. resume/reroute: {idempotency_key,reason,new_owner_agent_id?}; reroute requires the new active owner. Succeeded requires phase=commit and all required verified evidence. Reuse the same idempotency_key only when retrying the exact same mutation."},
                "node_id": {"type": "string", "minLength": 1, "description": "The work node id. Required for claim, join, status, and release. Omit entirely for a current-run inspect. Never substitute target_id."},
                "title": {"type": "string"},
                "outcome": {"type": "string", "description": "Observable result, not an activity."},
                "acceptance": {"type": "array", "items": {"type": "string"}},
                "dependencies": {"type": "array", "items": {"type": "string"}},
                "pattern": {"type": "string", "enum": ["split", "studio", "mob"]},
                "approach": {"type": "string", "description": "Distinct candidate approach."},
                "state": {"type": "string", "enum": ["proposed", "ready", "claimed", "active", "review_ready", "accepted", "rejected", "superseded", "waiting_user", "canceled"]},
                "reason": {"type": "string", "minLength": 1, "description": "Required for note and decide; optional explanation for status/release. Never substitute statement."},
                "focus": {"type": "object", "properties": {
                    "intent": {"type": "string"}, "evidence": {"type": "array", "items": {"type": "string"}},
                    "collaborators": {"type": "array", "items": {"type": "string"}},
                    "artifacts": {"type": "array", "items": {"type": "string"}},
                    "next_event": {"type": "string"}, "blockers": {"type": "array", "items": {"type": "string"}}
                }, "required": ["intent", "next_event"]},
                "path": {"type": "string", "description": "Workspace-relative artifact path; Phoenix hashes its bytes."},
                "source_artifacts": {"type": "array", "items": {"type": "string"}},
                "target_id": {"type": "string", "minLength": 1, "description": "Only for challenge or decide: the claim/artifact/decision being evaluated. It is NOT a work node id."}, "claim": {"type": "string"},
                "evidence": {"type": "array", "items": {"type": "string"}},
                "accepted_id": {"type": "string"}, "voters": {"type": "array", "items": {"type": "string"}},
                "after_seq": {"type": "integer"}
                ,"skill": {"type": "string"}, "checkpoint": {"type": "string"},
                "candidate_id": {"type": "string"},
                "learning_kind": {"type": "string", "enum": ["episodic_memory", "company_knowledge", "user_preference", "skill_patch", "regression_eval", "routing_prior", "collaboration_lesson", "tool_incident", "runtime_change"]},
                "statement": {"type": "string", "description": "Only for propose_learning. A company note uses reason instead."},
                "reviewers": {"type": "array", "items": {"type": "string"}},
                "canary_receipt": {"type": "string"}
            },
            "required": ["action"],
            "allOf": [
                {"if": {"properties": {"action": {"const": "propose"}}, "required": ["action"]}, "then": {"required": ["title", "outcome"]}},
                {"if": {"properties": {"action": {"enum": ["claim", "join"]}}, "required": ["action"]}, "then": {"required": ["node_id", "approach"]}},
                {"if": {"properties": {"action": {"const": "status"}}, "required": ["action"]}, "then": {"required": ["node_id", "state"]}},
                {"if": {"properties": {"action": {"const": "release"}}, "required": ["action"]}, "then": {"required": ["node_id"]}},
                {"if": {"properties": {"action": {"const": "publish"}}, "required": ["action"]}, "then": {"required": ["path"]}},
                {"if": {"properties": {"action": {"const": "challenge"}}, "required": ["action"]}, "then": {"required": ["target_id", "claim"]}},
                {"if": {"properties": {"action": {"const": "decide"}}, "required": ["action"]}, "then": {"required": ["target_id", "accepted_id", "reason"]}},
                {"if": {"properties": {"action": {"const": "note"}}, "required": ["action"]}, "then": {"required": ["reason"]}},
                {"if": {"properties": {"action": {"const": "skill_checkpoint"}}, "required": ["action"]}, "then": {"required": ["skill", "checkpoint"]}},
                {"if": {"properties": {"action": {"const": "propose_learning"}}, "required": ["action"]}, "then": {"required": ["learning_kind", "statement"]}},
                {"if": {"properties": {"action": {"const": "promote_learning"}}, "required": ["action"]}, "then": {"required": ["candidate_id", "reviewers", "canary_receipt"]}}
            ]
        }),
    }
}

fn routine() -> ToolDefinition {
    ToolDefinition {
        name: "routine".into(),
        description: "Use workflows the user explicitly taught Phoenix through the embedded browser. Search/list before improvising on a repeated browser-shaped job; begin_run loads the complete semantic playbook and records activation; complete_run records verified success or failure. A workflow that fails three consecutive times becomes needs_review: stop blind retries and ask whether to reteach, archive, or delete it. When the user asks to remove a workflow, do it yourself: archive hides it; delete (with its exact name) moves it into a 30-day recoverable deletion window. Sensitive parameters are vault/login placeholders and must never be passed through this tool as plaintext.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "search", "get", "begin_run", "complete_run", "archive", "delete"]},
                "name": {"type": "string", "description": "Exact visible workflow name; required for action=delete."},
                "routine_id": {"type": "string", "description": "Stable taught-workflow id returned by list/search."},
                "query": {"type": "string", "description": "Natural-language search phrase for action=search."},
                "success": {"type": "boolean", "description": "Required for complete_run; true only after the real outcome is verified."},
                "result": {"type": "string", "description": "Short evidence or exact blocker for complete_run; never include credentials."}
            },
            "required": ["action"]
        }),
    }
}

fn web_search() -> ToolDefinition {
    ToolDefinition {
        name: "web_search".into(),
        description: "Web search (Tavily/Exa/Brave/Firecrawl/Serper/SerpAPI/DuckDuckGo) for live/current/trending/external facts. For two or more independent discovery lanes, send `queries` once (up to 8): Phoenix runs them concurrently and deduplicates URLs. Use singular `query` only for one search. Then web_fetch the strongest URLs for full content. Never spend one tool call per account/date when one batch covers them; never repeat a query already run.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "One search query. Do not combine with queries."},
                "queries": {"type": "array", "minItems": 1, "maxItems": 8, "items": {"type": "string"}, "description": "Independent search queries to execute concurrently in this ONE call. Preferred for multi-source/date/account discovery."},
                "limit": {"type": "integer", "minimum": 1, "maximum": 20, "description": "Optional result count per query. Batched searches cap each lane at 10 to keep the result bounded."},
                "recency_days": {"type": "integer", "minimum": 1, "maximum": 365, "description": "For current/news tasks, enforce a provider freshness window. Use 1 for today, 3 for 72 hours, 7 for a week. Still verify exact publication timestamps on the source."}
            },
            "anyOf": [
                {"required": ["query"]},
                {"required": ["queries"]}
            ]
        }),
    }
}

fn transcribe_audio() -> ToolDefinition {
    ToolDefinition {
        name: "transcribe_audio".into(),
        description: "Transcribe the speech in a local audio or video file (ogg, mp3, m4a, wav, webm, mp4…) to text with the configured speech-to-text model (Groq Whisper by default). When: voice notes, recordings, meetings, podcasts, video narration. Max 25 MB per file; split longer recordings with ffmpeg first.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Audio or video file, workspace-relative or absolute."}
            },
            "required": ["path"]
        }),
    }
}

fn web_fetch() -> ToolDefinition {
    ToolDefinition {
        name: "web_fetch".into(),
        description: "Fetch one HTML, XML, JSON, or text URL and extract bounded readable content. When: have URL; need page, sitemap, feed, or API content. When NOT: replace web_search for discovery; binary files; huge unscoped crawl.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "max_chars": {"type": "integer", "description": "Default 8000."}
            },
            "required": ["url"]
        }),
    }
}

fn web_scrape() -> ToolDefinition {
    ToolDefinition {
        name: "web_scrape".into(),
        description: "Scrape one URL via configured scrape provider. When: JS-heavy page; profile.scrape set. When NOT: simple fetch (try web_fetch first).".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "max_chars": {"type": "integer", "description": "Default 8000."}
            },
            "required": ["url"]
        }),
    }
}

fn web_crawl() -> ToolDefinition {
    ToolDefinition {
        name: "web_crawl".into(),
        description: "Crawl from seed URL (Firecrawl/Tavily). When: small site/docs section with scoped limit. When NOT: single page (web_fetch); unbounded sites.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "limit": {"type": "integer", "description": "Max pages, default 10."},
                "max_chars_per_page": {"type": "integer", "description": "Default 6000."}
            },
            "required": ["url"]
        }),
    }
}

fn ask_user() -> ToolDefinition {
    ToolDefinition {
        name: "ask_user".into(),
        description: "Ask the user a question that pops up after your reply. Call it LAST: first do what you can without the answer and write your reply (what you did, what you found, why you need this), then ask and stop. Never assume the answer or perform its gated action, and never describe that action as already done in your reply. The user's response returns as a durable continuation in this conversation. When: real ambiguity, an approval gate, a user-fixable blocker, or a browser workflow the user should demonstrate. When NOT: decisions you can make, progress updates, or lazy routing. Give short options; the user can always type their own. For ordinary questions or approval to send/post a draft, omit approval metadata and put the entire exact draft and destination in question. Never use teach_workflow for posting approval. Typed approvals unlock purpose-built UI: permanent_agent gates a real hire; teach_workflow opens this coworker's embedded private browser, records the demonstration, saves the routine, and resumes through the answer continuation. The approved option must appear exactly in the choices.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": {"type": "string"},
                            "header": {"type": "string"},
                            "options": {"type": "array", "items": {"type": "string"}},
                            "multi_select": {"type": "boolean"}
                        },
                        "required": ["question"]
                    }
                },
                "approval": {
                    "type": "object",
                    "description": "Machine-readable approval or purpose-built user flow.",
                    "properties": {
                        "action": {"type": "string", "enum": ["permanent_agent", "teach_workflow"]},
                        "subject": {"type": "string", "description": "Exact role, capability, or workflow goal being presented."},
                        "approved_option": {"type": "string", "description": "Exact option text that counts as approval."},
                        "details": {
                            "type": "object",
                            "description": "For teach_workflow: owner_agent_id, workflow_goal, optional start_url, and scope (agent, group, or company).",
                            "additionalProperties": {"type": "string"}
                        }
                    },
                    "required": ["action", "subject", "approved_option"]
                }
            },
            "required": ["questions"]
        }),
    }
}

fn teach_workflow() -> ToolDefinition {
    ToolDefinition {
        name: "teach_workflow".into(),
        description: "Invite the user to demonstrate a repeatable browser workflow. This is Phoenix's FIRST-CLASS teaching action: it pings the user, opens a typed card above the composer, and—when they choose Teach now—opens this exact coworker's private embedded browser, records semantic steps, asks for a name and availability, saves the workflow, then resumes this same turn. Use immediately when the user offers to teach you or when they explicitly ask to demonstrate a browser task. Do not substitute a generic ask_user question, describe instructions, or operate the site yourself before the demonstration. Use routine after the saved-workflow receipt returns.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "workflow_goal": {"type": "string", "description": "Plain-language repeatable outcome the user will demonstrate, such as 'generate an image in ChatGPT'."},
                "start_url": {"type": "string", "description": "Optional http(s) page to open at the start of teaching."},
                "scope": {"type": "string", "enum": ["agent", "group", "company"], "default": "agent", "description": "Initial availability of the saved workflow. Use agent unless the user already requested broader reuse."}
            },
            "required": ["workflow_goal"]
        }),
    }
}

fn cron() -> ToolDefinition {
    ToolDefinition {
        name: "cron".into(),
        description: "Schedule wake-ups in a specific Phoenix conversation. create: at the scheduled time your prompt arrives in that conversation as a user message and you act on it. When: the user asks for reminders, monitoring, or recurring checks. When NOT: one-off work you can do now.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["create", "list", "delete"]},
                "when": {"type": "string", "description": "create: `every 10m` | `daily 09:30` | `in 45m` | `at 2026-06-12T08:00` (local time)"},
                "prompt": {"type": "string", "description": "create: the message the session wakes up to — write it as the instruction to execute then."},
                "id": {"type": "string", "description": "delete: cron id (prefix ok)"},
                "canvas": {"type": "string", "description": "create: target Phoenix conversation id; defaults to the current conversation"}
            },
            "required": ["action"]
        }),
    }
}

fn create_agent() -> ToolDefinition {
    ToolDefinition {
        name: "create_agent".into(),
        description: "Request a NEW permanent responsibility owner — a rare, deliberate hire, not temporary capacity. Call this tool directly with the proposed human name, role, and responsibility; the runtime itself presents one typed approval card and creates a role-scoped, session-scoped, single-use receipt. Do NOT ask for permanent-hire approval separately. Choose a human name and durable outcome/domain, never an app-bound 'Notion bot' or a duplicate generic tool agent. This records a tiny restart-safe Setting up entry. Phoenix MUST immediately follow it with agent_provision; there is no research/exam wizard.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "role": {"type": "string", "description": "Role name, lowercase snake_case — becomes the dir and the talk target (e.g. 'trader')."},
                "persona": {"type": "string", "description": "Display persona name (e.g. 'Ledger'). Optional; defaults to capitalized role."},
                "description": {"type": "string", "description": "One line: the domain this agent OWNS (shows in rosters)."},
                "mission": {"type": "string", "description": "Why it is being created — the user request that triggered it, recorded in the scaffold."}
            },
            "required": ["role", "description"]
        }),
    }
}

fn agent_provision() -> ToolDefinition {
    ToolDefinition {
        name: "agent_provision".into(),
        description: "Phoenix-only finalization for a coworker just requested with create_agent. Read the current company roster/responsibilities and use your recalled understanding of the user before calling. Define what this person owns, when they quietly consult or route to another owner, how success is judged, and the durable domain knowledge that makes them better than a generic assistant. Every coworker already receives every Phoenix tool; never create a tool allowlist or app silo. This atomically publishes the system prompt and makes the coworker immediately reachable. Specialists must ask Phoenix to perform this step.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "agent": {"type": "string", "description": "Requested lowercase role id."},
                "persona": {"type": "string", "description": "Warm human display name."},
                "role_title": {"type": "string", "description": "Human responsibility title, not an app name."},
                "description": {"type": "string", "description": "One concise line describing the outcome/domain this coworker owns."},
                "system_prompt": {"type": "string", "description": "Role doctrine: ownership boundaries, judgment, recurring duties, collaboration, escalation, success, and domain practices. Do not repeat Phoenix's universal tool/security contract."},
                "knowledge": {"type": "array", "items": {"type": "string"}, "description": "Small durable role-specific facts/guidelines; omit generic filler and secrets."},
                "color": {"type": "string", "description": "Optional CSS hex identity color."},
                "icon_seed": {"type": "string", "description": "Optional stable seed for the playful eyes/flame SVG."}
            },
            "required": ["agent", "persona", "role_title", "description", "system_prompt"]
        }),
    }
}

fn tools_create() -> ToolDefinition {
    ToolDefinition {
        name: "tools_create".into(),
        description: "Build a NEW capability as a local MCP server inside a coworker's tools/ dir when the capability is absent from Phoenix's complete shared catalog. Prefer, in order: Composio, a vendor-provided MCP server, then a small local public-API wrapper. action:\"create\" writes and registers a zero-dependency Python stdio server and live-probes its tool list. The coding coworker implements its handlers; action:\"test\" re-probes and, with call_tool + call_arguments, performs a REAL invocation using the registered environment. A capability is done only on CALL PASS with real output. Acquire accounts through ask_for_login, the private browser, the encrypted vault, and account_manage. Every coworker already receives mcp_servers and mcp_call, so no per-agent tool grant is needed.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "agent": {"type": "string", "description": "Agent role whose tools/ dir hosts the server ('trader')."},
                "name": {"type": "string", "description": "Server name, snake_case ('market_data'). Registered as <agent>-<name>."},
                "description": {"type": "string", "description": "One line: what the capability does (shows in MCP discovery)."},
                "action": {"type": "string", "enum": ["create", "test", "set_env"], "description": "create = scaffold+register+probe (default); test = re-probe an existing server, optionally firing a real call with the registered env; set_env = merge `env` (API keys) into the registration."},
                "env": {"type": "object", "description": "Environment for the server process (API keys, endpoints): {\"POLYMARKET_API_KEY\": \"…\"}. Stored in the registration; rides every run."},
                "tools": {"type": "array", "items": {"type": "object", "properties": {"name": {"type": "string"}, "description": {"type": "string"}, "params": {"type": "object", "description": "param name → one-line description (all string params)"}}, "required": ["name", "description"]}, "description": "create: the tools the server exposes."},
                "call_tool": {"type": "string", "description": "test: tool name to actually invoke."},
                "call_arguments": {"type": "object", "description": "test: real arguments for call_tool."}
            },
            "required": ["agent", "name"]
        }),
    }
}

fn reverse_skill() -> ToolDefinition {
    ToolDefinition {
        name: "reverse_skill".into(),
        description: "Durable, evidence-gated reverse-skill pipeline. observe reads successful Phoenix run traces and matching verified company jobs plus content-addressed artifacts; repeated patterns need three runs unless the user explicitly requested extraction. propose records LearningProposed. review requires two distinct non-producer DecisionRecorded receipts. canary requires three distinct held-out successful runs on the same route plus a real all-pass bench section. publish validates strict SKILL.md frontmatter and atomically installs under Phoenix home; rollback restores the prior version. Never invent receipts or use synthetic runs.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["observe", "status", "propose", "review", "canary", "publish", "rollback"]},
                "candidate_id": {"type": "string"},
                "name": {"type": "string", "description": "observe: lowercase letters, digits, and hyphens only."},
                "description": {"type": "string", "description": "observe: concise trigger-oriented skill description."},
                "run_ids": {"type": "array", "items": {"type": "string"}, "description": "observe: successful real Phoenix runs; empty performs a bounded trace scan."},
                "explicit_request": {"type": "boolean", "description": "observe: true only when the user explicitly asked for extraction; it bypasses repetition count, never evidence gates."},
                "reviewers": {"type": "array", "items": {"type": "object", "properties": {
                    "reviewer_id": {"type": "string"},
                    "decision_id": {"type": "string"}
                }, "required": ["reviewer_id"]}},
                "canary": {"type": "object", "properties": {
                    "run_ids": {"type": "array", "items": {"type": "string"}, "minItems": 3},
                    "bench": {"type": "object", "properties": {
                        "path": {"type": "string", "description": "Workspace-relative real bench report."},
                        "label": {"type": "string", "description": "Exact ## heading in the report."},
                        "min_tasks": {"type": "integer", "minimum": 1}
                    }, "required": ["label"]}
                }, "required": ["run_ids", "bench"]}
            },
            "required": ["action"]
        }),
    }
}

fn vital_memory_write() -> ToolDefinition {
    ToolDefinition {
        name: "vital_memory_write".into(),
        description: "Save ONE durable fact about the user to Phoenix's always-on vital memory (VITALS.md) — it is injected into EVERY future turn's context, all sessions. When: the user states a standing preference, a goal, a hard 'don't', or a correction worth honoring forever (e.g. 'I mean the AI model, not video games'). NOT for: task details, one-off facts, anything tied to a single project (the knowledge-graph memory picks those up automatically), or routine chatter. Keep it rare and short. Use `replaces` to supersede an entry the user just changed.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "note": {"type": "string", "description": "The durable fact, ONE short line."},
                "category": {"type": "string", "enum": ["preference", "goal", "avoid", "instruction"], "description": "preference = what they like; goal = what they're working toward; avoid = a hard don't; instruction = a standing 'always do this'."},
                "replaces": {"type": "string", "description": "Optional existing entry to supersede when the user corrects a fact. Case-insensitive exact text takes priority; otherwise a substring must match exactly one entry. Empty, missing or ambiguous matches fail without changing memory. Correction history remains inspectable in VITALS.md, outside prompt context."}
            },
            "required": ["note", "category"]
        }),
    }
}

fn recall() -> ToolDefinition {
    ToolDefinition {
        name: "recall".into(),
        description: "Recover this session's compacted-out history. Use only for a specific fact actually missing from the current context. Do not search visible context, repeat a successful search, or retry a no-match query without new evidence. SEARCH: set query to search terms and line/offset to null. To audit failed tools, set failed_only:true and optionally tool_prefix (for example computer_); query may be empty to list all matching failures. Filters inspect structured tool names/success flags, not words in the text. EXACT READ: clear tool_prefix/failed_only, set line to a returned archive line, query to an empty string, and offset to zero or returned next_offset. Continue pages until next_offset is null. Never guess a line number while searching. Excerpts are not full records; historical claims still need evidence and may be superseded by corrections.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "The fact/finding/value to pull back, in its own words — e.g. 'the postgres DSN', 'what the researcher found about the rate limit'."},
                "limit": {"type": ["integer", "null"], "description": "Max search matches (null defaults to 5, max 15)."},
                "tool_prefix": {"type":["string","null"],"description":"Optional structured tool-name prefix such as computer_ or bash. Null for unfiltered search or exact reads."},
                "failed_only": {"type":["boolean","null"],"description":"True selects failed tool results only. Null for ordinary search and exact reads."},
                "line": {"type": ["integer", "null"], "minimum": 1, "description": "Null to search. Otherwise the one-based archive line returned by search, selecting exact read."},
                "offset": {"type": ["integer", "null"], "minimum": 0, "description": "Null when searching. For exact read, character offset zero or the previous next_offset. Requires a non-null line."}
            },
            "required": []
        }),
    }
}

fn memory_recall() -> ToolDefinition {
    ToolDefinition {
        name: "memory_recall".into(),
        description: "Search long-term memory for remembered facts, decisions, preferences, and outcomes. Scope: YOUR OWN memory + the shared TEAM tier by default — each agent has its own memory lane; team notes are shared by everyone. Notes carry a provenance stamp ([from Leo (coder) — …]): attribute facts to whoever learned them, they are not automatically your own history. When: the answer genuinely depends on missing history — a name you half-recognize, a prior decision, a user preference, an earlier fix — search memory BEFORE guessing or asking. When NOT: greetings, acknowledgements, casual conversation, status confirmation, a fact the user just supplied, anything visible in this conversation, or THIS session's compacted history (use `recall`). Local/free avoids provider tokens but still costs a tool round; never call it merely to prove activity. Deeper search types (cost one cheap librarian-model call): GRAPH_COMPLETION composes an answer over graph relationships; TRIPLET_COMPLETION reasons over entity triplets; SUMMARIES returns condensed summaries; TEMPORAL for time-anchored questions. Start with the free default; escalate only when raw chunks didn't connect the dots.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "What you are trying to remember, phrased as the fact you want — e.g. 'how browser cookie porting was fixed', 'the user's preference for session naming'."},
                "search_type": {"type": "string", "description": "CHUNKS (default, local/free), GRAPH_COMPLETION, TRIPLET_COMPLETION, SUMMARIES, TEMPORAL, CHUNKS_LEXICAL."},
                "top_k": {"type": "integer", "description": "Max chunks to return (default 8)."},
                "all_agents": {"type": "boolean", "description": "Optional. True searches EVERY agent's memory, not just yours + team — for 'did anyone on the team ever…' questions. Default false."}
            },
            "required": ["query"]
        }),
    }
}

fn memory_save() -> ToolDefinition {
    ToolDefinition {
        name: "memory_save".into(),
        description: "Write one durable note into long-term memory, retrievable by memory_recall in every future session. Scope: specialists save into their OWN memory lane; the orchestrator saves TEAM-WIDE by default (he stewards the shared tier). A specialist who believes the WHOLE TEAM should remember something tells Phoenix via talk — Phoenix saves it team-wide; `scope:\"team\"` is honored only for the orchestrator. When: the moment you learn something worth keeping — a fix or gotcha discovered the hard way, a user correction or preference, a decision plus its why, a fact about the user's world or projects. Small is fine; one or two sentences is the right size. Write it BEFORE finalizing, not 'later'. When NOT: things visible in the repo/code itself, session-local details with no future value, or vital always-on facts (use vital_memory_write for those). Phrase the note so a stranger could act on it: name the thing, state the fact, include the why. Set `procedure:true` when the note is a REUSABLE, step-by-step method you worked out (how to drive a specific site's flow, a build/deploy sequence, a data pipeline) — the maintenance pass will vet it and, if it is a genuine method rather than a workaround, draft it into a loadable skill for future runs. Write those steps concretely, naming the actual tools you used.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "note": {"type": "string", "description": "The durable fact, self-contained in 1-3 sentences. E.g. 'The user prefers PR reviews posted as a single summary comment, not inline comments — they said inline feels noisy.'"},
                "scope": {"type": "string", "description": "Optional: \"mine\" (your own memory — specialist default) or \"team\" (shared tier — orchestrator default; only the orchestrator may set it)."},
                "procedure": {"type": "boolean", "description": "Optional. Set true when the note is a reusable, step-by-step method worth turning into a skill (the maintenance pass vets it, then drafts a SKILL.md for genuine methods). Default false — an ordinary fact or preference is not a procedure."}
            },
            "required": ["note"]
        }),
    }
}

fn react() -> ToolDefinition {
    ToolDefinition {
        name: "react".into(),
        description: "Acknowledge the user's message with ONE emoji instead of writing a reply. \
Use ONLY when a written answer would add nothing: a small courteous request you are about to do \
anyway (\"also add a button here, thanks\"), a thanks, or a confirmation that needs no detail. \
This is uncommon — reach for it a small fraction of the time, never as a habit and never twice in \
a row. NEVER use it when the user asked a question, when anything failed, when you are reporting \
what you did, or when a decision or caveat needs stating; those need real prose. Reacting is not a \
substitute for doing the work — call this only alongside actually doing it, never instead. \
Allowed: 👍 👀 🔥 ✅ 🎉 🙏 😄 🤔"
            .into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "emoji": {
                    "type": "string",
                    "enum": ["👍", "👀", "🔥", "✅", "🎉", "🙏", "😄", "🤔"],
                    "description": "The single reaction emoji."
                }
            },
            "required": ["emoji"]
        }),
    }
}

fn todo_write() -> ToolDefinition {
    ToolDefinition {
        name: "todo_write".into(),
        description: "Compact live map for substantive execution. Use near the start when work has multiple meaningful steps, surfaces, deliverables, or verification gates; keep 3–7 short outcome-shaped items and batch status updates as evidence arrives. Mark the current step in_progress with a progress estimate. Skip conversation, quick answers, one-action changes, and simple checks that finish in a handful of calls. Never create or tick boxes as planning theater.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "task": {"type": "string"},
                            "completed": {"type": "boolean"},
                            "status": {"type": "string", "enum": ["pending", "in_progress", "completed", "cancelled"], "description": "Mark the step you are working on in_progress; cancelled for dropped steps."},
                            "progress": {"type": "integer", "minimum": 0, "maximum": 100, "description": "Your honest estimate of how far the in_progress step is (0–100). The user sees it as a ring; raise it as real evidence lands, never to look busy."},
                            "detail": {"type": "string", "description": "Optional few words shown beside the step, e.g. \"3 of 5 pages\"."}
                        },
                        "required": ["task"]
                    }
                }
            },
            "required": ["todos"]
        }),
    }
}

// === Browser tools (donor: browser-use action registry, 1:1) ===
// Executed natively over CDP (tools::browser_native); every page-affecting
// result returns the fresh indexed browser state. Only elements with a
// numeric [index] are actionable.

fn browser_tool_definition(name: &str) -> Option<ToolDefinition> {
    let (description, parameters) = match name {
        "browser_console" => (
            "Read the page's captured console output and JS exceptions (CDP-fed ring buffer, since the browser session opened). When: after loading a page YOU built or changed — a clean console is part of 'it works'; and when a page misbehaves, the error is usually here. When NOT: pages you are merely reading.",
            serde_json::json!({"type": "object", "properties": {}}),
        ),
        "browser_act" => (
            "Run a SEQUENCE of browser actions in ONE call against the current page state — the browser-use multi-act pattern, the single biggest speed lever on this surface (one model round instead of one per action). Each item: {\"action\": name, …that action's params}. Batchable: navigate, search, go_back, wait, click, input, send_keys, scroll, find_text, search_page, dropdown_options, select_dropdown, upload_file. Runs in order with stale-DOM guards: navigate/search/go_back load a new document and TERMINATE the sequence (put them last); any unexpected page navigation aborts the rest; the first failure stops the batch and reports the exact step. Fresh page state attaches ONCE at the end. e.g. fill-and-submit = [{\"action\":\"input\",\"index\":3,\"text\":\"…\"},{\"action\":\"input\",\"index\":5,\"text\":\"…\"},{\"action\":\"click\",\"index\":7}]. When: any known multi-step sequence on the SAME state — form fills, type+enter, scroll+read. When NOT: a step whose target depends on the previous step's result (state between them), or the first action on an unfamiliar page.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "actions": {"type": "array", "description": "Ordered actions; each an object with `action` and that action's params (same params as the standalone browser_* tools).", "items": {"type": "object", "properties": {
                        "action": {"type": "string", "enum": ["navigate", "search", "go_back", "wait", "click", "input", "send_keys", "scroll", "find_text", "search_page", "dropdown_options", "select_dropdown", "upload_file"]},
                        "url": {"type": "string"}, "query": {"type": "string"}, "seconds": {"type": "integer"},
                        "index": {"type": "integer"}, "text": {"type": "string"}, "clear": {"type": "boolean"},
                        "keys": {"type": "string"}, "down": {"type": "boolean"}, "pages": {"type": "number"},
                        "pattern": {"type": "string"}, "path": {"type": "string"}
                    }, "required": ["action"]}}
                },
                "required": ["actions"]
            }),
        ),
        "browser_navigate" => (
            "Open a URL in the browser. When: you know the target URL. When NOT: you only have a vague topic (use browser_search). Set new_tab=true for side research so you keep your place.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "Absolute URL to open."},
                    "new_tab": {"type": "boolean", "description": "Open in a new tab (default false)."}
                },
                "required": ["url"]
            }),
        ),
        "browser_search" => (
            "Search the web inside the browser (search engine results page). When: you need to find the right page to operate on. When NOT: you already know the URL (browser_navigate).",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query — keep it short (<5 words)."}
                },
                "required": ["query"]
            }),
        ),
        "browser_go_back" => (
            "Go back one page in browser history. When: a click/navigation led somewhere wrong.",
            serde_json::json!({"type": "object", "properties": {}}),
        ),
        "browser_import_cookies" => (
            "Instantly import ONLY one site's portable cookies from a supported local browser into your private browser and save who may receive future refreshes. Use scope=agent by default; group only inside that group conversation; company only when the login is intentionally shared. Device-bound Google/Microsoft sessions are never copied—use ask_for_login so the user can sign in inside Phoenix. This imports cookies only, never saved passwords, history, localStorage, or unrelated sites.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "source": {"type": "string", "enum": ["firefox", "zen", "librewolf", "floorp", "waterfox", "chrome", "chromium", "brave", "edge", "vivaldi"]},
                    "site": {"type": "string", "description": "Exact site/domain to import, e.g. github.com."},
                    "scope": {"type": "string", "enum": ["agent", "group", "company"], "description": "Who may receive refreshes; default agent."}
                },
                "required": ["source", "site"]
            }),
        ),
        "browser_wait" => (
            "Wait for the page to settle. When: content is still loading or after an action that triggers async updates. Keep waits short.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "seconds": {"type": "integer", "description": "Seconds to wait (default 3, max 10)."}
                }
            }),
        ),
        "browser_click" => (
            "Click an interactive element by its numeric [index] from the browser state. When: the index is visible in the CURRENT state. When NOT: the index is from an older page state (get fresh state first); never invent indexes. Fallback: pass x+y viewport coordinates instead of index for canvas/unindexed regions only.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "index": {"type": "integer", "description": "Element index from the current browser state."},
                    "new_tab": {"type": "boolean", "description": "Open the link in a new tab (links only)."},
                    "x": {"type": "number", "description": "Viewport x — coordinate fallback when no index exists."},
                    "y": {"type": "number", "description": "Viewport y — coordinate fallback when no index exists."}
                }
            }),
        ),
        "browser_upload_file" => (
            "Attach a local file to a file input by [index]. When: a form needs a file and the state shows an <input type=file>. The path must exist on disk.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "index": {"type": "integer", "description": "File-input element index from the current browser state."},
                    "path": {"type": "string", "description": "Absolute path of the file to attach."}
                },
                "required": ["index", "path"]
            }),
        ),
        "browser_input" => (
            "Type text into an input/textarea by [index]. When: form filling. After typing, watch for autocomplete suggestions (new *[index] elements) — click the right one instead of pressing Enter. Set clear=false to append.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "index": {"type": "integer", "description": "Input element index from the current browser state."},
                    "text": {"type": "string"},
                    "clear": {"type": "boolean", "description": "Clear the field first (default true)."}
                },
                "required": ["index", "text"]
            }),
        ),
        "browser_input_credential" => (
            "Fill a password/secret field from Phoenix's encrypted vault by credential id. The secret is typed directly into this coworker's browser and NEVER appears in model context, traces, page state, or tool output. The credential must be visible in the current private/group/company scope and bound to the current site. Get its id with credential_list; use the CURRENT field [index]. A stale, remounted, or non-retained field is a browser-state failure—not an invalid password: refresh browser_state and retry this secure fill once. Use ask_for_login only after the SITE rejects the submitted credential or a genuine user-only MFA/passkey step is required; never ask the user to re-enter a password Phoenix can already use.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "index": {"type": "integer", "description": "Secret/password input element index from current browser state."},
                    "credential_id": {"type": "string", "description": "Opaque id returned by credential_list or credential_generate."},
                    "field": {"type": "string", "description": "Optional secret field: password (default), username, totp (current 2FA code), number, cvc, expiry, exp_month, exp_year, name, billing_zip, key, value, code."}
                },
                "required": ["index", "credential_id"]
            }),
        ),
        "browser_send_keys" => (
            "Send one special keyboard key or shortcut to the page (e.g. Enter, Escape, Tab, Control+a). This never types prose or printable characters: use browser_input with the focused field's current index for ALL text, including one character, spaces, and punctuation. When: submitting after input, dismissing dialogs, keyboard-driven UIs.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "keys": {"type": "string", "description": "Key or combo, e.g. 'Enter', 'Escape', 'Control+a'."}
                },
                "required": ["keys"]
            }),
        ),
        "browser_scroll" => (
            "Scroll the page or a scrollable container. When: target content is below the fold. When NOT: hunting for specific text (browser_search_page is free and instant).",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "down": {"type": "boolean", "description": "true = down, false = up (default true)."},
                    "pages": {"type": "number", "description": "How many viewport-heights to scroll (default 1)."},
                    "index": {"type": "integer", "description": "Optional index of a scrollable container."}
                }
            }),
        ),
        "browser_find_text" => (
            "Scroll directly to the first occurrence of the given text on the page. When: you know exact visible text to reach.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string"}
                },
                "required": ["text"]
            }),
        ),
        "browser_search_page" => (
            "Find text/regex on the current page — free and instant. When: verifying content exists, locating prices/dates/IDs/errors before acting. Prefer this over scrolling or browser_extract.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Text or regex pattern to find."}
                },
                "required": ["pattern"]
            }),
        ),
        "browser_find_elements" => (
            "Query the DOM with a CSS selector — free and instant. When: counting items (rows, cards), collecting links/attributes, understanding structure before extracting.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "selector": {"type": "string", "description": "CSS selector."},
                    "limit": {"type": "integer", "description": "Max elements to return."}
                },
                "required": ["selector"]
            }),
        ),
        "browser_extract" => (
            "Extract information from the WHOLE page (including off-screen and same-origin iframes). With `query`, a sidecar model structures exactly what you asked from the full text (preferred). Without it you get a bounded raw text dump. EXPENSIVE — when search_page/find_elements can't get it; never repeat the same query on an unchanged page.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What to extract, specific and complete — enables structured extraction."},
                    "extract_links": {"type": "boolean", "description": "Include hyperlinks (default false)."}
                }
            }),
        ),
        "browser_screenshot" => (
            "Capture a screenshot (saved to a file path). When: you are unsure about visual state, or need ground truth to verify an action's effect.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "full_page": {"type": "boolean", "description": "Capture the full page instead of the viewport."}
                }
            }),
        ),
        "browser_save_as_pdf" => (
            "Save the current page as a PDF file. When: the user wants a document artifact of a page.",
            serde_json::json!({"type": "object", "properties": {}}),
        ),
        "ui_snap" => (
            "Screenshot ANY URL with your OWN throwaway headless browser — isolated and instant. When: you just changed UI code and the dev server is running — snap it, then image_analyze the file to judge it against the brief and the slop test (this look-and-score loop is MANDATORY before finishing design work; a design final without it bounces). Snap desktop (default 1440px) AND phone (width: 390) for responsive work. When NOT: pages needing your durable logged-in profile (use your normal browser tools).",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "Page to capture, e.g. http://localhost:3001 (bare host:port gets http://)."},
                    "path": {"type": "string", "description": "Workspace-relative output path. Default artifacts/ui-snaps/<ts>.png."},
                    "full_page": {"type": "boolean", "description": "Capture full page height. Default false."},
                    "wait_ms": {"type": "integer", "description": "Wait after navigation before capture (render/animations). Default 1500."},
                    "width": {"type": "integer", "description": "Viewport width. Default 1440; use 390 for phone."},
                    "height": {"type": "integer", "description": "Viewport height. Default 900."}
                },
                "required": ["url"]
            }),
        ),
        "browser_download" => (
            "Download a URL to a file THROUGH the page's session (in-page fetch with this tab's cookies/auth → real bytes on disk). A downloaded image is shown to you in the same step (no separate image_analyze call needed just to see it). THE ONLY WAY to save a file from the browser: native downloads (a.click(), blob anchors, the site's own 'Download' button) are SILENTLY DROPPED in headless chrome — never hunt for download buttons and never expect a.click() to save anything. When: a generated image/video/file on a logged-in site (signed CDN URLs, chatgpt/dreamina asset URLs), any file behind the page's auth. Get the asset URL from the DOM (browser_evaluate on img.src / video.src), then one call.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "The file URL — fetched with this page's cookies, so signed/authenticated URLs work."},
                    "path": {"type": "string", "description": "ABSOLUTE destination path (e.g. /home/user/project/frontend/public/hero.png). Omit to save into the browser artifacts dir."}
                },
                "required": ["url"]
            }),
        ),
        "browser_dropdown_options" => (
            "List a dropdown's options by [index]. Native <select> controls are read directly; ARIA/custom dropdowns are opened and their visible options are read. When: before selecting, to see exact option text. If the result says the page exposes no option list, use the fresh attached state and click the visible option directly instead of retrying.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "index": {"type": "integer"}
                },
                "required": ["index"]
            }),
        ),
        "browser_select_dropdown" => (
            "Select a native or ARIA/custom dropdown option by exact visible text. When: after browser_dropdown_options showed the choices. Phoenix opens custom dropdowns automatically; if the page uses an inaccessible bespoke menu, follow the returned fresh state and click the visible option directly.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "index": {"type": "integer", "description": "Dropdown element index."},
                    "text": {"type": "string", "description": "Exact option text to select."}
                },
                "required": ["index", "text"]
            }),
        ),
        "browser_switch" => (
            "Switch to another open tab by its id (shown in browser state). When: research in a side tab, or returning to your main tab.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "tab_id": {"type": "string", "description": "Tab id from the browser state (last 4 chars ok)."}
                },
                "required": ["tab_id"]
            }),
        ),
        "browser_close" => (
            "Close a tab by its id. When: done with a side tab. Never close your last tab.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "tab_id": {"type": "string"}
                },
                "required": ["tab_id"]
            }),
        ),
        "browser_evaluate" => (
            "Run JavaScript on the page and return the result. LAST RESORT — and the most expensive habit on this surface: hand-writing a fresh scrape script every round is kilobytes of output per call and the #1 cause of slow browser turns. Content questions are ONE cheap call elsewhere: `browser_extract` (ask in plain English), `browser_find_text`/`browser_search_page` (locate), `browser_state` (structure + indexes). When: genuinely no other tool can do it (custom widgets, shadow-DOM tricks) — and if you must, reuse your previous script pattern instead of rewriting it. It can mutate the DOM — never assume old indexes survive it.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "code": {"type": "string", "description": "JavaScript to evaluate; return a JSON-serializable value."}
                },
                "required": ["code"]
            }),
        ),
        "browser_status" => (
            "Inspect browser/CDP health: launched vs attached mode, current URL, tab count, attach endpoint or fallback note, profile dir, binary, login source, and profile-holder process info. When: browser behavior seems stuck/wrong, after attach-port changes, or before debugging login inheritance. When NOT: routine page reading (use browser_state).",
            serde_json::json!({"type": "object", "properties": {}}),
        ),
        "browser_state" => (
            "Get the fresh browser state: URL, open tabs, and interactive elements with [index] markers. When: starting on a page, after anything ambiguous, or when your indexes feel stale. Cheap — when in doubt, refresh.",
            serde_json::json!({"type": "object", "properties": {}}),
        ),
        _ => return None,
    };
    Some(ToolDefinition {
        name: name.into(),
        description: description.into(),
        parameters,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn work_workflow_states_match_the_durable_parser() {
        let definition = super::work();
        let nested = &definition.parameters["properties"]["workflow_payload"]["properties"];
        let states = nested["state"]["enum"].as_array().unwrap();
        assert_eq!(states.len(), 12);
        for value in states {
            serde_json::from_value::<crate::runtime::workflow::WorkflowNodeState>(value.clone()).unwrap();
        }
        assert!(!states.contains(&serde_json::json!("active")));
        assert!(states.contains(&serde_json::json!("running")));
        for value in nested["phase"]["enum"].as_array().unwrap() {
            serde_json::from_value::<crate::runtime::workflow::WorkflowPhase>(value.clone()).unwrap();
        }
        for value in nested["restart_state"]["enum"].as_array().unwrap() {
            serde_json::from_value::<crate::runtime::workflow::RestartState>(value.clone()).unwrap();
        }
        // Existing shared-work status calls use their own state vocabulary.
        assert!(definition.parameters["properties"]["state"]["enum"].as_array().unwrap().contains(&serde_json::json!("active")));
    }

    #[test]
    fn advertised_workflow_concurrency_can_be_submitted_to_the_real_parser() {
        let definition = super::work();
        let schema = &definition.parameters["properties"]["workflow_payload"]["properties"]["contract"]["properties"]["concurrency"];
        assert_eq!(schema["type"], "object");
        let policy = crate::runtime::workflow::ConcurrencyPolicy::default();
        let serialized = serde_json::to_value(policy).unwrap();
        for key in schema["required"].as_array().unwrap() {
            assert!(serialized.get(key.as_str().unwrap()).is_some());
        }
        let parsed: crate::runtime::workflow::ConcurrencyPolicy = serde_json::from_value(serialized).unwrap();
        assert_eq!(parsed.max_active_workers, 6);
        assert!(serde_json::from_value::<crate::runtime::workflow::ConcurrencyPolicy>(serde_json::json!(1)).is_err());
    }

    #[test]
    fn workflow_evidence_schema_matches_persisted_contract_and_receipts() {
        use crate::runtime::workflow::{ConcurrencyPolicy, DurableWorkflowContract, EvidenceReceipt, EvidenceRequirement};
        let definition = super::work();
        let payload = &definition.parameters["properties"]["workflow_payload"]["properties"];
        let contract_schema = &payload["contract"];
        let required = &contract_schema["properties"]["evidence_requirements"]["items"];
        let requirement = serde_json::json!({
            "kind":"artifact_check", "description":"Actual result exercised",
            "minimum_receipts":2, "required":true
        });
        let parsed: EvidenceRequirement = serde_json::from_value(requirement.clone()).unwrap();
        assert_eq!(parsed.minimum_receipts, 2);
        for key in required["required"].as_array().unwrap() {
            assert!(requirement.get(key.as_str().unwrap()).is_some());
        }
        for key in ["kind","description","minimum_receipts","required"] {
            let mut invalid = requirement.clone();
            invalid.as_object_mut().unwrap().remove(key);
            assert!(serde_json::from_value::<EvidenceRequirement>(invalid).is_err());
        }
        assert_eq!(&payload["evidence_requirements"]["items"], required);
        let receipt = serde_json::json!({
            "receipt_id":"receipt_checked", "kind":"artifact_check",
            "summary":"Observed expected output", "verified":true,
            "recorded_at":"2026-09-17T12:00:00Z"
        });
        for key in payload["evidence"]["items"]["required"].as_array().unwrap() {
            assert!(receipt.get(key.as_str().unwrap()).is_some());
        }
        let receipt: EvidenceReceipt = serde_json::from_value(receipt).unwrap();
        assert_eq!(receipt.kind, parsed.kind);
        let contract: DurableWorkflowContract = serde_json::from_value(serde_json::json!({
            "title":"Example", "objective":"Checked output", "budget":{},
            "concurrency":ConcurrencyPolicy::default(),
            "evidence_requirements":[requirement]
        })).unwrap();
        let serialized = serde_json::to_value(contract).unwrap();
        for key in serialized.as_object().unwrap().keys() {
            assert!(contract_schema["properties"].get(key).is_some(), "missing advertised contract field {key}");
        }
        assert_eq!(contract_schema["additionalProperties"], false);
    }

    #[test]
    fn recall_nullable_search_fields_do_not_select_an_exact_line() {
        let dir=tempfile::tempdir().unwrap();
        let record=serde_json::json!({"type":"User","content":"Ceramic holder accepted finish: ochre"});
        std::fs::write(dir.path().join("nullable-search.archive.jsonl"),format!("{record}\n")).unwrap();
        let input=serde_json::from_value::<crate::tools::recall::RecallInput>(serde_json::json!({
            "query":"Ceramic holder","line":null,"offset":null,"limit":null
        })).unwrap();
        assert!(input.line.is_none());
        let result=crate::tools::recall::execute(input,dir.path(),"nullable-search").unwrap();
        assert!(result.summary.starts_with("recalled"));
        assert!(result.content.contains("archive line 1"));
        assert!(result.content.contains("ochre"));
    }

    #[test]
    fn computer_batch_schema_has_action_specific_fields() {
        let schema = super::computer_action_schema();
        let variants = schema["anyOf"].as_array().unwrap();
        assert_eq!(variants.len(), 10);
        let key = variants.iter().find(|v| v["properties"]["type"]["enum"][0] == "key").unwrap();
        assert_eq!(key["properties"].as_object().unwrap().len(), 2);
        assert_eq!(key["required"], serde_json::json!(["type", "combo"]));
        assert_eq!(key["additionalProperties"], false);
        for value in [
            serde_json::json!({"type":"key","combo":"a"}),
            serde_json::json!({"type":"type","text":"Banana"}),
            serde_json::json!({"type":"move","x":10,"y":20,"duration_ms":150}),
            serde_json::json!({"type":"drag","from_x":1,"from_y":2,"to_x":3,"to_y":4}),
        ] {
            serde_json::from_value::<crate::tools::computer_use::Action>(value).unwrap();
        }
    }

    #[test]
    fn window_action_guidance_separates_pointer_position_from_edit_focus() {
        let definition = super::tool_definition("computer_window_act").unwrap();
        for rule in [
            "does not commit a text edit or release keyboard capture",
            "never insert Enter or Escape blindly",
            "End the batch at uncertain creation, selection or mode changes",
            "Delivered input is not proof",
        ] {
            assert!(definition.description.contains(rule));
        }
        for application_recipe in ["Blender", "banana", "bpy", "Add Area"] {
            assert!(!definition.description.contains(application_recipe));
        }
        assert_eq!(definition.parameters["properties"]["actions"]["maxItems"], 25);
        assert!(definition.description.contains("without changing selection"));
    }

    #[test]
    fn window_batch_schema_matches_native_action_fields_and_limits() {
        let definition = super::tool_definition("computer_window_act").unwrap();
        let actions = &definition.parameters["properties"]["actions"];
        assert_eq!(actions["minItems"], 1);
        assert_eq!(actions["maxItems"], 25);
        let variants = actions["items"]["anyOf"].as_array().unwrap();
        assert_eq!(variants.len(), 8);
        for (kind, fields) in [
            ("move",vec!["type","x","y"]),
            ("click",vec!["type","x","y","button","double"]),
            ("double_click",vec!["type","x","y","button"]),
            ("scroll",vec!["type","x","y","dx","dy"]),
            ("type",vec!["type","text"]),
            ("key",vec!["type","combo"]),
            ("wait",vec!["type","ms"]),
            ("drag",vec!["type","from_x","from_y","to_x","to_y","duration_ms"]),
        ] {
            let variant = variants.iter().find(|v|v["properties"]["type"]["enum"][0]==kind).unwrap();
            let properties = variant["properties"].as_object().unwrap();
            assert_eq!(properties.len(),fields.len());
            assert!(fields.into_iter().all(|field|properties.contains_key(field)));
            assert_eq!(variant["additionalProperties"],false);
            if matches!(kind,"move"|"click"|"double_click"|"scroll") {
                assert_eq!(variant["required"],serde_json::json!(["type","x","y"]));
            }
            if kind=="scroll" {
                assert_eq!(variant["properties"]["dy"]["minimum"],-30);
                assert_eq!(variant["properties"]["dy"]["maximum"],30);
                assert_eq!(variant["anyOf"],serde_json::json!([{"required":["dx"]},{"required":["dy"]}]));
            }
        }
    }

    use super::*;

    #[test]
    fn work_schema_exposes_action_specific_required_fields() {
        let work = tool_definition("work").unwrap();
        let lifecycle = work.parameters["properties"]["workflow_action"]["description"].as_str().unwrap();
        assert!(lifecycle.contains("omit goal_id"));
        assert!(lifecycle.contains("minimum_receipts (integer >=1)"));
        assert!(lifecycle.contains("required (boolean)"));
        let schema = tool_definition("work").expect("work definition").parameters;
        let rules = schema["allOf"].as_array().expect("conditional rules");
        let required_for = |action: &str| {
            rules
                .iter()
                .find(|rule| {
                    rule["if"]["properties"]["action"]["const"] == action
                        || rule["if"]["properties"]["action"]["enum"]
                            .as_array()
                            .is_some_and(|values| values.iter().any(|value| value == action))
                })
                .and_then(|rule| rule["then"]["required"].as_array())
                .expect("action-specific requirements")
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
        };
        assert_eq!(required_for("claim"), ["node_id", "approach"]);
        assert_eq!(required_for("status"), ["node_id", "state"]);
        assert_eq!(required_for("note"), ["reason"]);
        assert!(schema["properties"]["target_id"]["description"]
            .as_str()
            .unwrap()
            .contains("NOT a work node id"));
    }

    #[test]
    fn credential_tool_teaches_secret_safe_autonomous_recovery() {
        let definition = tool_definition("browser_input_credential").expect("credential tool");
        assert!(definition.description.contains("not an invalid password"));
        assert!(definition.description.contains("refresh browser_state"));
        assert!(definition
            .description
            .contains("retry this secure fill once"));
        assert!(definition
            .description
            .contains("only after the SITE rejects"));
        assert!(definition
            .description
            .contains("never ask the user to re-enter"));
    }
}
