use crate::ark;
use crate::scripts::builtin::SCRIPT_DOC_AI_PROMPT;
use crate::ui_host::{AppHandle, Emitter, Runtime};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub struct ChatResult {
    pub full_text: String,
    pub asks: Vec<serde_json::Value>,
    pub script: Option<serde_json::Value>,
}

const CLIENT_PROTOCOL: &str = r#"
====================
本客户端对话协议（必须遵守）
====================
测试沙箱目录：
- downloads/ = 用户上传的源表（脚本用 find_file_optional("downloads", ...)）
- templates/ = 设置→模板（find_template / copy_template）
- files/ = 脚本输出（track_output）

用户附件会以「附件预览」注入消息，请用真实表头 col_index，禁止猜列。

口径未齐时：拆成若干独立问题，本轮可一次输出多个 ask 块，不要同时给脚本。
硬性规则：
- 一个 ask = 一个决策点。禁止把不同事项塞进同一题的选项里。
- 选项只服务这一题，选项之间互斥、含义不交叉。
- 每题 prompt 只问一件事（一句）。options 至少 2 个。
- 本轮有几件未决事项，就输出几个 ```ask 块（建议 2～5 个）。

```ask
{"id":"q1","prompt":"问题（一句）","allow_multiple":false,"options":[{"id":"a","label":"选项A"},{"id":"b","label":"选项B"}]}
```

用户会一次提交本轮全部选择，消息形如：
用户本轮选择：
1. [q1] 问题… → 选项A
或：
1. [q1] 问题… → 自定义：用户手写内容

每题除选项外，用户也可自定义回答；收到「自定义：…」时以该文案为准。

可以交付脚本时：先 1～3 句中文说明，再输出唯一 rhai 块：
```rhai name="脚本名称"
完整可运行脚本
```
有 ask 时禁止输出 rhai。不要 Python。源表为 .xls 时先按 .xls 打开再 wb_save 到 .xlsx。
循环只用 while；禁止 for u in map（map 不可迭代，会报 For loop expects iterable type）。建 map 时把键 push 到数组再 while 遍历。
客户端交付后会做语法与常见坑校验；若校验失败会发回错误，请修正后重新输出完整 rhai 块。
"#;

fn system_prompt() -> String {
    let base = extract_text_block(SCRIPT_DOC_AI_PROMPT)
        .unwrap_or_else(|| SCRIPT_DOC_AI_PROMPT.to_string());
    format!("{base}\n\n{CLIENT_PROTOCOL}")
}

fn extract_text_block(md: &str) -> Option<String> {
    let start = md.find("```text\n")?;
    let rest = &md[start + "```text\n".len()..];
    let end = rest.find("```")?;
    Some(rest[..end].trim().to_string())
}

pub async fn stream_chat<R: Runtime>(
    app: &AppHandle<R>,
    session_id: &str,
    history: Vec<ChatMessage>,
    cancel: Arc<AtomicBool>,
) -> Result<ChatResult, String> {
    let mut messages = vec![json!({
        "role": "system",
        "content": system_prompt(),
    })];
    for m in &history {
        messages.push(json!({ "role": m.role, "content": m.content }));
    }
    let body = json!({
        "messages": messages,
        "temperature": 0.2,
        "max_tokens": 8192,
    });

    let stream_id = format!("rhai-studio-{session_id}");
    // 把 ark 的 ai-stream-chunk 桥接到工作台事件
    // 直接用 stream_chat_completion，再在外层根据结果解析；思考/正文另起监听较复杂，
    // 这里包一层自定义流式：复用 ark 内部请求但自行 emit。
    let result = stream_with_studio_events(app, session_id, &stream_id, body, cancel).await?;
    let (asks, script) = extract_asks_and_script(&result);
    Ok(ChatResult {
        full_text: result,
        asks,
        script,
    })
}

async fn stream_with_studio_events<R: Runtime>(
    app: &AppHandle<R>,
    session_id: &str,
    stream_id: &str,
    body: serde_json::Value,
    cancel: Arc<AtomicBool>,
) -> Result<String, String> {
    // 监听 ark 推送的 chunk，转发为 rhai-studio-event
    // 简化：直接调用 stream_chat_completion，同时用临时通道不行；
    // ark 已 emit ai-stream-chunk —— 前端也可听，但我们希望统一 rhai-studio-event。
    // 这里调用 ark，再在完成后把全文当 text；思考过程通过旁路：
    // 为获得实时效果，复制一份轻量流式（与 kit 相同），仍走 ark 的 key。

    use futures_util::StreamExt;

    ark::ensure_ark_configured()?;

    let mut body = body;
    if let Some(obj) = body.as_object_mut() {
        obj.insert("stream".into(), json!(true));
        obj.insert("model".into(), json!(ark::model_id()));
        obj.insert("thinking".into(), json!({ "type": "enabled" }));
        obj.insert("reasoning_effort".into(), json!(ark::reasoning_effort()));
    }

    let url = format!("{}/chat/completions", ark::base_url());
    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(ark::api_key())
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("方舟 API 请求失败: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp.text().await.unwrap_or_default();
        return Err(format!("方舟 API 错误 ({status}): {err}"));
    }

    let _ = app.emit(
        "rhai-studio-event",
        json!({ "sessionId": session_id, "event": "session", "data": { "sessionId": session_id } }),
    );

    let mut full_text = String::new();
    let mut buffer = String::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let chunk = chunk.map_err(|e| e.to_string())?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].trim_end_matches('\r').to_string();
            buffer = buffer[pos + 1..].to_string();
            if !line.starts_with("data:") {
                continue;
            }
            let data = line[5..].trim();
            if data == "[DONE]" {
                break;
            }
            let Ok(obj) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };
            let delta = obj
                .pointer("/choices/0/delta")
                .cloned()
                .unwrap_or(json!({}));
            if let Some(reasoning) = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .and_then(|v| v.as_str())
            {
                if !reasoning.is_empty() {
                    let _ = app.emit(
                        "rhai-studio-event",
                        json!({ "sessionId": session_id, "event": "thinking", "data": { "delta": reasoning } }),
                    );
                }
            }
            if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                if !content.is_empty() {
                    full_text.push_str(content);
                    let _ = app.emit(
                        "rhai-studio-event",
                        json!({ "sessionId": session_id, "event": "text", "data": { "delta": content } }),
                    );
                }
            }
        }
    }

    let _ = stream_id; // reserved for future cancel UX parity
    Ok(full_text)
}

fn extract_asks_and_script(text: &str) -> (Vec<serde_json::Value>, Option<serde_json::Value>) {
    let mut asks = Vec::new();
    if let Ok(re) = regex::Regex::new(r"(?s)```asks\s*\n(.*?)```") {
        if let Some(c) = re.captures(text) {
            let raw = c.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
                push_asks(&mut asks, v);
            }
        }
    }
    if let Ok(re) = regex::Regex::new(r"(?s)```ask\s*\n(.*?)```") {
        for c in re.captures_iter(text) {
            let raw = c.get(1).map(|m| m.as_str().trim()).unwrap_or("");
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
                push_asks(&mut asks, v);
            }
        }
    }
    let script = regex::Regex::new(r#"(?s)```rhai(?:\s+name="([^"]+)")?\s*\n(.*?)```"#)
        .ok()
        .and_then(|re| {
            re.captures(text).map(|c| {
                let name = c
                    .get(1)
                    .map(|m| m.as_str().trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "未命名脚本".into());
                let code = c
                    .get(2)
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default();
                json!({ "name": name, "code": code })
            })
        });
    (asks, script)
}

fn push_asks(out: &mut Vec<serde_json::Value>, v: serde_json::Value) {
    match v {
        serde_json::Value::Array(arr) => {
            for item in arr {
                push_asks(out, item);
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(qs) = map.get("questions").cloned() {
                if qs.is_array() {
                    push_asks(out, qs);
                    return;
                }
            }
            out.push(serde_json::Value::Object(map));
        }
        _ => {}
    }
}
