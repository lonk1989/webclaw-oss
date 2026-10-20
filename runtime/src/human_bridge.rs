//! 人机协同桥：桌面弹窗 vs 伙伴空间文件交换。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanRequest {
    pub prompt_id: String,
    pub task_id: String,
    /// `image` | `slider` | `human`
    pub kind: String,
    pub hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_path: Option<String>,
    /// 图形验证码时可选；Skill 也可读 image_path
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_base64: Option<String>,
}

#[derive(Debug, Clone)]
pub enum HumanReply {
    /// 图形验证码：用户输入的文本
    Text(String),
    /// 人机/滑块：用户确认已完成
    Confirmed,
    Cancelled,
}

/// 文件系统人机桥（伙伴 Skill 轮询）。
///
/// 目录约定（`WEBCLAW_HUMAN_DIR` 或 run 下 `human/`）：
/// - 写出 `waiting_human.json` + 可选 `captcha.png`
/// - Skill 写回 `human_reply.txt`（验证码明文）或 `human_reply.json`：`{"ok":true}` / `{"cancel":true}`
pub struct FileHumanBridge {
    pub dir: PathBuf,
}

impl FileHumanBridge {
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var("WEBCLAW_HUMAN_DIR").ok()?;
        let dir = PathBuf::from(raw.trim());
        if dir.as_os_str().is_empty() {
            return None;
        }
        Some(Self { dir })
    }

    pub fn prepare(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("创建人机目录失败: {e}"))?;
        let _ = std::fs::remove_file(self.dir.join("human_reply.txt"));
        let _ = std::fs::remove_file(self.dir.join("human_reply.json"));
        let _ = std::fs::remove_file(self.dir.join("waiting_human.json"));
        Ok(())
    }

    pub fn write_request(&self, req: &HumanRequest, png: Option<&[u8]>) -> Result<PathBuf, String> {
        self.prepare()?;
        let mut req = req.clone();
        if let Some(bytes) = png {
            let img = self.dir.join("captcha.png");
            std::fs::write(&img, bytes).map_err(|e| format!("写验证码图失败: {e}"))?;
            req.image_path = Some(img.display().to_string());
        }
        let path = self.dir.join("waiting_human.json");
        // 收件箱不写 base64，避免助手把密文/大图读进对话
        let mut inbox_req = req.clone();
        inbox_req.image_base64 = None;
        let json = serde_json::to_string_pretty(&inbox_req).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| format!("写 waiting_human.json 失败: {e}"))?;
        let reply_file = self.dir.join("human_reply.txt");
        let inbox_image = publish_human_inbox(&req, png, &reply_file)?;
        let image_base64 = png.map(|b| {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(b)
        });
        crate::run_sink::emit_event(crate::run_sink::RunEvent::NeedHuman {
            kind: req.kind.clone(),
            prompt_id: req.prompt_id.clone(),
            hint: req.hint.clone(),
            image_path: req.image_path.clone(),
            reply_file: Some(reply_file.display().to_string()),
            inbox_image: inbox_image.clone(),
            image_base64,
        });
        crate::run_sink::emit_log(format!(
            "【需要人看】立刻把图片发到当前对话（不要只发文字、不要念路径）。图片={} 回复写入={}",
            inbox_image
                .as_deref()
                .or(req.image_path.as_deref())
                .unwrap_or("(无图)"),
            reply_file.display()
        ));
        Ok(path)
    }

    pub fn clear_request(&self) {
        let _ = std::fs::remove_file(self.dir.join("waiting_human.json"));
        clear_human_inbox();
    }

    pub async fn wait_reply(
        &self,
        cancel: &AtomicBool,
        timeout: Duration,
    ) -> Result<HumanReply, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.load(Ordering::SeqCst) {
                self.clear_request();
                return Err("已取消".into());
            }
            if Instant::now() >= deadline {
                self.clear_request();
                return Err("人机验证超时".into());
            }
            if let Some(r) = read_reply(&self.dir)? {
                self.clear_request();
                let _ = std::fs::remove_file(self.dir.join("human_reply.txt"));
                let _ = std::fs::remove_file(self.dir.join("human_reply.json"));
                return Ok(r);
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
}

fn read_reply(dir: &Path) -> Result<Option<HumanReply>, String> {
    let json_path = dir.join("human_reply.json");
    if json_path.is_file() {
        let raw = std::fs::read_to_string(&json_path).map_err(|e| e.to_string())?;
        let v: serde_json::Value =
            serde_json::from_str(raw.trim()).map_err(|e| format!("human_reply.json 无效: {e}"))?;
        if v.get("cancel").and_then(|x| x.as_bool()) == Some(true) {
            return Ok(Some(HumanReply::Cancelled));
        }
        if let Some(t) = v.get("text").and_then(|x| x.as_str()) {
            return Ok(Some(HumanReply::Text(t.trim().to_string())));
        }
        if v.get("ok").and_then(|x| x.as_bool()) == Some(true) {
            return Ok(Some(HumanReply::Confirmed));
        }
        return Ok(Some(HumanReply::Confirmed));
    }
    let txt_path = dir.join("human_reply.txt");
    if txt_path.is_file() {
        let raw = std::fs::read_to_string(&txt_path).map_err(|e| e.to_string())?;
        let t = raw.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("ok") || t == "完成" || t == "已完成" {
            return Ok(Some(HumanReply::Confirmed));
        }
        if t.eq_ignore_ascii_case("cancel") || t == "取消" {
            return Ok(Some(HumanReply::Cancelled));
        }
        return Ok(Some(HumanReply::Text(t.to_string())));
    }
    Ok(None)
}

fn human_inbox_json() -> PathBuf {
    crate::paths::app_data_dir().join("need_human.json")
}

fn human_inbox_image() -> PathBuf {
    crate::paths::app_data_dir().join("last-captcha.png")
}

fn publish_human_inbox(
    req: &HumanRequest,
    png: Option<&[u8]>,
    reply_file: &Path,
) -> Result<Option<String>, String> {
    let mut inbox_image = None;
    if let Some(bytes) = png {
        let img = human_inbox_image();
        std::fs::write(&img, bytes).map_err(|e| format!("写 last-captcha.png 失败: {e}"))?;
        inbox_image = Some(img.display().to_string());
    }
    let payload = serde_json::json!({
        "waiting": true,
        "kind": req.kind,
        "hint": req.hint,
        "promptId": req.prompt_id,
        "imagePath": req.image_path,
        "inboxImage": inbox_image,
        "replyFile": reply_file.display().to_string(),
    });
    std::fs::write(
        human_inbox_json(),
        serde_json::to_vec_pretty(&payload).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("写 need_human.json 失败: {e}"))?;
    Ok(inbox_image)
}

fn clear_human_inbox() {
    // 保留 last-captcha.png 供助手补发；把收件箱归档便于事后排查
    if let Ok(raw) = std::fs::read_to_string(human_inbox_json()) {
        let archive = crate::paths::app_data_dir().join("last-need-human.json");
        let _ = std::fs::write(archive, raw);
    }
    let _ = std::fs::remove_file(human_inbox_json());
}

/// 当前是否启用文件人机桥（伙伴空间）。
pub fn file_bridge_enabled() -> bool {
    FileHumanBridge::from_env().is_some()
}
