//! 跨步骤焦点锚点（输入 → 按键），仅任务执行期间内存持有

use crate::cdp::CdpClient;
use crate::commands::frame_eval;
use crate::commands::scripts;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// 最近成功 OOPIF 在此窗口内视为「热」：可跳过 SPA 就绪等待
const STICKY_OOPIF_WARM: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FocusAnchor {
    Structural {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
    },
    FieldHint {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
    },
}

impl FocusAnchor {
    pub fn target_id(&self) -> Option<&str> {
        match self {
            FocusAnchor::Structural { target_id, .. }
            | FocusAnchor::FieldHint { target_id, .. } => target_id.as_deref(),
        }
    }

    pub fn with_target_id(mut self, tid: Option<String>) -> Self {
        match &mut self {
            FocusAnchor::Structural { target_id, .. }
            | FocusAnchor::FieldHint { target_id, .. } => {
                *target_id = tid;
            }
        }
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FrameSurface {
    #[default]
    Unknown,
    Parent,
    Oopif,
}

#[derive(Debug, Default)]
pub struct FocusContext {
    pub last: Option<FocusAnchor>,
    /// 本任务最近一次成功命中的跨域 iframe target（续步优先探测）
    sticky_oopif_target_id: Option<String>,
    sticky_oopif_warm_at: Option<Instant>,
    /// 最近成功命中的表面：连续主页操作应优先打父页，避免先空等 sticky iframe
    last_surface: FrameSurface,
}

impl FocusContext {
    /// 记录成功命中的 frame。父页命中保留 sticky id（稍后还能回 iframe），但切换表面亲和性。
    pub fn remember_frame_hit(&mut self, target_id: &str, is_parent: bool) {
        if target_id.is_empty() {
            return;
        }
        if is_parent {
            self.last_surface = FrameSurface::Parent;
            return;
        }
        self.sticky_oopif_target_id = Some(target_id.to_string());
        self.sticky_oopif_warm_at = Some(Instant::now());
        self.last_surface = FrameSurface::Oopif;
    }

    pub fn sticky_oopif(&self) -> Option<(&str, bool)> {
        let tid = self.sticky_oopif_target_id.as_deref()?;
        let warm = self
            .sticky_oopif_warm_at
            .is_some_and(|t| t.elapsed() < STICKY_OOPIF_WARM);
        Some((tid, warm))
    }

    /// 用户切换了浏览器页签后，旧 sticky iframe 属于上一页，必须清掉以免坐标点到旧页
    pub fn clear_frame_affinity(&mut self) {
        self.sticky_oopif_target_id = None;
        self.sticky_oopif_warm_at = None;
        self.last_surface = FrameSurface::Unknown;
    }

    pub fn last_surface(&self) -> FrameSurface {
        self.last_surface
    }

    pub fn set_from_value(&mut self, v: &Value) {
        let Some(obj) = v.as_object() else {
            return;
        };
        let kind = obj
            .get("kind")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        let value = obj
            .get("value")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if value.is_empty() {
            return;
        }
        let target_id = obj
            .get("targetId")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        self.last = Some(match kind {
            "structural" => FocusAnchor::Structural { value, target_id },
            _ => FocusAnchor::FieldHint { value, target_id },
        });
    }

    pub fn to_json(&self) -> Value {
        match &self.last {
            Some(FocusAnchor::Structural { value, target_id }) => {
                let mut o = json!({ "kind": "structural", "value": value });
                if let Some(tid) = target_id {
                    o.as_object_mut()
                        .unwrap()
                        .insert("targetId".into(), json!(tid));
                }
                o
            }
            Some(FocusAnchor::FieldHint { value, target_id }) => {
                let mut o = json!({ "kind": "fieldHint", "value": value });
                if let Some(tid) = target_id {
                    o.as_object_mut()
                        .unwrap()
                        .insert("targetId".into(), json!(tid));
                }
                o
            }
            None => Value::Null,
        }
    }
}

fn anchor_json(anchor: &FocusAnchor) -> Value {
    match anchor {
        FocusAnchor::Structural { value, target_id } => {
            let mut o = json!({ "kind": "structural", "value": value });
            if let Some(tid) = target_id {
                o.as_object_mut()
                    .unwrap()
                    .insert("targetId".into(), json!(tid));
            }
            o
        }
        FocusAnchor::FieldHint { value, target_id } => {
            let mut o = json!({ "kind": "fieldHint", "value": value });
            if let Some(tid) = target_id {
                o.as_object_mut()
                    .unwrap()
                    .insert("targetId".into(), json!(tid));
            }
            o
        }
    }
}

fn wrap_helpers(expr: &str) -> String {
    format!(
        "(() => {{\n{}\n{}\nreturn ({});\n}})()",
        scripts::STRUCTURAL_TARGET_RESOLVER,
        scripts::FOCUS_HELPERS,
        expr
    )
}

pub async fn refocus_from_anchor(
    client: &CdpClient,
    anchor: &FocusAnchor,
) -> Result<Value, String> {
    let a = anchor_json(anchor);
    let a_str = serde_json::to_string(&a).map_err(|e| e.to_string())?;
    let expr = wrap_helpers(&format!("refocusFromAnchor({a_str})"));
    frame_eval::evaluate_on_target(client, anchor.target_id(), &expr).await
}

pub async fn sync_controlled_input_value(
    client: &CdpClient,
    text: &str,
) -> Result<Value, String> {
    sync_controlled_input_value_on(client, None, text).await
}

pub async fn sync_controlled_input_value_on(
    client: &CdpClient,
    target_id: Option<&str>,
    text: &str,
) -> Result<Value, String> {
    let text_json = serde_json::to_string(text).map_err(|e| e.to_string())?;
    let expr = wrap_helpers(&format!("syncControlledInputValue({text_json})"));
    frame_eval::evaluate_on_target(client, target_id, &expr).await
}

pub async fn describe_active_element(client: &CdpClient) -> Result<String, String> {
    describe_active_element_on(client, None).await
}

pub async fn describe_active_element_on(
    client: &CdpClient,
    target_id: Option<&str>,
) -> Result<String, String> {
    let v = frame_eval::evaluate_on_target(
        client,
        target_id,
        &wrap_helpers("describeActiveElement()"),
    )
    .await?;
    Ok(v.as_str().unwrap_or("?").to_string())
}

pub async fn try_generic_submit(client: &CdpClient) -> Result<Value, String> {
    try_generic_submit_on(client, None).await
}

pub async fn try_generic_submit_on(
    client: &CdpClient,
    target_id: Option<&str>,
) -> Result<Value, String> {
    let expr = wrap_helpers("tryGenericSubmitFromFocused()");
    frame_eval::evaluate_on_target(client, target_id, &expr).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_focus_anchor_from_script_value() {
        let mut ctx = FocusContext::default();
        ctx.set_from_value(&json!({ "kind": "structural", "value": "输入框#1", "targetId": "ABC" }));
        assert_eq!(
            ctx.last,
            Some(FocusAnchor::Structural {
                value: "输入框#1".into(),
                target_id: Some("ABC".into()),
            })
        );
        ctx.set_from_value(&json!({ "kind": "fieldHint", "value": "密码" }));
        assert_eq!(
            ctx.last,
            Some(FocusAnchor::FieldHint {
                value: "密码".into(),
                target_id: None,
            })
        );
    }
}
