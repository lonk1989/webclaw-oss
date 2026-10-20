//! 浏览器标签快照：供 AI 增强感知当前页面上下文

use crate::cdp::CdpClient;
use crate::chrome;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const PAGE_SIGNAL_JS: &str = r#"(() => {
  const isVisible = (el) => {
    if (!el) return false;
    const s = getComputedStyle(el);
    if (s.display === 'none' || s.visibility === 'hidden') return false;
    const r = el.getBoundingClientRect();
    return r.width >= 4 && r.height >= 4;
  };
  const inputs = [...document.querySelectorAll('input, textarea, [role="searchbox"], [role="combobox"]')]
    .filter(isVisible)
    .slice(0, 8);
  for (const el of inputs) {
    const ph = (el.placeholder || '').trim();
    const aria = (el.getAttribute('aria-label') || '').trim();
    const role = (el.getAttribute('role') || '').trim();
    if (ph || aria || role === 'searchbox') {
      return { placeholder: ph.slice(0, 80), aria: aria.slice(0, 80), role };
    }
  }
  return null;
})()"#;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserTabSnapshot {
    pub port: u16,
    pub target_id: String,
    pub title: String,
    pub url: String,
    pub hostname: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_signal: Option<String>,
}

pub type BrowserSnapshotSlot = Arc<Mutex<Option<BrowserTabSnapshot>>>;

pub fn site_hint_from_hostname(hostname: &str) -> Option<String> {
    let h = hostname.trim().trim_start_matches("www.").to_lowercase();
    if h.is_empty() || h == "about" {
        return None;
    }
    if h == "bilibili.com" || h.ends_with(".bilibili.com") {
        return Some("B站".into());
    }
    if h == "baidu.com" || h.ends_with(".baidu.com") {
        return Some("百度".into());
    }
    if h == "jd.com" || h.ends_with(".jd.com") {
        return Some("京东".into());
    }
    if h == "taobao.com" || h.ends_with(".taobao.com") {
        return Some("淘宝".into());
    }
    if h == "tmall.com" || h.ends_with(".tmall.com") {
        return Some("天猫".into());
    }
    if h == "zhihu.com" || h.ends_with(".zhihu.com") {
        return Some("知乎".into());
    }
    if h == "google.com" || h.ends_with(".google.com") {
        return Some("Google".into());
    }
    if h == "bing.com" || h.ends_with(".bing.com") {
        return Some("必应".into());
    }
    if h.contains("hcmcloud.cn") || h.ends_with(".hcmcloud.cn") {
        return Some("E-HR/HCM".into());
    }
    if h.contains("cleanpro") {
        return Some("中环洁E-HR".into());
    }
    if h.contains("feishu.cn") || h.ends_with(".feishu.cn") {
        return Some("飞书".into());
    }
    None
}

pub fn hostname_from_url(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_default()
}

pub fn snapshot_from_client(client: &CdpClient) -> BrowserTabSnapshot {
    BrowserTabSnapshot {
        port: client.port,
        target_id: client.target_id.clone(),
        title: client.target_title.clone(),
        url: client.target_url.clone(),
        hostname: hostname_from_url(&client.target_url),
        site_hint: site_hint_from_hostname(&hostname_from_url(&client.target_url)),
        page_signal: None,
    }
}

pub fn store_snapshot(slot: &BrowserSnapshotSlot, snap: BrowserTabSnapshot) {
    *slot.lock() = Some(snap);
}

pub fn cached_snapshot(slot: &BrowserSnapshotSlot) -> Option<BrowserTabSnapshot> {
    slot.lock().clone()
}

async fn enrich_page_signal(client: &CdpClient, mut snap: BrowserTabSnapshot) -> BrowserTabSnapshot {
    if !snap.url.starts_with("http://") && !snap.url.starts_with("https://") {
        return snap;
    }
    if let Ok(v) = client.evaluate(PAGE_SIGNAL_JS).await {
        if let Some(obj) = v.as_object() {
            let ph = obj.get("placeholder").and_then(|x| x.as_str()).unwrap_or("");
            let aria = obj.get("aria").and_then(|x| x.as_str()).unwrap_or("");
            let role = obj.get("role").and_then(|x| x.as_str()).unwrap_or("");
            let mut parts = Vec::new();
            if !ph.is_empty() {
                parts.push(format!("placeholder={ph}"));
            }
            if !aria.is_empty() {
                parts.push(format!("aria={aria}"));
            }
            if role == "searchbox" {
                parts.push("role=searchbox".into());
            }
            if !parts.is_empty() {
                snap.page_signal = Some(parts.join(", "));
            }
        }
    }
    snap
}

/// 从 CDP 实时采集当前标签快照（优先浏览器活动页签，其次 preferred，最后任意可用页）
pub async fn capture_live_snapshot(
    port: u16,
    preferred_target_id: Option<&str>,
) -> Result<BrowserTabSnapshot, String> {
    let client = match chrome::find_foreground_page_on(port).await {
        Ok(page) => CdpClient::connect_to_target_on(port, &page.id).await?,
        Err(_) => {
            if let Some(tid) = preferred_target_id.filter(|s| !s.is_empty()) {
                match CdpClient::connect_to_target_on(port, tid).await {
                    Ok(c) => c,
                    Err(_) => CdpClient::connect_on(port).await?,
                }
            } else {
                CdpClient::connect_on(port).await?
            }
        }
    };

    let mut snap = snapshot_from_client(&client);
    if snap.url.is_empty() || snap.url == "about:blank" {
        if let Ok(title) = client.evaluate("document.title").await {
            if let Some(t) = title.as_str() {
                snap.title = t.to_string();
            }
        }
        if let Ok(href) = client.evaluate("location.href").await {
            if let Some(u) = href.as_str() {
                snap.url = u.to_string();
                snap.hostname = hostname_from_url(u);
                snap.site_hint = site_hint_from_hostname(&snap.hostname);
            }
        }
    }
    Ok(enrich_page_signal(&client, snap).await)
}

/// 刷新快照：Chrome 未就绪时返回缓存
pub async fn refresh_browser_snapshot(
    slot: &BrowserSnapshotSlot,
    port: u16,
    preferred_target_id: Option<&str>,
) -> Option<BrowserTabSnapshot> {
    if !chrome::is_debug_port_ready_on(port).await {
        return cached_snapshot(slot);
    }
    match capture_live_snapshot(port, preferred_target_id).await {
        Ok(snap) => {
            store_snapshot(slot, snap.clone());
            Some(snap)
        }
        Err(_) => cached_snapshot(slot),
    }
}

pub fn format_browser_context_section(snap: &BrowserTabSnapshot) -> String {
    let mut lines = vec![
        format!("标签: {}", snap.title),
        format!("URL: {}", snap.url),
    ];
    if !snap.hostname.is_empty() {
        if let Some(ref hint) = snap.site_hint {
            lines.push(format!("站点: {hint} ({})", snap.hostname));
        } else {
            lines.push(format!("站点: {}", snap.hostname));
        }
    }
    if let Some(ref sig) = snap.page_signal {
        lines.push(format!("页面信号: {sig}"));
    }
    lines.join("\n")
}

pub fn context_summary_label(snap: Option<&BrowserTabSnapshot>, chat_turns: usize) -> String {
    let site = snap
        .and_then(|s| s.site_hint.as_deref())
        .or_else(|| snap.map(|s| s.hostname.as_str()))
        .unwrap_or("—");
    format!("上下文: {site} | 对话 {chat_turns} 轮")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_hint_bilibili_and_baidu() {
        assert_eq!(
            site_hint_from_hostname("www.bilibili.com"),
            Some("B站".into())
        );
        assert_eq!(
            site_hint_from_hostname("search.bilibili.com"),
            Some("B站".into())
        );
        assert_eq!(site_hint_from_hostname("baidu.com"), Some("百度".into()));
        assert_eq!(
            site_hint_from_hostname("cleanpro.hcmcloud.cn"),
            Some("E-HR/HCM".into())
        );
        assert_eq!(site_hint_from_hostname("internal.corp"), None);
    }

    #[test]
    fn format_browser_section_includes_url() {
        let snap = BrowserTabSnapshot {
            port: 9222,
            target_id: "t1".into(),
            title: "哔哩哔哩".into(),
            url: "https://www.bilibili.com/".into(),
            hostname: "www.bilibili.com".into(),
            site_hint: Some("B站".into()),
            page_signal: Some("placeholder=搜索".into()),
        };
        let s = format_browser_context_section(&snap);
        assert!(s.contains("bilibili.com"));
        assert!(s.contains("B站"));
        assert!(s.contains("placeholder=搜索"));
    }
}
