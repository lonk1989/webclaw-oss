//! Rhai: HTTP 请求 + JSON 解析（业务脚本拉接口用）

use chrono::Timelike;
use rhai::{Dynamic, EvalAltResult, Map};
use std::time::Duration;

fn err_runtime(msg: impl Into<String>) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(
        Dynamic::from(msg.into()),
        rhai::Position::NONE,
    ))
}

fn header_value(v: &Dynamic) -> String {
    match v.clone().into_string() {
        Ok(s) => s,
        Err(_) => v.to_string(),
    }
}

fn apply_headers(
    mut req: reqwest::blocking::RequestBuilder,
    headers: &Map,
) -> reqwest::blocking::RequestBuilder {
    for (k, v) in headers.iter() {
        req = req.header(k.as_str(), header_value(v));
    }
    req
}

fn client() -> Result<reqwest::blocking::Client, Box<EvalAltResult>> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| err_runtime(format!("创建 HTTP 客户端失败: {e}")))
}

/// 底层请求：返回 `#{ status, ok, body }`，不因 4xx/5xx 抛错（网络错误仍抛）
pub fn http_exchange(
    method: &str,
    url: &str,
    headers: Map,
    body: &str,
) -> Result<Map, Box<EvalAltResult>> {
    let m = method.trim().to_uppercase();
    if m.is_empty() {
        return Err(err_runtime("http_exchange：method 不能为空"));
    }
    let c = client()?;
    let builder = match m.as_str() {
        "GET" => c.get(url),
        "POST" => c.post(url),
        "PUT" => c.put(url),
        "PATCH" => c.patch(url),
        "DELETE" => c.delete(url),
        "HEAD" => c.head(url),
        other => {
            return Err(err_runtime(format!(
                "http_exchange：不支持的 method「{other}」（可用 GET/POST/PUT/PATCH/DELETE/HEAD）"
            )));
        }
    };

    let builder = apply_headers(builder, &headers);
    let builder = if body.is_empty() || m == "GET" || m == "HEAD" {
        builder
    } else {
        builder.body(body.to_string())
    };

    let resp = builder
        .send()
        .map_err(|e| err_runtime(format!("HTTP {m} 失败: {e}")))?;
    let status = resp.status();
    let code = status.as_u16() as i64;
    let ok = status.is_success();
    let text = resp
        .text()
        .map_err(|e| err_runtime(format!("读取响应失败: {e}")))?;

    let mut out = Map::new();
    out.insert("status".into(), Dynamic::from(code));
    out.insert("ok".into(), Dynamic::from(ok));
    out.insert("body".into(), Dynamic::from(text));
    Ok(out)
}

/// 同 `http_exchange`，但非 2xx 时 `throw`（返回响应 body 字符串）
pub fn http_request(
    method: &str,
    url: &str,
    headers: Map,
    body: &str,
) -> Result<String, Box<EvalAltResult>> {
    let m = method.trim().to_uppercase();
    let ex = http_exchange(&m, url, headers, body)?;
    let ok = ex
        .get("ok")
        .and_then(|v| v.as_bool().ok())
        .unwrap_or(false);
    let status = ex
        .get("status")
        .cloned()
        .map(|v| v.as_int().unwrap_or(0))
        .unwrap_or(0);
    let text = ex
        .get("body")
        .cloned()
        .map(|v| match v.into_string() {
            Ok(s) => s,
            Err(d) => d.to_string(),
        })
        .unwrap_or_default();
    if !ok {
        let preview: String = text.chars().take(200).collect();
        return Err(err_runtime(format!(
            "HTTP {m} 返回 {status}: {preview}"
        )));
    }
    Ok(text)
}

fn empty_headers() -> Map {
    Map::new()
}

// ── GET ──────────────────────────────────────────────

pub fn http_get(url: &str, headers: Map) -> Result<String, Box<EvalAltResult>> {
    http_request("GET", url, headers, "")
}

pub fn http_get_simple(url: &str) -> Result<String, Box<EvalAltResult>> {
    http_get(url, empty_headers())
}

// ── POST ─────────────────────────────────────────────

pub fn http_post(url: &str, headers: Map, body: &str) -> Result<String, Box<EvalAltResult>> {
    http_request("POST", url, headers, body)
}

pub fn http_post_simple(url: &str, body: &str) -> Result<String, Box<EvalAltResult>> {
    http_post(url, empty_headers(), body)
}

// ── PUT ──────────────────────────────────────────────

pub fn http_put(url: &str, headers: Map, body: &str) -> Result<String, Box<EvalAltResult>> {
    http_request("PUT", url, headers, body)
}

pub fn http_put_simple(url: &str, body: &str) -> Result<String, Box<EvalAltResult>> {
    http_put(url, empty_headers(), body)
}

// ── PATCH ────────────────────────────────────────────

pub fn http_patch(url: &str, headers: Map, body: &str) -> Result<String, Box<EvalAltResult>> {
    http_request("PATCH", url, headers, body)
}

pub fn http_patch_simple(url: &str, body: &str) -> Result<String, Box<EvalAltResult>> {
    http_patch(url, empty_headers(), body)
}

// ── DELETE ───────────────────────────────────────────

pub fn http_delete(url: &str, headers: Map, body: &str) -> Result<String, Box<EvalAltResult>> {
    http_request("DELETE", url, headers, body)
}

pub fn http_delete_headers(url: &str, headers: Map) -> Result<String, Box<EvalAltResult>> {
    http_delete(url, headers, "")
}

pub fn http_delete_simple(url: &str) -> Result<String, Box<EvalAltResult>> {
    http_delete(url, empty_headers(), "")
}

// ── JSON ─────────────────────────────────────────────

pub fn json_parse(s: &str) -> Result<Dynamic, Box<EvalAltResult>> {
    let v: serde_json::Value =
        serde_json::from_str(s).map_err(|e| err_runtime(format!("JSON 解析失败: {e}")))?;
    rhai::serde::to_dynamic(v).map_err(|e| err_runtime(format!("JSON 转脚本值失败: {e}")))
}

pub fn json_stringify(v: Dynamic) -> Result<String, Box<EvalAltResult>> {
    let value: serde_json::Value = rhai::serde::from_dynamic(&v)
        .map_err(|e| err_runtime(format!("序列化 JSON 失败: {e}")))?;
    serde_json::to_string(&value).map_err(|e| err_runtime(format!("序列化 JSON 失败: {e}")))
}

/// 从 Rhai map 取字符串（缺省或非字符串 → ""）
pub fn map_str(m: Map, key: &str) -> String {
    match m.get(key) {
        Some(v) => match v.clone().into_string() {
            Ok(s) => s,
            Err(_) => v.to_string(),
        },
        None => String::new(),
    }
}

/// 便于脚本组装 headers：`http_headers(["K", "V", "K2", "V2", ...])`
pub fn http_headers(pairs: rhai::Array) -> Result<Map, Box<EvalAltResult>> {
    if pairs.len() % 2 != 0 {
        return Err(err_runtime(
            "http_headers 参数须成对：键1,值1,键2,值2,…",
        ));
    }
    let mut m = Map::new();
    let mut i = 0;
    while i + 1 < pairs.len() {
        let k = pairs[i]
            .clone()
            .into_string()
            .map_err(|_| err_runtime(format!("http_headers 第 {} 项须为字符串键", i + 1)))?;
        let v = header_value(&pairs[i + 1]);
        m.insert(k.into(), Dynamic::from(v));
        i += 2;
    }
    Ok(m)
}

pub fn today_ymd() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// 比较两个 `YYYY-MM-DD`：a<b → -1，相等 → 0，a>b → 1
pub fn ymd_cmp(a: &str, b: &str) -> Result<i64, Box<EvalAltResult>> {
    let da = parse_ymd(a)?;
    let db = parse_ymd(b)?;
    Ok(match da.cmp(&db) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })
}

/// 本地当前时刻 `HH:MM`（24 小时制）
pub fn now_hm() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

/// 本地当前小时 0–23
pub fn now_hour() -> i64 {
    chrono::Local::now().hour() as i64
}

/// 本地当前分钟 0–59
pub fn now_minute() -> i64 {
    chrono::Local::now().minute() as i64
}

/// 阻塞休眠秒数（最少 0；上限 86400）。`cancel` 置位时尽快返回「已取消」。
pub fn sleep_secs(secs: i64) -> Result<(), Box<EvalAltResult>> {
    sleep_secs_cancellable(secs, None)
}

pub fn sleep_secs_cancellable(
    secs: i64,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(), Box<EvalAltResult>> {
    use std::sync::atomic::Ordering;
    if secs < 0 {
        return Err(err_runtime("sleep_secs：秒数不能为负"));
    }
    let mut left_ms = (secs.min(86_400) as u64).saturating_mul(1000);
    while left_ms > 0 {
        if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
            return Err(Box::new(EvalAltResult::ErrorTerminated(
                Dynamic::from("已取消"),
                rhai::Position::NONE,
            )));
        }
        let slice = left_ms.min(200);
        std::thread::sleep(Duration::from_millis(slice));
        left_ms -= slice;
    }
    if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
        return Err(Box::new(EvalAltResult::ErrorTerminated(
            Dynamic::from("已取消"),
            rhai::Position::NONE,
        )));
    }
    Ok(())
}

/// 闭区间随机整数 `[lo, hi]`
pub fn rand_int(lo: i64, hi: i64) -> Result<i64, Box<EvalAltResult>> {
    if lo > hi {
        return Err(err_runtime(format!("rand_int：下限 {lo} 大于上限 {hi}")));
    }
    let span = (hi - lo) as u64;
    let r = rand_u64();
    Ok(lo + (r % (span + 1)) as i64)
}

fn rand_u64() -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut h);
    std::thread::current().id().hash(&mut h);
    h.finish().wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1)
}

fn parse_ymd(ymd: &str) -> Result<chrono::NaiveDate, Box<EvalAltResult>> {
    chrono::NaiveDate::parse_from_str(ymd.trim(), "%Y-%m-%d")
        .map_err(|_| err_runtime(format!("无效日期（须 YYYY-MM-DD）: {ymd}")))
}

/// `YYYY-MM-DD` 加减天数
pub fn ymd_add_days(ymd: &str, days: i64) -> Result<String, Box<EvalAltResult>> {
    let d = parse_ymd(ymd)?;
    let out = d
        .checked_add_signed(chrono::Duration::days(days))
        .ok_or_else(|| err_runtime(format!("日期溢出: {ymd} + {days}")))?;
    Ok(out.format("%Y-%m-%d").to_string())
}

/// 星期：1=周一 … 7=周日
pub fn ymd_weekday(ymd: &str) -> Result<i64, Box<EvalAltResult>> {
    use chrono::Datelike;
    let d = parse_ymd(ymd)?;
    Ok(d.weekday().number_from_monday() as i64)
}

/// 星期中文：周一…周日
pub fn ymd_weekday_cn(ymd: &str) -> Result<String, Box<EvalAltResult>> {
    let w = ymd_weekday(ymd)?;
    let names = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];
    Ok(names[(w as usize) - 1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip_slots_shape() {
        let raw = r#"{"successed":true,"result":{"slots":[{"classRoomName":"1号网球","cost":58.0,"apptInfo":{"canApptOrNot":true}}]}}"#;
        let d = json_parse(raw).unwrap();
        let m = d.try_cast::<Map>().unwrap();
        assert_eq!(m.get("successed").unwrap().as_bool().unwrap(), true);
        let result = m.get("result").unwrap().clone().try_cast::<Map>().unwrap();
        let slots = result.get("slots").unwrap().clone().into_array().unwrap();
        assert_eq!(slots.len(), 1);
    }

    #[test]
    fn http_headers_pairs() {
        let mut arr = rhai::Array::new();
        arr.push(Dynamic::from("A"));
        arr.push(Dynamic::from("1"));
        arr.push(Dynamic::from("B"));
        arr.push(Dynamic::from("2"));
        let m = http_headers(arr).unwrap();
        assert_eq!(map_str(m.clone(), "A"), "1");
        assert_eq!(map_str(m, "B"), "2");
    }

    #[test]
    fn http_get_public_json() {
        let body = http_get_simple("https://jsonplaceholder.typicode.com/todos/1");
        let body = match body {
            Ok(b) => b,
            Err(e) => {
                eprintln!("http_get skipped: {e}");
                return;
            }
        };
        let d = json_parse(&body).unwrap();
        let m = d.try_cast::<Map>().unwrap();
        assert!(m.get("id").is_some());
        assert!(m.get("title").is_some());
    }

    #[test]
    fn ymd_helpers_weekday_and_add() {
        assert_eq!(ymd_add_days("2026-09-17", 1).unwrap(), "2026-09-18");
        assert_eq!(ymd_weekday("2026-09-17").unwrap(), 4); // 周四
        assert_eq!(ymd_weekday_cn("2026-09-17").unwrap(), "周四");
        assert_eq!(ymd_weekday("2026-09-19").unwrap(), 6); // 周六
        let n = rand_int(300, 600).unwrap();
        assert!(n >= 300 && n <= 600);
    }
}
