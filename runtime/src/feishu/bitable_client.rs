use calamine::{open_workbook, Data, Reader, Xlsx};
use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
use reqwest::{Method, StatusCode};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{LazyLock, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use super::args::{query_conditions_summary, QueryCondition, QueryOp};

const API_ROOT: &str = "https://open.feishu.cn/open-apis";
const MAX_API_RETRIES: usize = 4;
const BATCH_SIZE: usize = 500;
const FIELD_CACHE_TTL: Duration = Duration::from_secs(300);
const TOKEN_REFRESH_SKEW: Duration = Duration::from_secs(120);
/// 飞书开放平台企业自建应用（多维表格 Open API），内置不可由用户修改
pub(crate) const FEISHU_OPEN_APP_ID: &str = "cli_a9312e4bd9ba9cd3";
pub(crate) const FEISHU_OPEN_APP_SECRET: &str = "MdJwS5IMjrPVi2okhRnfTd0f6vlOObZJ";

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
static TOKEN_CACHE: LazyLock<Mutex<Option<CachedToken>>> = LazyLock::new(|| Mutex::new(None));
static FIELD_CACHE: LazyLock<Mutex<HashMap<String, CachedFields>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone)]
struct CachedToken {
    app_id: String,
    app_secret: String,
    token: String,
    expires_at: Instant,
}

#[derive(Clone, Debug)]
struct FieldMeta {
    field_type: i64,
    #[allow(dead_code)]
    ui_type: String,
}

#[derive(Clone)]
struct CachedFields {
    fetched_at: Instant,
    fields: HashMap<String, FieldMeta>,
    /// 与飞书表头一致的字段顺序
    order: Vec<String>,
}

#[derive(Debug, Default)]
pub struct SyncRecordsResult {
    pub created: u32,
    pub updated: u32,
    pub failed: u32,
    pub errors: Vec<String>,
}

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .build()
            .expect("build Feishu HTTP client")
    })
}

pub(crate) fn shared_http_client() -> &'static reqwest::Client {
    http_client()
}

pub(crate) fn api_root() -> &'static str {
    API_ROOT
}

fn field_cache_key(app_token: &str, table_id: &str) -> String {
    format!("{app_token}:{table_id}")
}

async fn tenant_access_token(app_id: &str, app_secret: &str) -> Result<(String, u64), String> {
    if app_id.trim().is_empty() || app_secret.trim().is_empty() {
        return Err(
            "未配置飞书开放平台凭证。请在 设置 → 飞书 填写 App ID 与 App Secret".into(),
        );
    }
    let body = json!({
        "app_id": app_id.trim(),
        "app_secret": app_secret.trim(),
    });
    let resp: Value = http_client()
        .post(format!("{API_ROOT}/auth/v3/tenant_access_token/internal"))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("请求飞书 token 失败: {e}"))?
        .json()
        .await
        .map_err(|e| format!("解析飞书 token 响应失败: {e}"))?;

    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code != 0 {
        let msg = resp
            .get("msg")
            .and_then(|v| v.as_str())
            .unwrap_or("未知错误");
        return Err(format!("飞书 token 获取失败 ({code}): {msg}"));
    }
    let token = resp
        .get("tenant_access_token")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| "飞书 token 响应缺少 tenant_access_token".to_string())?;
    let expire = resp
        .get("expire")
        .and_then(|v| v.as_u64())
        .unwrap_or(7200);
    Ok((token, expire))
}

async fn auth_token() -> Result<String, String> {
    let app_id = FEISHU_OPEN_APP_ID.to_string();
    let app_secret = FEISHU_OPEN_APP_SECRET.to_string();

    {
        let cache = TOKEN_CACHE.lock().await;
        if let Some(cached) = cache.as_ref() {
            if cached.app_id == app_id
                && cached.app_secret == app_secret
                && Instant::now() < cached.expires_at
            {
                return Ok(cached.token.clone());
            }
        }
    }

    let (token, expire_secs) = tenant_access_token(&app_id, &app_secret).await?;
    let ttl = Duration::from_secs(expire_secs.max(180)).saturating_sub(TOKEN_REFRESH_SKEW);
    let expires_at = Instant::now() + ttl;
    {
        let mut cache = TOKEN_CACHE.lock().await;
        *cache = Some(CachedToken {
            app_id,
            app_secret,
            token: token.clone(),
            expires_at,
        });
    }
    Ok(token)
}

pub(crate) async fn shared_auth_token() -> Result<String, String> {
    auth_token().await
}

fn api_error(prefix: &str, resp: &Value) -> String {
    let code = resp.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    let msg = resp
        .get("msg")
        .and_then(|v| v.as_str())
        .unwrap_or("未知错误");
    let base = format!("{prefix} ({code}): {msg}");
    match code {
        1254045 => format!(
            "{base}\n说明：写入的字段名与表头不一致。请先 飞书查询 查看表结构，或打开表格核对真实列名后再写 飞书新增,列名=值。"
        ),
        91403 => format!(
            "{base}\n\
             说明：开放平台勾选 bitable:app 只代表「有能力调用接口」，还须在目标多维表格里授权本应用。\n\
             处理：打开该多维表格 → 右上角「…」→ 更多 → 添加文档应用 → 选中本应用 → 权限选「可编辑」\
             （若表格开启了高级权限，需给「可管理」）。授权后重试即可，一般无需再申请其它 API 权限。"
        ),
        99991672 => format!(
            "{base}\n说明：应用缺少多维表格相关 API 权限。请在飞书开放平台 → 权限管理开通 bitable:app（或更细粒度的 base:record:*），发布新版本后再试。"
        ),
        _ => base,
    }
}

fn is_retryable_api_code(code: i64) -> bool {
    matches!(code, 99991400 | 1254290 | 1254291 | 1254607 | 1254608)
}

fn retry_after_secs(headers: &reqwest::header::HeaderMap, attempt: usize) -> u64 {
    if let Some(v) = headers.get("x-ogw-ratelimit-reset").and_then(|h| h.to_str().ok()) {
        if let Ok(secs) = v.parse::<u64>() {
            return secs.max(1).min(30);
        }
    }
    if let Some(v) = headers.get(reqwest::header::RETRY_AFTER).and_then(|h| h.to_str().ok()) {
        if let Ok(secs) = v.parse::<u64>() {
            return secs.max(1).min(30);
        }
    }
    (1u64 << attempt.min(3)).min(8)
}

async fn api_request_json(
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let url = format!("{API_ROOT}{path}");
    let mut last_err = String::new();

    for attempt in 0..MAX_API_RETRIES {
        let token = auth_token().await?;
        let mut req = http_client()
            .request(method.clone(), &url)
            .header("Authorization", format!("Bearer {token}"));
        if let Some(ref b) = body {
            req = req.json(b);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("请求飞书 API 失败: {e}");
                if attempt + 1 < MAX_API_RETRIES {
                    tokio::time::sleep(Duration::from_millis(250 * (attempt as u64 + 1))).await;
                    continue;
                }
                return Err(last_err);
            }
        };

        let status = resp.status();
        let headers = resp.headers().clone();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("读取飞书响应失败: {e}"))?;
        let json: Value = serde_json::from_str(&text).unwrap_or_else(|_| {
            json!({
                "code": -1,
                "msg": text,
            })
        });
        let code = json.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);

        if status == StatusCode::TOO_MANY_REQUESTS
            || status.as_u16() == 429
            || is_retryable_api_code(code)
        {
            last_err = api_error("飞书 API 限流/冲突", &json);
            if attempt + 1 < MAX_API_RETRIES {
                let wait = retry_after_secs(&headers, attempt);
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }
            return Err(last_err);
        }

        if !status.is_success() && code == -1 {
            return Err(format!("飞书 API HTTP {status}: {text}"));
        }
        if code != 0 {
            return Err(api_error("飞书 API 调用失败", &json));
        }
        return Ok(json);
    }

    Err(last_err)
}

async fn list_table_fields(
    app_token: &str,
    table_id: &str,
) -> Result<(HashMap<String, FieldMeta>, Vec<String>), String> {
    let cache_key = field_cache_key(app_token, table_id);
    {
        let cache = FIELD_CACHE.lock().await;
        if let Some(entry) = cache.get(&cache_key) {
            if entry.fetched_at.elapsed() < FIELD_CACHE_TTL {
                return Ok((entry.fields.clone(), entry.order.clone()));
            }
        }
    }

    let mut out = HashMap::new();
    let mut order = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut path = format!(
            "/bitable/v1/apps/{app_token}/tables/{table_id}/fields?page_size=100"
        );
        if let Some(ref pt) = page_token {
            path.push_str(&format!("&page_token={pt}"));
        }
        let resp = api_request_json(Method::GET, &path, None).await?;
        let data = resp.get("data").unwrap_or(&Value::Null);
        if let Some(items) = data.get("items").and_then(|v| v.as_array()) {
            for item in items {
                let name = item
                    .get("field_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                let field_type = item.get("type").and_then(|v| v.as_i64()).unwrap_or(1);
                let ui_type = item
                    .get("ui_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !out.contains_key(&name) {
                    order.push(name.clone());
                }
                out.insert(name, FieldMeta { field_type, ui_type });
            }
        }
        let has_more = data
            .get("has_more")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !has_more {
            break;
        }
        page_token = data
            .get("page_token")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if page_token.is_none() {
            break;
        }
    }

    {
        let mut cache = FIELD_CACHE.lock().await;
        cache.insert(
            cache_key,
            CachedFields {
                fetched_at: Instant::now(),
                fields: out.clone(),
                order: order.clone(),
            },
        );
    }
    Ok((out, order))
}

fn parse_bool_loose(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "y" | "是" | "真" | "勾选" => Some(true),
        "false" | "0" | "no" | "n" | "否" | "假" | "未勾选" => Some(false),
        _ => None,
    }
}

fn parse_number_loose(raw: &str) -> Option<f64> {
    let t = raw.trim().replace(',', "");
    if t.is_empty() {
        return None;
    }
    if let Ok(n) = t.parse::<f64>() {
        return Some(n);
    }
    let cleaned: String = t
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
        .collect();
    cleaned.parse().ok()
}

fn parse_datetime_millis(raw: &str) -> Option<i64> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(n) = t.parse::<i64>() {
        if n > 10_000_000_000 {
            return Some(n); // already ms
        }
        if n > 1_000_000_000 {
            return Some(n * 1000); // seconds
        }
    }
    if let Ok(n) = t.parse::<f64>() {
        if n > 10_000_000_000.0 {
            return Some(n as i64);
        }
        if n > 1_000_000_000.0 {
            return Some((n * 1000.0) as i64);
        }
    }

    let formats = [
        "%Y-%m-%d %H:%M:%S",
        "%Y/%m/%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y/%m/%d %H:%M",
        "%Y-%m-%d",
        "%Y/%m/%d",
        "%Y年%m月%d日",
    ];
    for fmt in formats {
        if let Ok(dt) = NaiveDateTime::parse_from_str(t, fmt) {
            return Some(Utc.from_utc_datetime(&dt).timestamp_millis());
        }
        if let Ok(d) = NaiveDate::parse_from_str(t, fmt) {
            let dt = d.and_hms_opt(0, 0, 0)?;
            return Some(Utc.from_utc_datetime(&dt).timestamp_millis());
        }
    }
    None
}

fn split_multi_values(raw: &str) -> Vec<String> {
    raw.split(|c| c == ',' || c == '，' || c == ';' || c == '；' || c == '|' || c == '\n')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn convert_field_value(meta: Option<&FieldMeta>, raw: &str) -> Value {
    let raw = raw.trim();
    let Some(meta) = meta else {
        return json!(raw);
    };

    match meta.field_type {
        2 => {
            // number / progress / currency / rating
            if let Some(n) = parse_number_loose(raw) {
                json!(n)
            } else {
                json!(raw)
            }
        }
        4 | 18 | 21 => {
            // multi-select / single link / duplex link
            json!(split_multi_values(raw))
        }
        5 | 1001 | 1002 => {
            // datetime
            if let Some(ms) = parse_datetime_millis(raw) {
                json!(ms)
            } else {
                json!(raw)
            }
        }
        7 => {
            if let Some(b) = parse_bool_loose(raw) {
                json!(b)
            } else {
                json!(raw)
            }
        }
        11 | 23 => {
            // user / group chat
            let ids = split_multi_values(raw);
            let arr: Vec<Value> = ids.into_iter().map(|id| json!({ "id": id })).collect();
            json!(arr)
        }
        15 => {
            // url
            if raw.contains('|') {
                let mut parts = raw.splitn(2, '|');
                let text = parts.next().unwrap_or("").trim();
                let link = parts.next().unwrap_or("").trim();
                json!({ "text": text, "link": link })
            } else {
                json!({ "text": raw, "link": raw })
            }
        }
        17 => {
            // attachment tokens
            let tokens = split_multi_values(raw);
            let arr: Vec<Value> = tokens
                .into_iter()
                .map(|t| json!({ "file_token": t }))
                .collect();
            json!(arr)
        }
        _ => json!(raw), // text / single select / phone / etc.
    }
}

fn ensure_known_fields(
    fields: &HashMap<String, String>,
    metas: &HashMap<String, FieldMeta>,
) -> Result<(), String> {
    let unknown: Vec<&String> = fields.keys().filter(|k| !metas.contains_key(*k)).collect();
    if unknown.is_empty() {
        return Ok(());
    }
    let mut available: Vec<&String> = metas.keys().collect();
    available.sort();
    let sample = available.first().map(|s| s.as_str()).unwrap_or("列名");
    Err(format!(
        "字段不存在：{}\n当前表可用字段：{}\n请改成表头里的真实列名，例如：飞书新增,{sample}=值",
        unknown
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("、"),
        if available.is_empty() {
            "（未读到字段，请确认应用对该表有权限）".into()
        } else {
            available
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("、")
        }
    ))
}

fn fields_to_api_map(
    fields: &HashMap<String, String>,
    metas: &HashMap<String, FieldMeta>,
) -> Map<String, Value> {
    let mut api_fields = Map::new();
    for (key, value) in fields {
        if value.trim().is_empty() {
            continue;
        }
        api_fields.insert(key.clone(), convert_field_value(metas.get(key), value));
    }
    api_fields
}

fn is_date_field(meta: &FieldMeta) -> bool {
    matches!(meta.field_type, 5 | 1001 | 1002)
}

fn api_operator(op: QueryOp) -> &'static str {
    match op {
        QueryOp::Is => "is",
        QueryOp::IsNot => "isNot",
        QueryOp::Contains => "contains",
        QueryOp::DoesNotContain => "doesNotContain",
        QueryOp::IsEmpty => "isEmpty",
        QueryOp::IsNotEmpty => "isNotEmpty",
    }
}

fn validate_query_op(field: &str, op: QueryOp, metas: &HashMap<String, FieldMeta>) -> Result<(), String> {
    let Some(meta) = metas.get(field) else {
        let mut available: Vec<&String> = metas.keys().collect();
        available.sort();
        let sample = available.first().map(|s| s.as_str()).unwrap_or("列名");
        return Err(format!(
            "字段不存在：{field}\n当前表可用字段：{}\n请改成表头里的真实列名，例如：{sample}=值",
            if available.is_empty() {
                "（未读到字段，请确认应用对该表有权限）".into()
            } else {
                available
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("、")
            }
        ));
    };
    if is_date_field(meta)
        && matches!(
            op,
            QueryOp::IsNot | QueryOp::Contains | QueryOp::DoesNotContain
        )
    {
        return Err(format!(
            "「{field}」是日期字段，不支持「{}」。请改用 =、empty 或 !empty",
            op.label()
        ));
    }
    Ok(())
}

fn build_query_filter(
    conditions: &[QueryCondition],
    metas: &HashMap<String, FieldMeta>,
) -> Result<Option<Value>, String> {
    if conditions.is_empty() {
        return Ok(None);
    }
    let mut items = Vec::new();
    for cond in conditions {
        validate_query_op(&cond.field, cond.op, metas)?;
        let (operator, value) = match cond.op {
            QueryOp::IsEmpty | QueryOp::IsNotEmpty => (api_operator(cond.op), json!([])),
            _ => {
                let raw = cond
                    .value
                    .as_deref()
                    .ok_or_else(|| format!("字段「{}」{} 需要填写比较值", cond.field, cond.op.label()))?;
                let converted = convert_field_value(metas.get(&cond.field), raw);
                let filter_value = match converted {
                    Value::Array(arr) => arr,
                    other => vec![other],
                };
                (api_operator(cond.op), json!(filter_value))
            }
        };
        items.push(json!({
            "field_name": cond.field,
            "operator": operator,
            "value": value,
        }));
    }
    Ok(Some(json!({
        "conjunction": "and",
        "conditions": items,
    })))
}

pub fn split_match_and_data(
    fields: &HashMap<String, String>,
    unique_keys: &[String],
) -> (HashMap<String, String>, HashMap<String, String>) {
    let mut match_fields = HashMap::new();
    let mut data_fields = HashMap::new();
    for (k, v) in fields {
        if unique_keys.iter().any(|uk| uk == k) {
            match_fields.insert(k.clone(), v.clone());
        } else {
            data_fields.insert(k.clone(), v.clone());
        }
    }
    (match_fields, data_fields)
}

fn unique_key_fingerprint(fields: &HashMap<String, String>, unique_keys: &[String]) -> String {
    unique_keys
        .iter()
        .map(|k| {
            let v = fields.get(k).map(|s| s.trim()).unwrap_or("");
            format!("{k}={v}")
        })
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

pub async fn search_records(
    app_token: &str,
    table_id: &str,
    filter: &HashMap<String, String>,
) -> Result<Vec<(String, Value)>, String> {
    let conditions: Vec<QueryCondition> = filter
        .iter()
        .map(|(field, value)| QueryCondition {
            field: field.clone(),
            op: QueryOp::Is,
            value: Some(value.clone()),
        })
        .collect();
    search_records_with_query(app_token, table_id, &conditions).await
}

pub async fn search_records_with_query(
    app_token: &str,
    table_id: &str,
    conditions: &[QueryCondition],
) -> Result<Vec<(String, Value)>, String> {
    let (metas, _order) = list_table_fields(app_token, table_id).await?;
    let path = format!("/bitable/v1/apps/{app_token}/tables/{table_id}/records/search");

    let mut all = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut body = json!({
            "page_size": 500,
        });
        if let Some(f) = build_query_filter(conditions, &metas)? {
            body["filter"] = f;
        }
        if let Some(ref pt) = page_token {
            body["page_token"] = json!(pt);
        }

        let resp = api_request_json(Method::POST, &path, Some(body)).await?;
        let data = resp.get("data").unwrap_or(&Value::Null);
        if let Some(items) = data.get("items").and_then(|v| v.as_array()) {
            for item in items {
                let record_id = item
                    .get("record_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let fields = item.get("fields").cloned().unwrap_or(json!({}));
                if !record_id.is_empty() {
                    all.push((record_id, fields));
                }
            }
        }

        let has_more = data
            .get("has_more")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !has_more {
            break;
        }
        page_token = data
            .get("page_token")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if page_token.is_none() {
            break;
        }
    }
    Ok(all)
}

pub async fn create_record(
    app_token: &str,
    table_id: &str,
    fields: &HashMap<String, String>,
) -> Result<String, String> {
    let (metas, _order) = list_table_fields(app_token, table_id).await?;
    ensure_known_fields(fields, &metas)?;
    let api_fields = fields_to_api_map(fields, &metas);
    if api_fields.is_empty() {
        return Err("至少需要提供一个非空字段".into());
    }
    let path = format!("/bitable/v1/apps/{app_token}/tables/{table_id}/records");
    let body = json!({ "fields": api_fields });
    let resp = api_request_json(Method::POST, &path, Some(body)).await?;
    resp.get("data")
        .and_then(|d| d.get("record"))
        .and_then(|r| r.get("record_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| "飞书响应缺少 record_id".into())
}

pub async fn update_record(
    app_token: &str,
    table_id: &str,
    record_id: &str,
    fields: &HashMap<String, String>,
) -> Result<(), String> {
    if fields.is_empty() {
        return Err("更新字段不能为空".into());
    }
    let (metas, _order) = list_table_fields(app_token, table_id).await?;
    ensure_known_fields(fields, &metas)?;
    let api_fields = fields_to_api_map(fields, &metas);
    if api_fields.is_empty() {
        return Err("更新字段不能为空".into());
    }
    let path = format!(
        "/bitable/v1/apps/{app_token}/tables/{table_id}/records/{record_id}"
    );
    let body = json!({ "fields": api_fields });
    let _ = api_request_json(Method::PUT, &path, Some(body)).await?;
    Ok(())
}

pub async fn delete_records(
    app_token: &str,
    table_id: &str,
    record_ids: &[String],
) -> Result<u32, String> {
    if record_ids.is_empty() {
        return Ok(0);
    }
    let path = format!("/bitable/v1/apps/{app_token}/tables/{table_id}/records/batch_delete");
    let mut deleted = 0u32;
    for chunk in record_ids.chunks(BATCH_SIZE) {
        let body = json!({ "records": chunk });
        let _ = api_request_json(Method::POST, &path, Some(body)).await?;
        deleted += chunk.len() as u32;
    }
    Ok(deleted)
}

/// 按唯一键 upsert：匹配则更新，否则新增
pub async fn upsert_record(
    app_token: &str,
    table_id: &str,
    unique_keys: &[String],
    fields: &HashMap<String, String>,
) -> Result<(String, bool), String> {
    let (match_fields, data_fields) = split_match_and_data(fields, unique_keys);
    if match_fields.is_empty() && !unique_keys.is_empty() {
        return Err(format!(
            "更新/同步需要至少提供一个唯一键字段：{}",
            unique_keys.join("、")
        ));
    }

    let filter = if match_fields.is_empty() {
        HashMap::new()
    } else {
        match_fields.clone()
    };

    let existing = search_records(app_token, table_id, &filter).await?;
    if existing.is_empty() {
        let id = create_record(app_token, table_id, fields).await?;
        return Ok((id, true));
    }

    let mut update_fields = data_fields;
    for (k, v) in &match_fields {
        update_fields.entry(k.clone()).or_insert_with(|| v.clone());
    }

    let (record_id, _) = &existing[0];
    update_record(app_token, table_id, record_id, &update_fields).await?;
    Ok((record_id.clone(), false))
}

async fn batch_create_records(
    app_token: &str,
    table_id: &str,
    rows: &[HashMap<String, String>],
    metas: &HashMap<String, FieldMeta>,
) -> Result<u32, String> {
    if rows.is_empty() {
        return Ok(0);
    }
    let path = format!("/bitable/v1/apps/{app_token}/tables/{table_id}/records/batch_create");
    let mut created = 0u32;
    for chunk in rows.chunks(BATCH_SIZE) {
        let records: Vec<Value> = chunk
            .iter()
            .map(|fields| {
                json!({
                    "fields": fields_to_api_map(fields, metas),
                })
            })
            .collect();
        let body = json!({ "records": records });
        let _ = api_request_json(Method::POST, &path, Some(body)).await?;
        created += chunk.len() as u32;
    }
    Ok(created)
}

async fn batch_update_records(
    app_token: &str,
    table_id: &str,
    rows: &[(String, HashMap<String, String>)],
    metas: &HashMap<String, FieldMeta>,
) -> Result<u32, String> {
    if rows.is_empty() {
        return Ok(0);
    }
    let path = format!("/bitable/v1/apps/{app_token}/tables/{table_id}/records/batch_update");
    let mut updated = 0u32;
    for chunk in rows.chunks(BATCH_SIZE) {
        let records: Vec<Value> = chunk
            .iter()
            .map(|(record_id, fields)| {
                json!({
                    "record_id": record_id,
                    "fields": fields_to_api_map(fields, metas),
                })
            })
            .collect();
        let body = json!({ "records": records });
        let _ = api_request_json(Method::POST, &path, Some(body)).await?;
        updated += chunk.len() as u32;
    }
    Ok(updated)
}

/// 批量同步：先按唯一键扫表建索引，再 batch_create / batch_update。
pub async fn sync_records(
    app_token: &str,
    table_id: &str,
    unique_keys: &[String],
    rows: &[HashMap<String, String>],
) -> Result<SyncRecordsResult, String> {
    let mut result = SyncRecordsResult::default();
    if rows.is_empty() {
        return Ok(result);
    }
    if unique_keys.is_empty() {
        return Err("同步需要配置唯一键（unique_keys）".into());
    }

    let (metas, _order) = list_table_fields(app_token, table_id).await?;
    let existing = search_records(app_token, table_id, &HashMap::new()).await?;
    let mut index: HashMap<String, String> = HashMap::new();
    for (record_id, fields_val) in &existing {
        let mut key_map = HashMap::new();
        for uk in unique_keys {
            if let Some(v) = fields_val.get(uk) {
                let s = match v {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Array(arr) => arr
                        .iter()
                        .map(|x| match x {
                            Value::String(s) => s.clone(),
                            other => other.to_string().trim_matches('"').to_string(),
                        })
                        .collect::<Vec<_>>()
                        .join(","),
                    other => other.to_string().trim_matches('"').to_string(),
                };
                key_map.insert(uk.clone(), s);
            }
        }
        if key_map.len() == unique_keys.len() {
            index.insert(unique_key_fingerprint(&key_map, unique_keys), record_id.clone());
        }
    }

    let mut to_create: Vec<HashMap<String, String>> = Vec::new();
    let mut to_update: Vec<(String, HashMap<String, String>)> = Vec::new();
    let mut seen_keys = HashSet::new();

    for row in rows {
        let (match_fields, data_fields) = split_match_and_data(row, unique_keys);
        if match_fields.len() != unique_keys.len() {
            result.failed += 1;
            result.errors.push(format!(
                "缺少唯一键字段：需要 {}，实际 {}",
                unique_keys.join("、"),
                match_fields
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("、")
            ));
            continue;
        }
        let fp = unique_key_fingerprint(&match_fields, unique_keys);
        if !seen_keys.insert(fp.clone()) {
            // 同源重复行：保留首次，后续记失败
            result.failed += 1;
            result.errors.push(format!("重复唯一键：{fp}"));
            continue;
        }

        let mut update_fields = data_fields;
        for (k, v) in &match_fields {
            update_fields.entry(k.clone()).or_insert_with(|| v.clone());
        }

        if let Some(record_id) = index.get(&fp) {
            to_update.push((record_id.clone(), update_fields));
        } else {
            to_create.push(row.clone());
        }
    }

    match batch_create_records(app_token, table_id, &to_create, &metas).await {
        Ok(n) => result.created += n,
        Err(e) => {
            result.failed += to_create.len() as u32;
            result.errors.push(format!("批量新增失败: {e}"));
        }
    }

    match batch_update_records(app_token, table_id, &to_update, &metas).await {
        Ok(n) => result.updated += n,
        Err(e) => {
            result.failed += to_update.len() as u32;
            result.errors.push(format!("批量更新失败: {e}"));
        }
    }

    Ok(result)
}

fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(s) => s.trim().to_string(),
        Data::Float(f) => {
            if f.fract().abs() < f64::EPSILON {
                format!("{}", *f as i64)
            } else {
                format!("{f}")
            }
        }
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => b.to_string(),
        Data::DateTime(dt) => dt.to_string(),
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("{e:?}"),
    }
}

pub fn read_excel_rows(path: &Path) -> Result<(Vec<String>, Vec<HashMap<String, String>>), String> {
    let mut workbook: Xlsx<_> =
        open_workbook(path).map_err(|e| format!("读取 Excel 失败 {}: {e}", path.display()))?;
    let range = workbook
        .worksheet_range_at(0)
        .ok_or_else(|| "Excel 无工作表".to_string())?
        .map_err(|e| format!("读取工作表失败: {e}"))?;

    let mut rows_iter = range.rows();
    let header_row = rows_iter
        .next()
        .ok_or_else(|| "Excel 表头为空".to_string())?;
    let headers: Vec<String> = header_row
        .iter()
        .map(cell_to_string)
        .map(|s| s.trim().to_string())
        .collect();
    if headers.iter().all(|h| h.is_empty()) {
        return Err("Excel 第一行表头为空".into());
    }

    let mut data_rows = Vec::new();
    for row in rows_iter {
        let mut record = HashMap::new();
        let mut has_value = false;
        for (i, cell) in row.iter().enumerate() {
            let key = headers.get(i).map(|s| s.as_str()).unwrap_or("");
            if key.is_empty() {
                continue;
            }
            let val = cell_to_string(cell);
            if !val.is_empty() {
                has_value = true;
                record.insert(key.to_string(), val);
            }
        }
        if has_value {
            data_rows.push(record);
        }
    }
    Ok((headers, data_rows))
}

/// 将飞书字段 JSON 展平为普通用户可读文本。
fn bitable_value_to_plain(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => if *b { "是".into() } else { "否".into() },
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if let Some(s) = format_epoch_millis(i) {
                    return s;
                }
            }
            if let Some(f) = n.as_f64() {
                let i = f as i64;
                if (f - i as f64).abs() < 1e-6 {
                    if let Some(s) = format_epoch_millis(i) {
                        return s;
                    }
                }
            }
            n.to_string()
        }
        Value::String(s) => {
            let t = s.replace('\n', " ").trim().to_string();
            if let Ok(i) = t.parse::<i64>() {
                if let Some(formatted) = format_epoch_millis(i) {
                    return formatted;
                }
            }
            t
        }
        Value::Array(arr) => arr
            .iter()
            .map(bitable_value_to_plain)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("、"),
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(|x| x.as_str()) {
                let text = text.trim();
                if let Some(link) = map.get("link").and_then(|x| x.as_str()) {
                    let link = link.trim();
                    if !link.is_empty() && link != text {
                        return format!("{text}（{link}）");
                    }
                }
                return text.to_string();
            }
            if let Some(name) = map.get("name").and_then(|x| x.as_str()) {
                return name.trim().to_string();
            }
            if let Some(id) = map.get("id").and_then(|x| x.as_str()) {
                return id.trim().to_string();
            }
            if let Some(token) = map.get("file_token").and_then(|x| x.as_str()) {
                return format!("附件:{token}");
            }
            let mut parts = Vec::new();
            for key in ["en_name", "email", "full_address", "location"] {
                if let Some(s) = map.get(key).and_then(|x| x.as_str()) {
                    let s = s.trim();
                    if !s.is_empty() {
                        parts.push(s.to_string());
                    }
                }
            }
            if !parts.is_empty() {
                return parts.join(" ");
            }
            String::new()
        }
    }
}

fn format_epoch_millis(ms: i64) -> Option<String> {
    // 毫秒时间戳（约 2001 之后）；秒级则乘 1000
    let ms = if (1_000_000_000..10_000_000_000).contains(&ms) {
        ms * 1000
    } else if ms >= 10_000_000_000 {
        ms
    } else {
        return None;
    };
    Utc.timestamp_millis_opt(ms).single().map(|dt| {
        dt.with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    })
}

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|")
        .replace('\r', "")
        .replace('\n', " ")
        .trim()
        .to_string()
}

fn row_all_empty(fields: &Value, columns: &[String]) -> bool {
    columns.iter().all(|c| {
        fields
            .get(c)
            .map(bitable_value_to_plain)
            .unwrap_or_default()
            .is_empty()
    })
}

/// 将查询结果展平为表头 + 行（「飞书查询」与「采集」表格分类共用）。
pub fn records_to_plain_table(
    records: &[(String, Value)],
    column_order: &[String],
) -> (Vec<String>, Vec<Vec<String>>) {
    let mut columns: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for name in column_order {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        if seen.insert(name.to_string()) {
            columns.push(name.to_string());
        }
    }
    for (_id, fields) in records {
        if let Some(obj) = fields.as_object() {
            for k in obj.keys() {
                if seen.insert(k.clone()) {
                    columns.push(k.clone());
                }
            }
        }
    }

    let mut rows = Vec::new();
    for (_id, fields) in records {
        if row_all_empty(fields, &columns) {
            continue;
        }
        let row: Vec<String> = columns
            .iter()
            .map(|c| {
                fields
                    .get(c)
                    .map(bitable_value_to_plain)
                    .unwrap_or_default()
            })
            .collect();
        rows.push(row);
    }
    (columns, rows)
}

/// 生成面向普通用户的 Markdown 查询报告（表格）。
/// `column_order` 应为飞书字段接口返回的表头顺序。
pub fn format_records_markdown(
    table_name: &str,
    filter: &[QueryCondition],
    records: &[(String, Value)],
    column_order: &[String],
) -> String {
    let (columns, rows) = records_to_plain_table(records, column_order);

    let cond = if filter.is_empty() {
        "全部".to_string()
    } else {
        query_conditions_summary(filter)
    };
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();

    let mut out = String::new();
    // 摘要压成一行，让表头尽快出现，方便预览识别
    out.push_str(&format!("# {table_name}\n\n"));
    out.push_str(&format!("共 {} 条 · 条件：{cond} · {now}\n\n", records.len()));

    if records.is_empty() {
        out.push_str("（没有匹配的记录）\n");
        return out;
    }

    if columns.is_empty() {
        out.push_str("（记录存在，但没有可读字段）\n");
        return out;
    }

    out.push('|');
    for c in &columns {
        out.push(' ');
        out.push_str(&md_cell(c));
        out.push_str(" |");
    }
    out.push('\n');
    out.push('|');
    for _ in &columns {
        out.push_str(" --- |");
    }
    out.push('\n');

    if rows.is_empty() {
        out.push_str("\n（记录均为空行）\n");
        return out;
    }
    for row in &rows {
        out.push('|');
        for cell in row {
            out.push(' ');
            out.push_str(&md_cell(cell));
            out.push_str(" |");
        }
        out.push('\n');
    }
    out
}

/// 返回表字段顺序（与飞书表头一致），结果带缓存。
pub async fn table_field_order(app_token: &str, table_id: &str) -> Result<Vec<String>, String> {
    let (_metas, order) = list_table_fields(app_token, table_id).await?;
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_table_for_records() {
        let filter = vec![QueryCondition {
            field: "项目".into(),
            op: QueryOp::Is,
            value: Some("鹤山".into()),
        }];
        let records = vec![(
            "rec1".into(),
            json!({
                "项目": "鹤山",
                "状态": "进行中",
            }),
        )];
        let order = vec!["项目".into(), "状态".into()];
        let md = format_records_markdown("台账", &filter, &records, &order);
        assert!(md.contains("# 台账"));
        assert!(md.contains("| 项目 | 状态 |") || md.contains("| 项目 |"));
        let header = md.find("| 项目 |").expect("header");
        let sep = md.find("| --- |").expect("separator");
        assert!(header < sep);
        assert!(md.contains("进行中"));
    }

    #[test]
    fn markdown_keeps_field_api_order() {
        let records = vec![(
            "rec1".into(),
            json!({
                "审核人": "李四",
                "项目名称": "鹤山",
            }),
        )];
        let order = vec!["项目名称".into(), "审核人".into()];
        let md = format_records_markdown("表", &[], &records, &order);
        let i_proj = md.find("| 项目名称 |").expect("project col");
        let i_auditor = md.find("审核人").expect("auditor col");
        assert!(i_proj < i_auditor);
    }

    #[test]
    fn convert_number_and_bool() {
        let num = FieldMeta {
            field_type: 2,
            ui_type: "Number".into(),
        };
        let checkbox = FieldMeta {
            field_type: 7,
            ui_type: "Checkbox".into(),
        };
        assert_eq!(convert_field_value(Some(&num), "12.5"), json!(12.5));
        assert_eq!(convert_field_value(Some(&checkbox), "是"), json!(true));
    }

    #[test]
    fn convert_multi_select_and_date() {
        let multi = FieldMeta {
            field_type: 4,
            ui_type: "MultiSelect".into(),
        };
        let date = FieldMeta {
            field_type: 5,
            ui_type: "DateTime".into(),
        };
        assert_eq!(
            convert_field_value(Some(&multi), "A，B;C"),
            json!(["A", "B", "C"])
        );
        let ms = convert_field_value(Some(&date), "2026-06-01");
        assert!(ms.as_i64().unwrap() > 0);
    }

    #[test]
    fn unique_key_fingerprint_stable() {
        let mut fields = HashMap::new();
        fields.insert("项目名称".into(), "鹤山".into());
        fields.insert("归属月份".into(), "2026-06".into());
        let keys = vec!["项目名称".into(), "归属月份".into()];
        assert_eq!(
            unique_key_fingerprint(&fields, &keys),
            "项目名称=鹤山\u{1f}归属月份=2026-06"
        );
    }
}
