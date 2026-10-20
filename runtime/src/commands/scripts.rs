//! 内置能力默认 JavaScript（可在「能力」页编辑，持久化到用户目录）

use regex::Regex;
use std::sync::LazyLock;

/// 结构化目标：`按钮#2` / `表格#1行#1列#4` / `表格#1行#1图标按钮#2`（类型后直接跟 `#N`）
static STRUCTURAL_TARGET_RE: LazyLock<Regex> = LazyLock::new(|| {
    // 图标按钮须排在按钮前，避免误切
    Regex::new(r"^(?:(?:图标按钮|输入框|表格|按钮|行|列)#\d+)+$").unwrap()
});

/// 去掉步骤参数尾部分隔符（与菜谱拆步分隔符一致）
pub fn strip_trailing_step_separators(s: &str) -> &str {
    s.trim_end_matches(|c: char| {
        c.is_whitespace()
            || c == '\r'
            || matches!(c, ',' | '，' | '、' | ';' | '；' | '.' | '。' | '．')
    })
}

/// 是否为结构化执行目标 DSL（整段匹配）
pub fn is_structural_target(s: &str) -> bool {
    let s = strip_trailing_step_separators(s.trim());
    !s.is_empty() && STRUCTURAL_TARGET_RE.is_match(s)
}

/// 单段结构化目标：`图标按钮#15` → (类型, 全局序号)。多段路径（表格#1行#1…）返回 None。
pub fn parse_single_structural_segment(s: &str) -> Option<(String, u32)> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(图标按钮|输入框|表格|按钮|行|列)#(\d+)$").unwrap()
    });
    let caps = RE.captures(strip_trailing_step_separators(s.trim()))?;
    let ty = caps.get(1)?.as_str().to_string();
    let idx: u32 = caps.get(2)?.as_str().parse().ok()?;
    if idx < 1 {
        return None;
    }
    Some((ty, idx))
}

/// 解析「点击文字 / 点击文字#N」参数（不含结构化、不含「图标:」）。
/// 返回 (query, global_index)。
pub fn parse_click_text_args(args: &str) -> Option<(String, u32)> {
    let raw = strip_trailing_step_separators(args.trim());
    if raw.is_empty() || is_structural_target(raw) {
        return None;
    }
    let mut match_index = 1u32;
    let mut query = raw.to_string();
    if let Some(hash_pos) = query.rfind('#') {
        let after = &query[hash_pos + 1..];
        if !after.is_empty() && after.chars().all(|c| c.is_ascii_digit()) {
            let before = &query[..hash_pos];
            let mut bs = 0usize;
            for c in before.chars().rev() {
                if c == '\\' {
                    bs += 1;
                } else {
                    break;
                }
            }
            if bs % 2 == 0 {
                let n: u32 = after.parse().ok()?;
                if n < 1 {
                    return None;
                }
                match_index = n;
                query = before.trim_end().to_string();
            }
        }
    }
    query = query.replace("\\#", "#").replace("\\\\", "\\");
    query = strip_trailing_step_separators(&query).to_string();
    if query.is_empty() {
        return None;
    }
    // 「图标:xxx」走图标点击路径，不参与文字全局 #N
    if query.contains("图标:") || query.contains("图标：") {
        return None;
    }
    Some((query, match_index))
}

/// 各 frame 内点击文字候选目录（本地排序后的 score 列表），供 Rust 跨 frame 合成全局 #N。
pub fn compose_click_text_catalog(query: &str) -> String {
    let q = serde_json::to_string(query).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        "(() => {{ {ranking}\n const pack = __webclawRankClickTextHits({q});\n if (!pack.ok) return {{ ok: false, hits: [] }};\n return {{ ok: true, scanMode: pack.scanMode, hits: pack.ranked.map((h, i) => ({{ localIndex: i + 1, score: h.score, textScore: h.textScore, clickable: !!h.clickable }})) }}; }})()",
        ranking = CLICK_TEXT_RANKING,
        q = q
    )
}

/// 统计当前文档中某类型可见候选数量（与探针/点击同一套 collect + sort）
pub fn compose_structural_count(type_name: &str) -> String {
    let type_json = serde_json::to_string(type_name).unwrap_or_else(|_| "\"\"".to_string());
    let resolver = STRUCTURAL_TARGET_RESOLVER;
    format!(
        "(() => {{ {resolver}\n const type = {type_json};\n const n = orderStructuralCandidates(collectStructuralCandidates(type, document).filter(structuralElementVisible)).length;\n return {{ ok: true, type, count: n }}; }})()"
    )
}

/// 查找探针打标（缩放后序号仍绑在同一元素上）
pub fn compose_find_probe_stamp(label: &str) -> String {
    let label_json = serde_json::to_string(label).unwrap_or_else(|_| "\"\"".to_string());
    let resolver = STRUCTURAL_TARGET_RESOLVER;
    format!(
        "(() => {{ {resolver}\n const label = {label_json};\n const el = findByProbeStamp(label, document);\n return {{ ok: true, found: !!el }}; }})()"
    )
}

/// 「输入」命中只读/禁用输入框时的提示尾串（与 `INPUT_FIELD` 内文案一致）。
/// 刷新页面不会改变只读/禁用状态，据此跳过自动恢复重试。
pub const LOCKED_INPUT_HINT_TAIL: &str = "，请用点击";

/// 解析「输入」步骤 args → `(内容, 字段提示)`
///
/// 主写法：`字段=内容`（如 `密码=abc`、`月份=2026-05`、`输入框#2=关键词`）
/// 兼容：`内容到字段`、`内容|字段`（内部）
pub fn parse_input_step_args(args: &str) -> (String, String) {
    let args = args.trim();
    if args.is_empty() {
        return (String::new(), String::new());
    }

    if args.contains('|') && !args.contains('=') && !args.contains('到') {
        if let Some((text, field)) = args.split_once('|') {
            return (text.trim().to_string(), field.trim().to_string());
        }
    }

    if let Some((field, text)) = split_input_field_value(args) {
        return (text, field);
    }

    if let Some((text, field)) = args.split_once('到') {
        return (text.trim().to_string(), field.trim().to_string());
    }

    (args.to_string(), String::new())
}

/// 供输入脚本使用的 `内容|字段` 格式
pub fn format_input_script_args(args: &str) -> String {
    let (text, field) = parse_input_step_args(args);
    format!("{}|{}", text, field)
}

/// 「点击 x y」像素坐标（视口 client 坐标，单位 px；空格分隔）。
/// 无参数时视为 `0 0`（点击左上角，常用于关弹层点空白）。
pub fn parse_pixel_click_args(args: &str) -> Option<(f64, f64)> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(-?\d+(?:\.\d+)?)\s+(-?\d+(?:\.\d+)?)$").unwrap());
    let s = strip_trailing_step_separators(args.trim());
    if s.is_empty() {
        return Some((0.0, 0.0));
    }
    let caps = RE.captures(s)?;
    let x: f64 = caps.get(1)?.as_str().parse().ok()?;
    let y: f64 = caps.get(2)?.as_str().parse().ok()?;
    Some((x, y))
}

/// 「点击」无参：默认空白点 (0,0)，需同时打主文档与各 iframe
pub fn is_default_blank_pixel_click(args: &str) -> bool {
    strip_trailing_step_separators(args.trim()).is_empty()
}

/// 是否为像素坐标点击（含无参默认 0 0）
pub fn is_pixel_click_args(args: &str) -> bool {
    parse_pixel_click_args(args).is_some()
}

fn split_input_field_value(s: &str) -> Option<(String, String)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' {
            let mut backslashes = 0usize;
            let mut j = i;
            while j > 0 && bytes[j - 1] == b'\\' {
                backslashes += 1;
                j -= 1;
            }
            if backslashes % 2 == 0 {
                let field = s[..i].trim().to_string();
                let text = s[i + 1..].trim().to_string();
                return Some((field, text));
            }
        }
        i += 1;
    }
    None
}

/// 将共享定位模块注入能力脚本正文
pub fn compose_script(body: &str) -> String {
    format!("{STRUCTURAL_TARGET_RESOLVER}\n{body}")
}

/// 「点击」专用：额外注入点击文字 #N 排序模块（勿放进等待/输入等，避免 isDisplayed 等同名冲突）
pub fn compose_click_script(body: &str) -> String {
    format!("{STRUCTURAL_TARGET_RESOLVER}\n{CLICK_TEXT_RANKING}\n{body}")
}

fn click_text_probe_js() -> String {
    format!("{CLICK_TEXT_RANKING}\n{CLICK_TEXT_PROBE}")
}

/// 与页内 `PROBE_VERSION` 对齐；短调用据此判断是否需要注入整包模块。
pub const PROBE_API_VERSION: u32 = 28;

/// 点击文字探针 API 版本（install-once）。
pub const CLICK_TEXT_PROBE_API_VERSION: u32 = 7;

/// 仅安装探针模块到 `window.__webclawProbe`（整包只应在 needInstall 时发一次）。
pub fn compose_ensure_probe_installed() -> String {
    let module = probe_module_js();
    format!("(() => {{ {module} __webclawInstallProbe(); return {{ ok: true, installed: true }}; }})()")
}

/// 页面探针：扫描结构化候选（scope 可选）；search 可选；index_offsets 为跨 frame 连续编号起点（已占用数量）
/// 仅返回数据，不绘制 overlay（绘制请用 compose_probe_overlay）
pub fn compose_probe_page(
    scope: Option<&str>,
    search: Option<&str>,
    index_offsets: Option<&std::collections::HashMap<String, u32>>,
) -> String {
    compose_probe_page_filtered(scope, search, index_offsets, None)
}

/// 与 `compose_probe_page` 相同，可限定采集类型（如只要图标按钮/按钮/输入框，避免表格行撑爆）。
/// 短调用：不携带整包模块；未安装时返回 `{ needInstall: true }`，由 Rust 侧补装后再调。
pub fn compose_probe_page_filtered(
    scope: Option<&str>,
    search: Option<&str>,
    index_offsets: Option<&std::collections::HashMap<String, u32>>,
    types: Option<&[&str]>,
) -> String {
    let scope_json =
        serde_json::to_string(scope.unwrap_or("")).unwrap_or_else(|_| "\"\"".to_string());
    let search_json =
        serde_json::to_string(search.unwrap_or("")).unwrap_or_else(|_| "\"\"".to_string());
    let empty = std::collections::HashMap::new();
    let offsets_json = serde_json::to_string(index_offsets.unwrap_or(&empty))
        .unwrap_or_else(|_| "{}".to_string());
    let types_json = types
        .filter(|t| !t.is_empty())
        .map(|t| serde_json::to_string(t).unwrap_or_else(|_| "null".to_string()))
        .unwrap_or_else(|| "null".to_string());
    let ver = PROBE_API_VERSION;
    format!(
        "(() => {{ const p = window.__webclawProbe; if (!p || p.version !== {ver} || typeof p.scan !== 'function') return {{ ok: false, needInstall: true, items: [], countsByType: {{}} }}; return p.scan({{ scope: {scope_json}, search: {search_json}, indexOffsets: {offsets_json}, types: {types_json} }}); }})()"
    )
}

/// 页面探针：在页内绘制编号 overlay（payload 可含 indexOffsets 实现跨 frame 连续编号）
/// 短调用：悬停 reuseItems 时尤其不应再注入 ~50KB 模块。
pub fn compose_probe_overlay(payload: &serde_json::Value) -> String {
    let payload_json = payload.to_string();
    let ver = PROBE_API_VERSION;
    format!(
        "(() => {{ const p = window.__webclawProbe; if (!p || p.version !== {ver} || typeof p.apply !== 'function') return {{ ok: false, needInstall: true, items: [], countsByType: {{}} }}; const __p = {payload_json}; return p.apply({{ scope: __p.scope || '', search: __p.search || '', visibleTypes: __p.visibleTypes, highlight: __p.highlight, indexOffsets: __p.indexOffsets || {{}}, preserveIndices: true, overlayOnly: true, reuseItems: !!__p.reuseItems }}); }})()"
    )
}

/// 页面探针：轻量清除 overlay / 打标 / 缓存。
/// 禁止再注入整包 PROBE_MODULE：关闭时重装脚本会在多 iframe 下把内存继续抬高且难回落。
/// 已安装过探针时优先走 api.clear()，避免重复逻辑分叉。
pub fn compose_clear_probe_overlay() -> String {
    r#"(() => {
  const OVERLAY_ID = '__webclaw_probe_overlay__';
  const clearWin = (win) => {
    if (!win) return;
    try {
      if (win.__webclawProbe && typeof win.__webclawProbe.clear === 'function') {
        try { win.__webclawProbe.clear(); return; } catch (_) {}
      }
    } catch (_) {}
    try {
      win.document.querySelectorAll('[id="' + OVERLAY_ID + '"]').forEach((n) => {
        try { n.remove(); } catch (_) {}
      });
    } catch (_) {}
    try {
      win.document.querySelectorAll('[data-webclaw-probe]').forEach((el) => {
        try { el.removeAttribute('data-webclaw-probe'); } catch (_) {}
      });
    } catch (_) {}
    try { delete win.__webclawProbe; } catch (_) {
      try { win.__webclawProbe = null; } catch (_) {}
    }
  };
  const walk = (win) => {
    clearWin(win);
    let frames;
    try { frames = win.document.querySelectorAll('iframe'); } catch (_) { return; }
    for (const f of frames) {
      try {
        const cw = f.contentWindow;
        if (cw) walk(cw);
      } catch (_) {}
    }
  };
  try { walk(window); } catch (_) { clearWin(window); }
  return { ok: true };
})()"#
    .to_string()
}

pub const STRUCTURAL_TARGET_RESOLVER: &str = r#"
// structural-target-v19 — 列/单元格；EasyUI datagrid；参数尾部分隔符剥除；图标名多源 + 业务 class 兜底
// 编号不依赖视口裁剪；#N 按 DOM 文档顺序（窗口缩放折行不改序）；indexOffsets 跨 frame 连续
// data-webclaw-probe 打标：同会话内缩放只更新坐标；无打标时仍靠 DOM 序保证配方稳定
const STRUCTURAL_TYPES = '图标按钮|输入框|表格|按钮|行|列';
const PROBE_STAMP_ATTR = 'data-webclaw-probe';
const formatStructuralSegment = (index, type) => type + '#' + index;
const formatStructuralLabel = (path) => path.map((p) => formatStructuralSegment(p.index, p.type)).join('');

/** 去掉菜谱拆步残留的尾部分隔符（如「确定，」「按钮#2。」） */
const stripTrailingStepSeps = (s) => {
  let t = String(s || '').trim();
  while (t && /[,，、;；.。．]$/.test(t)) {
    t = t.slice(0, -1).trimEnd();
  }
  return t;
};

const clearProbeStamps = (root) => {
  try {
    const r = root && root.querySelectorAll ? root : document;
    r.querySelectorAll('[' + PROBE_STAMP_ATTR + ']').forEach((el) => {
      try { el.removeAttribute(PROBE_STAMP_ATTR); } catch (_) {}
    });
  } catch (_) {}
};

const readProbeStamp = (el) => {
  try {
    return (el && el.getAttribute && el.getAttribute(PROBE_STAMP_ATTR)) || '';
  } catch (_) {
    return '';
  }
};

const writeProbeStamp = (el, label) => {
  try {
    if (el && el.setAttribute) el.setAttribute(PROBE_STAMP_ATTR, label);
  } catch (_) {}
};

const findByProbeStamp = (label, root) => {
  if (!label) return null;
  try {
    const sel = '[' + PROBE_STAMP_ATTR + '="' + CSS.escape(label) + '"]';
    const scope = root && root.querySelector ? root : document;
    const hit = scope.querySelector(sel);
    if (hit && structuralElementVisible(hit)) return hit;
  } catch (_) {}
  return null;
};

const parseStructuralTarget = (raw) => {
  const s = stripTrailingStepSeps(raw || '');
  if (!s) return null;
  const path = [];
  // 主语法：按钮#2 / 表格#1行#1图标按钮#2（无需「中的」）
  const re = new RegExp('(' + STRUCTURAL_TYPES + ')#(\\d+)', 'g');
  let consumed = '';
  let m;
  while ((m = re.exec(s)) !== null) {
    if (m.index !== consumed.length) return null;
    const type = m[1];
    const index = parseInt(m[2], 10);
    if (!type || !Number.isFinite(index) || index < 1) return null;
    path.push({ index, type });
    consumed += m[0];
  }
  if (path.length === 0 || consumed !== s) return null;
  return { ok: true, path, label: formatStructuralLabel(path) };
};

/** 去掉全部空白并小写，用于 UI 文案匹配（「登 录」→「登录」） */
const normUiText = (s) => (s || '').trim().toLowerCase().replace(/\s+/g, '');

const structuralElementVisible = (el) => {
  if (!el || !(el instanceof Element)) return false;
  const s = window.getComputedStyle(el);
  if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false;
  // 禁用输入框常带 pointer-events:none，仍需被探针识别以便「点击」触发弹层
  if (s.pointerEvents === 'none') {
    const tag = el.tagName;
    const role = (el.getAttribute('role') || '').toLowerCase();
    const inputLike = tag === 'INPUT' || tag === 'TEXTAREA' || el.isContentEditable
      || ['textbox', 'searchbox', 'combobox'].includes(role);
    if (!inputLike) return false;
  }
  // 不含视口裁剪：离屏元素仍参与编号，滚动不会导致 #N 重排
  const r = el.getBoundingClientRect();
  return r.width >= 2 && r.height >= 2;
};

/** 视觉坐标排序：仅用于文字点击「第 N 个」等与阅读顺序相关的匹配 */
const sortByPosition = (els) => {
  const sx = window.scrollX || document.documentElement.scrollLeft || 0;
  const sy = window.scrollY || document.documentElement.scrollTop || 0;
  return [...els].sort((a, b) => {
    const ra = a.getBoundingClientRect();
    const rb = b.getBoundingClientRect();
    const dy = (ra.top + sy) - (rb.top + sy);
    if (Math.abs(dy) > 4) return dy;
    return (ra.left + sx) - (rb.left + sx);
  });
};

/**
 * 结构化 #N 的稳定顺序：DOM 文档顺序（compareDocumentPosition）。
 * 窗口缩放导致折行时视觉坐标会变，但 DOM 序不变，故「输入框#2」在大小窗口下指向同一控件。
 */
const sortByDocumentOrder = (els) => {
  return [...els].sort((a, b) => {
    if (a === b) return 0;
    if (!a || !b) return 0;
    const pos = a.compareDocumentPosition(b);
    if (pos & Node.DOCUMENT_POSITION_FOLLOWING) return -1;
    if (pos & Node.DOCUMENT_POSITION_PRECEDING) return 1;
    return 0;
  });
};

/** 结构化候选的标准编号顺序（探针 / 点击 / 输入 / 计数共用） */
const orderStructuralCandidates = (els) => sortByDocumentOrder(els);

/** 真正禁用：仅 IDL disabled / aria-disabled=true（供「输入」锁定，避免 class 误伤） */
const structuralIsTrulyDisabled = (el) => {
  if (!el || !(el instanceof Element)) return true;
  try {
    if (el.disabled === true) return true;
  } catch (_) {}
  try {
    if ((el.getAttribute && el.getAttribute('aria-disabled')) === 'true') return true;
  } catch (_) {}
  return false;
};

/** 探针/可点启发式：另含 class 中的 disabled 词（可能误标「禁用」，可接受） */
const structuralIsDisabled = (el) => {
  if (structuralIsTrulyDisabled(el)) return true;
  const cls = (el.className || '').toString();
  return /\bdisabled\b/i.test(cls);
};

const structuralIsReadonly = (el) => {
  if (!el || !(el instanceof Element)) return true;
  try {
    // IDL：<input readonly> / readonly="" 均为 true
    if (el.readOnly === true) return true;
  } catch (_) {}
  try {
    if (typeof el.hasAttribute === 'function' && el.hasAttribute('readonly')) return true;
  } catch (_) {}
  try {
    const attr = el.getAttribute && el.getAttribute('readonly');
    // 兼容只读属性存在但值为空串 / "readonly" / "true"
    if (attr != null) return true;
  } catch (_) {}
  try {
    if ((el.getAttribute && el.getAttribute('aria-readonly')) === 'true') return true;
  } catch (_) {}
  return false;
};

const structuralIsClickable = (el) => {
  if (!el || el === document.body || structuralIsDisabled(el)) return false;
  const tag = el.tagName;
  if (tag === 'A' || tag === 'BUTTON' || tag === 'SUMMARY' || tag === 'LABEL') return true;
  if (tag === 'INPUT' && ['submit', 'button', 'reset', 'image'].includes(((el.type || 'text') + '').toLowerCase())) return true;
  const role = (el.getAttribute('role') || '').toLowerCase();
  if (['button', 'link', 'menuitem', 'tab'].includes(role)) return true;
  if (el.hasAttribute('onclick') || el.hasAttribute('ng-click') || el.hasAttribute('data-ng-click')) return true;
  const cls = (el.className || '').toString();
  if (/\b(btn|button|clickable)\b/i.test(cls)) return true;
  return window.getComputedStyle(el).cursor === 'pointer';
};

const structuralIsIconButton = (el) => {
  if (!structuralIsClickable(el)) return false;
  let text = '';
  try { text = (el.innerText || '').trim(); } catch (_) {}
  if (text.length > 2) return false;
  if (el.querySelector('svg, img, i, [class*="icon"], [class*="Icon"]')) return true;
  const cls = (el.className || '').toString();
  if (/icon/i.test(cls)) return true;
  const aria = (el.getAttribute('aria-label') || '').trim();
  if (aria) return true;
  return text.length === 0;
};

const structuralIsInput = (el) => {
  if (!el || !(el instanceof Element)) return false;
  // 含只读/禁用：常见于日期/下拉触发器，探针与「点击输入框#N」需能命中
  const tag = el.tagName;
  if (tag === 'TEXTAREA') return true;
  if (tag === 'INPUT') {
    const type = ((el.type || 'text') + '').toLowerCase();
    if (['hidden', 'checkbox', 'radio', 'submit', 'button', 'reset', 'file', 'image'].includes(type)) return false;
    return true;
  }
  const role = (el.getAttribute('role') || '').toLowerCase();
  if (['textbox', 'searchbox', 'combobox'].includes(role)) return true;
  if (el.isContentEditable) return true;
  return false;
};

const structuralInputLockHint = (el) => {
  if (!el || !(el instanceof Element)) return '';
  if (structuralIsDisabled(el)) return '禁用';
  if (structuralIsReadonly(el)) return '只读';
  return '';
};

const structuralIsHeaderRow = (el) => {
  if (!el || !(el instanceof Element)) return false;
  if (el.classList.contains('datagrid-header-row')) return true;
  if (el.closest('.datagrid-header, .datagrid-htable, .datagrid-header-inner')) return true;
  if (el.closest('thead, .ant-table-thead, .el-table__header-wrapper, [role="rowgroup"]')) {
    const rg = el.closest('[role="rowgroup"]');
    if (!rg || rg.getAttribute('role') !== 'rowgroup') return true;
    if (rg.querySelector('[role="columnheader"]')) return true;
  }
  if (el.getAttribute('role') === 'columnheader') return true;
  if (el.classList.contains('ant-table-thead') || el.closest('.ant-table-thead')) return true;
  return false;
};

/** EasyUI 逻辑表格根（panel.datagrid），一张业务表只计一次 */
const isEasyuiDatagridRoot = (el) => {
  if (!el || !(el instanceof Element)) return false;
  if (el.matches?.('table.datagrid-f, table.easyui-datagrid')) return false;
  if (el.classList.contains('datagrid') || el.classList.contains('easyui-datagrid')) {
    // 取最外层 .datagrid，避免 wrap/view 内层重复
    const outer = el.closest?.('.panel.datagrid, .datagrid');
    return !outer || outer === el;
  }
  return false;
};

/** EasyUI 内部拆表：header/body/footer/冻结列/隐藏源表，不作独立「表格」 */
const isEasyuiInternalTable = (el) => {
  if (!el || !(el instanceof Element)) return false;
  if (el.matches?.('table.datagrid-htable, table.datagrid-btable, table.datagrid-ftable, table.datagrid-f')) {
    return true;
  }
  if (el.matches?.('table.easyui-datagrid')) return true;
  if (el.closest?.('.datagrid-view, .datagrid-view1, .datagrid-view2, .datagrid-header, .datagrid-body, .datagrid-footer, .datagrid-wrap')) {
    return el.tagName === 'TABLE' || el.getAttribute('role') === 'grid' || el.getAttribute('role') === 'table';
  }
  return false;
};

const isEasyuiFooterRow = (el) => {
  if (!el || !(el instanceof Element)) return false;
  return !!el.closest('.datagrid-footer, .datagrid-ftable, .datagrid-footer-inner');
};

/** EasyUI 冻结列 view1 与主视图 view2 行一一对应，只保留主视图，避免双计 */
const isEasyuiFrozenDuplicateRow = (el) => {
  if (!el || !(el instanceof Element)) return false;
  if (!el.closest('.datagrid-view1')) return false;
  const root = el.closest('.datagrid, .panel.datagrid');
  if (!root) return false;
  return !!root.querySelector('.datagrid-view2 .datagrid-btable tr.datagrid-row');
};

const dedupeByPosition = (els) => {
  const seen = new Set();
  const out = [];
  for (const el of els) {
    const r = el.getBoundingClientRect();
    const key = `${Math.round(r.left)}:${Math.round(r.top)}:${el.tagName}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(el);
  }
  return out;
};

const queryWithin = (scope, sel) => {
  try {
    if (scope === document || scope === document.documentElement) {
      return [...document.querySelectorAll(sel)];
    }
    if (scope.querySelectorAll) return [...scope.querySelectorAll(sel)];
  } catch (_) {}
  return [];
};

const collectTables = (scope) => {
  const easy = queryWithin(scope, '.panel.datagrid, div.datagrid, .datagrid.easyui-fluid')
    .filter((el) => isEasyuiDatagridRoot(el));
  const generic = queryWithin(
    scope,
    'table, [role="grid"], [role="table"], .ant-table, .el-table, .ant-table-wrapper, .el-table__body-wrapper'
  ).filter((el) => {
    if (isEasyuiInternalTable(el)) return false;
    if (el.closest?.('.panel.datagrid, div.datagrid')) return false;
    return true;
  });
  return dedupeByPosition([...easy, ...generic]);
};

const collectRows = (scope) => {
  const easyRows = queryWithin(
    scope,
    'tr.datagrid-row, .datagrid-btable tr, .datagrid-body tr'
  ).filter((el) => {
    if (structuralIsHeaderRow(el)) return false;
    if (isEasyuiFooterRow(el)) return false;
    if (isEasyuiFrozenDuplicateRow(el)) return false;
    // 只要数据体：btable / datagrid-body，排除仅挂在 footer 的空壳
    if (!el.closest('.datagrid-btable, .datagrid-body')) return false;
    if (el.closest('.datagrid-footer, .datagrid-header')) return false;
    return true;
  });
  const generic = queryWithin(
    scope,
    'tbody tr, [role="row"], .ant-table-row, .el-table__row'
  ).filter((el) => {
    if (el.closest?.('.datagrid, .panel.datagrid')) return false;
    if (structuralIsHeaderRow(el)) return false;
    return true;
  });
  return dedupeByPosition([...easyRows, ...generic]);
};

const isStructuralCell = (el) => {
  if (!el || !(el instanceof Element)) return false;
  const tag = el.tagName;
  if (tag === 'TD' || tag === 'TH') return true;
  const role = (el.getAttribute('role') || '').toLowerCase();
  return role === 'cell' || role === 'gridcell' || role === 'columnheader' || role === 'rowheader';
};

/** 隐藏列（EasyUI display:none / 零宽冻结空壳）不参与「列#N」 */
const structuralCellHidden = (el) => {
  if (!el || !(el instanceof Element)) return true;
  try {
    if (el.style && String(el.style.display || '').toLowerCase() === 'none') return true;
    const s = window.getComputedStyle(el);
    if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return true;
    const r = el.getBoundingClientRect();
    if (r.width < 2 || r.height < 2) return true;
  } catch (_) {
    return true;
  }
  return false;
};

const directCellsOfRow = (row) => {
  if (!row || !row.children) return [];
  const out = [];
  for (const child of row.children) {
    if (isStructuralCell(child) && !structuralCellHidden(child)) out.push(child);
  }
  if (out.length) return out;
  return queryWithin(row, 'td, th, [role="cell"], [role="gridcell"], [role="columnheader"], [role="rowheader"]')
    .filter((el) => !structuralCellHidden(el));
};

/** EasyUI 冻结列：view1/view2 同行按 datagrid-row-index 合并，左冻结在前 */
const easyuiPairedFrozenRow = (row) => {
  if (!row || !row.classList || !row.classList.contains('datagrid-row')) return null;
  const idx = row.getAttribute('datagrid-row-index');
  if (idx == null || idx === '') return null;
  const root = row.closest('.datagrid, .panel.datagrid');
  if (!root) return null;
  const sel = 'tr.datagrid-row[datagrid-row-index="' + CSS.escape(String(idx)) + '"]';
  if (row.closest('.datagrid-view2')) {
    return root.querySelector('.datagrid-view1 .datagrid-btable ' + sel)
      || root.querySelector('.datagrid-view1 .datagrid-body ' + sel);
  }
  if (row.closest('.datagrid-view1')) {
    return root.querySelector('.datagrid-view2 .datagrid-btable ' + sel)
      || root.querySelector('.datagrid-view2 .datagrid-body ' + sel);
  }
  return null;
};

const isStructuralRowEl = (el) => {
  if (!el || !(el instanceof Element)) return false;
  if (el.matches?.('tr, [role="row"]')) return true;
  if (el.classList?.contains('datagrid-row')) return true;
  if (el.classList?.contains('ant-table-row')) return true;
  if (el.classList?.contains('el-table__row')) return true;
  return false;
};

const collectCellsInRow = (row) => {
  if (!row) return [];
  const paired = easyuiPairedFrozenRow(row);
  let cells = [];
  if (paired && row.closest('.datagrid-view2')) {
    cells = [...directCellsOfRow(paired), ...directCellsOfRow(row)];
  } else if (paired && row.closest('.datagrid-view1')) {
    cells = [...directCellsOfRow(row), ...directCellsOfRow(paired)];
  } else {
    cells = directCellsOfRow(row);
  }
  return dedupeByPosition(cells);
};

/** 列 = 单元格；优先在「行」作用域内从左到右编号（跳过隐藏列） */
const collectCells = (scope) => {
  if (scope && scope !== document && scope !== document.documentElement && isStructuralRowEl(scope)) {
    return collectCellsInRow(scope);
  }
  const rows = collectRows(scope || document);
  const out = [];
  for (const row of rows) {
    out.push(...collectCellsInRow(row));
  }
  return dedupeByPosition(out);
};

const collectButtons = (scope) => {
  const sel = 'button, input[type="submit"], input[type="button"], input[type="reset"], [role="button"], a[role="button"], [onclick], [ng-click], [data-ng-click]';
  return dedupeByPosition(queryWithin(scope, sel).filter(structuralIsClickable));
};

const collectIconButtons = (scope) => {
  const sel = 'button, a, span, div, i, [role="button"], [onclick], [ng-click], [data-ng-click]';
  return dedupeByPosition(queryWithin(scope, sel).filter(structuralIsIconButton));
};

const collectInputs = (scope) => {
  const sel = 'input:not([type=hidden]):not([type=checkbox]):not([type=radio]):not([type=submit]):not([type=button]):not([type=reset]):not([type=file]), textarea, [role="textbox"], [role="searchbox"], [role="combobox"], [contenteditable="true"]';
  return dedupeByPosition(queryWithin(scope, sel).filter(structuralIsInput));
};

const collectStructuralCandidates = (type, scope) => {
  switch (type) {
    case '表格': return collectTables(scope);
    case '行': return collectRows(scope);
    case '列': return collectCells(scope);
    case '按钮': return collectButtons(scope);
    case '图标按钮': return collectIconButtons(scope);
    case '输入框': return collectInputs(scope);
    default: return [];
  }
};

const resolveStructuralTarget = (path, root = document) => {
  if (!path || !path.length) return null;
  // 优先按探针打标解析：缩放折行后序号仍指向同一元素
  const stamped = findByProbeStamp(formatStructuralLabel(path), root);
  if (stamped) return stamped;
  let scope = root;
  let result = null;
  for (let i = 0; i < path.length; i++) {
    const { index, type } = path[i];
    const candidates = orderStructuralCandidates(collectStructuralCandidates(type, scope).filter(structuralElementVisible));
    const el = candidates[index - 1] || null;
    if (!el) return null;
    result = el;
    // 表格/行/列均可作下一层作用域（如 表格#1行#1列#4输入框#1）
    if (type === '表格' || type === '行' || type === '列') {
      scope = el;
    }
  }
  return result;
};

const countStructuralCandidates = (path, root = document) => {
  if (!path || !path.length) return 0;
  let scope = root;
  for (let i = 0; i < path.length - 1; i++) {
    const { index, type } = path[i];
    const candidates = orderStructuralCandidates(collectStructuralCandidates(type, scope).filter(structuralElementVisible));
    const el = candidates[index - 1] || null;
    if (!el) return 0;
    scope = el;
  }
  const last = path[path.length - 1];
  return orderStructuralCandidates(collectStructuralCandidates(last.type, scope).filter(structuralElementVisible)).length;
};

const structuralClassName = (el) => {
  if (!el) return '';
  try {
    const raw = el.className;
    const s = (raw && typeof raw === 'object' && raw.baseVal != null)
      ? String(raw.baseVal)
      : String(raw || '');
    return s.trim().replace(/\s+/g, ' ').slice(0, 160);
  } catch (_) {
    return '';
  }
};

const structuralTextSnippet = (el) => {
  if (!el) return '';
  try {
    return (el.innerText || '').trim().replace(/\s+/g, ' ').slice(0, 32);
  } catch (_) {
    return '';
  }
};

const describeStructuralElement = (el) => {
  if (!el) return '';
  const tag = el.tagName || '';
  const cls = structuralClassName(el).split(/\s+/).slice(0, 2).join('.');
  const aria = (el.getAttribute && el.getAttribute('aria-label')) || '';
  const ph = (el.placeholder || '').trim();
  const lock = structuralInputLockHint(el);
  return [tag, cls, aria || ph, lock].filter(Boolean).join(' ').slice(0, 80);
};

/** 生成探针「指令文本」用的语义标签（去掉易破坏指令的分隔符） */
const sanitizeProbeCmdLabel = (s) => {
  return (s || '')
    .replace(/\s+/g, ' ')
    .replace(/[=，,。.\n\r\t]/g, '')
    .trim()
    .slice(0, 40);
};

const probeLabelForInput = (el) => {
  if (!el) return '';
  const aria = (el.getAttribute('aria-label') || '').trim();
  const ph = (el.placeholder || '').trim();
  const title = (el.getAttribute('title') || '').trim();
  const name = (el.name || '').trim();
  let label = '';
  try {
    if (el.id) {
      const lb = document.querySelector('label[for="' + CSS.escape(el.id) + '"]');
      if (lb) label = (lb.innerText || lb.textContent || '').trim();
    }
  } catch (_) {}
  if (!label) {
    try {
      const wrap = el.closest('label');
      if (wrap) label = (wrap.innerText || wrap.textContent || '').trim();
    } catch (_) {}
  }
  return sanitizeProbeCmdLabel(aria || ph || title || label || name);
};

const probeLabelForClickable = (el) => {
  if (!el) return '';
  let text = '';
  try { text = (el.innerText || '').trim(); } catch (_) {}
  if (text.length > 40) text = '';
  const aria = (el.getAttribute('aria-label') || '').trim();
  const title = (el.getAttribute('title') || '').trim();
  const value = ((el.value || '') + '').trim();
  return sanitizeProbeCmdLabel(text || aria || title || value);
};

/**
 * 图标名提取（多源，按分数择优）：
 * class 库前缀 / data-icon / SVG symbol / 图片文件名 / material 字形 / aria|title|alt
 * 仍无结果时：元素自身业务/修饰 class 兜底（非库壳；如 el-upload--picture-card）
 * 「点击图标:xxx」带冒号；执行侧只比最终抽取名（与探针一致）。
 */
const ICON_CLASS_PATTERNS = [
  { re: /^anticon[-_](.+)$/i, score: 100 },
  { re: /^el-icon[-_](.+)$/i, score: 95 },
  { re: /^arco-icon[-_](.+)$/i, score: 92 },
  { re: /^semi-icons?[-_](.+)$/i, score: 92 },
  { re: /^van-icon[-_](.+)$/i, score: 90 },
  { re: /^ivu-icon[-_](.+)$/i, score: 90 },
  { re: /^t-icon[-_](.+)$/i, score: 88 },
  { re: /^n-icon[-_](.+)$/i, score: 88 },
  { re: /^iconpark-icon[-_](.+)$/i, score: 88 },
  { re: /^iconfont[-_](.+)$/i, score: 85 },
  { re: /^glyphicon[-_](.+)$/i, score: 80 },
  { re: /^mdi[-_](.+)$/i, score: 78 },
  { re: /^bi[-_](.+)$/i, score: 75 },
  { re: /^bx[-_](.+)$/i, score: 75 },
  { re: /^fa[srlb]?[-_](.+)$/i, score: 72 },
  { re: /^ri[-_](.+)$/i, score: 70 },
  { re: /^icon[-_](.+)$/i, score: 65 },
  // 企业/HCM 等：xxx-icon-screening / btn-icon-close
  { re: /^[a-z][\w]*-icon[-_](.+)$/i, score: 58 },
  // close-icon / delete-icon
  { re: /^(.+)[-_]icons?$/i, score: 50 },
];
const GENERIC_ICON_NAME_PARTS = new Set([
  'wrapper', 'box', 'btn', 'button', 'inner', 'svg', 'font', 'container', 'wrap',
  'item', 'hover', 'active', 'disabled', 'spin', 'loading', 'icon', 'icons',
  'default', 'primary', 'success', 'warning', 'danger', 'info', 'error', 'link',
  'small', 'large', 'mini', 'tiny', 'medium', 'size', 'color', 'theme',
  'left', 'right', 'top', 'bottom', 'start', 'end', 'center', 'middle',
  'sm', 'md', 'lg', 'xl', 'xs', 'xxl', 'fill', 'outline', 'solid', 'regular',
  'light', 'dark', 'white', 'black', 'red', 'blue', 'green', 'gray', 'grey',
  'anticon', 'el', 'arco', 'semi', 'van', 'ivu', 'material', 'symbols',
  'outlined', 'rounded', 'sharp', 'twotone', 'two-tone', 'filled',
  // 库壳整词（无具体图标名）：不能用于「点击图标:」定位
  'el-icon', 'arco-icon', 'van-icon', 'ivu-icon', 'n-icon', 't-icon',
  'semi-icon', 'semi-icons', 'iconfont', 'glyphicon', 'iconpark-icon',
  'material-icons', 'material-symbols', 'material-symbols-outlined',
  'material-symbols-rounded', 'material-symbols-sharp',
]);
/** 库/组件壳 class：只有前缀没有具体名（如 el-icon、anticon） */
const isLibraryShellClass = (token) => {
  const n = normalizeIconName(token);
  if (!n) return true;
  if (GENERIC_ICON_NAME_PARTS.has(n)) return true;
  if (/^(anticon|el-icon|arco-icon|semi-icons?|van-icon|ivu-icon|iconfont|glyphicon|n-icon|t-icon|iconpark-icon|material-icons|material-symbols(?:-outlined|-rounded|-sharp)?)$/i.test(n)) {
    return true;
  }
  // el-icons / fa / mdi 等仅库名
  if (/^(el|ant|arco|van|ivu|fa|fas|far|fal|fab|mdi|bi|bx|ri)-?icons?$/i.test(n)) return true;
  return false;
};

const MATERIAL_ICON_CLASS_RE = /\b(material-icons|material-symbols(?:-outlined|-rounded|-sharp)?)\b/i;

const normalizeIconName = (s) => (s || '')
  .toString()
  .trim()
  .toLowerCase()
  .replace(/_/g, '-')
  .replace(/\s+/g, '');

const isUsableIconName = (name) => {
  const n = normalizeIconName(name);
  if (!n || GENERIC_ICON_NAME_PARTS.has(n)) return false;
  if (isLibraryShellClass(n)) return false;
  if (n.length < 2 || n.length > 48) return false;
  if (/^\d+$/.test(n)) return false;
  return true;
};

const iconNamesEqual = (a, b) => {
  const na = normalizeIconName(a);
  const nb = normalizeIconName(b);
  return !!na && na === nb;
};

const extractIconNameFromToken = (token) => {
  if (!token) return { name: '', score: 0 };
  for (const { re, score } of ICON_CLASS_PATTERNS) {
    const m = token.match(re);
    if (!m || !m[1]) continue;
    const name = normalizeIconName(m[1]);
    if (!isUsableIconName(name)) continue;
    return { name, score };
  }
  return { name: '', score: 0 };
};

/** 裸 class 如 close-circle / search-outlined（落在 icon 子节点上时） */
const extractBareIconToken = (token) => {
  if (!token) return { name: '', score: 0 };
  const t = token.toString();
  if (/^(anticon|el-icon|arco-icon|semi-icons?|van-icon|ivu-icon|iconfont|glyphicon|material|n-icon|t-icon)([-_]|$)/i.test(t)) {
    return { name: '', score: 0 };
  }
  if (ICON_CLASS_PATTERNS.some(({ re }) => re.test(t))) return { name: '', score: 0 };
  const name = normalizeIconName(t);
  if (!isUsableIconName(name)) return { name: '', score: 0 };
  // 需像图标标识：含连字符，或 3–24 位字母数字
  if (!/-/.test(name) && !/^[a-z][a-z0-9]{2,23}$/i.test(name)) return { name: '', score: 0 };
  if (/^(hover|focus|selected|checked|active|disabled|opened|closed)$/i.test(name)) {
    return { name: '', score: 0 };
  }
  return { name, score: /-/.test(name) ? 42 : 28 };
};

const extractBestIconNameFromClass = (cls, allowBare) => {
  let best = { name: '', score: 0 };
  const tokens = (cls || '').toString().split(/\s+/).filter(Boolean);
  for (const t of tokens) {
    const hit = extractIconNameFromToken(t);
    if (hit.name && hit.score > best.score) best = hit;
  }
  if (allowBare) {
    for (const t of tokens) {
      const hit = extractBareIconToken(t);
      if (hit.name && hit.score > best.score) best = hit;
    }
  }
  return best;
};

const considerIconHint = (best, name, score) => {
  const n = normalizeIconName(name);
  if (!isUsableIconName(n)) return best;
  if (score > best.score) return { name: n, score };
  return best;
};

const extractIconNameFromHrefOrSrc = (raw) => {
  const s = (raw || '').toString().trim();
  if (!s) return { name: '', score: 0 };
  // #icon-close / .../icons/delete.svg / sprite.svg#close-circle
  let m = s.match(/[#/](?:icon[-_])?([^#/?=]+?)(?:\.(?:svg|png|jpg|jpeg|gif|webp))?$/i);
  if (!m) m = s.match(/([^/#?]+)\.(?:svg|png|jpg|jpeg|gif|webp)(?:\?|$)/i);
  if (!m || !m[1]) return { name: '', score: 0 };
  const fromToken = extractIconNameFromToken(m[1]);
  if (fromToken.name) return { name: fromToken.name, score: Math.max(fromToken.score - 10, 35) };
  const bare = extractBareIconToken(m[1]);
  if (bare.name) return { name: bare.name, score: Math.max(bare.score, 32) };
  const rawName = normalizeIconName(m[1]);
  if (isUsableIconName(rawName)) return { name: rawName, score: 30 };
  return { name: '', score: 0 };
};

/**
 * 无明确图标名时：从元素自身 class 选可定位的业务/修饰 token（非库壳）。
 * 分数低于真实图标名，且仅在 best 仍空时启用。
 */
const scoreClassFallbackToken = (token) => {
  if (!token) return { name: '', score: 0 };
  const name = normalizeIconName(token);
  if (!isUsableIconName(name) || isLibraryShellClass(name)) return { name: '', score: 0 };
  // 工具/状态前缀
  if (/^(is|has|no|js|css|v|data)[-_]/.test(name)) return { name: '', score: 0 };
  if (/^(hover|focus|selected|checked|active|disabled|opened|closed|visible|hidden)$/i.test(name)) {
    return { name: '', score: 0 };
  }
  // 过短或纯数字段
  if (name.length < 4) return { name: '', score: 0 };
  let score = 8;
  if (/--/.test(name)) score = 22; // BEM 修饰：el-upload--picture-card
  else if ((name.match(/-/g) || []).length >= 2) score = 18; // a-b-c / picture-upload-default
  else if (/-/.test(name)) score = 14;
  if (name.length >= 16) score += 2;
  else if (name.length >= 10) score += 1;
  return { name, score };
};

/** 仅看元素自身 class 列表，择优一个兜底名 */
const extractClassFallbackHint = (cls) => {
  let best = { name: '', score: 0 };
  const tokens = (cls || '').toString().split(/\s+/).filter(Boolean);
  for (const t of tokens) {
    const hit = scoreClassFallbackToken(t);
    if (hit.name && hit.score > best.score) best = hit;
  }
  return best;
};

const extractIconHintFromElement = (el) => {
  if (!el || !(el instanceof Element)) return '';
  let best = { name: '', score: 0 };
  const considerCls = (cls, allowBare) => {
    const hit = extractBestIconNameFromClass(cls, allowBare);
    if (hit.name && hit.score > best.score) best = hit;
  };
  const considerAttr = (node, score) => {
    if (!node || !node.getAttribute) return;
    try {
      for (const key of ['data-icon', 'data-icon-name', 'data-iconname', 'data-name']) {
        const v = (node.getAttribute(key) || '').trim();
        if (v) best = considerIconHint(best, v, score);
      }
    } catch (_) {}
  };
  const considerAria = (node, score) => {
    if (!node || !node.getAttribute) return;
    try {
      const aria = (node.getAttribute('aria-label') || '').trim();
      const title = (node.getAttribute('title') || '').trim();
      const alt = (node.getAttribute('alt') || '').trim();
      // 过长文案不像图标标识
      for (const v of [aria, title, alt]) {
        if (!v || v.length > 24) continue;
        if (/\s/.test(v) && v.length > 12) continue;
        best = considerIconHint(best, v, score);
      }
    } catch (_) {}
  };

  considerCls(structuralClassName(el), false);
  considerAttr(el, 82);
  considerAria(el, 48);

  try {
    el.querySelectorAll('svg, i, span, use, img, [class*="icon"], [class*="Icon"], [data-icon]').forEach((node) => {
      const onIconish = (() => {
        try {
          const cls = structuralClassName(node);
          if (/icon/i.test(cls) || MATERIAL_ICON_CLASS_RE.test(cls)) return true;
          if (node.tagName === 'I' || node.tagName === 'SVG' || node.tagName === 'USE' || node.tagName === 'IMG') return true;
          if (node.getAttribute && node.getAttribute('data-icon')) return true;
        } catch (_) {}
        return false;
      })();
      considerCls(structuralClassName(node), onIconish);
      considerAttr(node, 80);
      considerAria(node, 46);
      try {
        if (MATERIAL_ICON_CLASS_RE.test(structuralClassName(node))) {
          const glyph = ((node.textContent || '') + '').trim().replace(/\s+/g, '');
          if (glyph && glyph.length <= 32) best = considerIconHint(best, glyph, 74);
        }
      } catch (_) {}
      try {
        if (node.tagName === 'IMG') {
          const srcHit = extractIconNameFromHrefOrSrc(node.getAttribute('src') || '');
          if (srcHit.name) best = considerIconHint(best, srcHit.name, srcHit.score);
        }
      } catch (_) {}
      try {
        const use = node.tagName === 'USE' ? node : (node.querySelector ? node.querySelector('use') : null);
        if (use) {
          const href = use.getAttribute('href') || use.getAttribute('xlink:href') || '';
          const hrefHit = extractIconNameFromHrefOrSrc(href);
          if (hrefHit.name) best = considerIconHint(best, hrefHit.name, hrefHit.score);
        }
      } catch (_) {}
      try {
        if (node.tagName === 'SVG') {
          const labelled = (node.getAttribute('aria-label') || node.getAttribute('aria-labelledby') || '').trim();
          if (labelled && !/\s/.test(labelled) && labelled.length <= 24) {
            best = considerIconHint(best, labelled, 44);
          }
          const titleEl = node.querySelector('title');
          if (titleEl) {
            const t = ((titleEl.textContent || '') + '').trim();
            if (t && t.length <= 24) best = considerIconHint(best, t, 40);
          }
        }
      } catch (_) {}
    });
  } catch (_) {}

  // 无更高质量名时：仅用本元素 class 兜底（不扫子孙，避免串台）
  if (!best.name) {
    const fb = extractClassFallbackHint(structuralClassName(el));
    if (fb.name) best = fb;
  }
  return best.name;
};

/**
 * 探针/配方专用：「图标: name」或「图标：name」（中英文冒号），勿与普通文字点击混淆。
 * 多个同名时追加 #N（与 findIconButtonHits 同序）。
 */
const formatIconClickCmd = (name, matchIndex, totalSame) => {
  const n = normalizeIconName(name);
  if (!n) return null;
  // 保留连字符；去掉易破坏步骤分隔的字符
  const safe = n.replace(/[=，,。.\n\r\t]/g, '');
  if (!safe) return null;
  const idx = Number(matchIndex) || 1;
  const many = Number(totalSame) > 1;
  const suffix = many ? ('#' + idx) : (idx > 1 ? ('#' + idx) : '');
  return '点击图标:' + safe + suffix;
};

/** 有可解析图标标识时生成「图标:」指令；单次定位仍可用（点击执行路径） */
const buildIconProbeTextCmd = (el, scope) => {
  const iconName = extractIconHintFromElement(el);
  if (!iconName) return null;
  const hits = findIconButtonHits(iconName, scope);
  let at = hits.indexOf(el);
  if (at < 0) {
    try {
      const r = el.getBoundingClientRect();
      at = hits.findIndex((h) => {
        const hr = h.getBoundingClientRect();
        return Math.abs(hr.left - r.left) < 1 && Math.abs(hr.top - r.top) < 1 && h.tagName === el.tagName;
      });
    } catch (_) { at = -1; }
  }
  const matchIndex = at >= 0 ? at + 1 : 1;
  return formatIconClickCmd(iconName, matchIndex, hits.length || 1);
};

/**
 * 探针批量生成图标指令：每个元素只 extract 一次，按 DOM 序算同名 #N。
 * 避免对每个图标再 findIconButtonHits（O(n²) 会卡死/撑爆页面）。
 * 库壳名（el-icon 等）不生成「点击图标:」，改用结构序号。
 */
const buildIconProbeTextCmdsForEls = (els) => {
  const list = Array.isArray(els) ? els : [];
  const nameByEl = new Map();
  for (const el of list) {
    if (!el || nameByEl.has(el)) continue;
    const raw = extractIconHintFromElement(el) || '';
    // 壳名不可用：宁可用结构序号 图标按钮#N
    nameByEl.set(el, (raw && !isLibraryShellClass(raw)) ? raw : '');
  }
  const counts = Object.create(null);
  for (const n of nameByEl.values()) {
    if (!n) continue;
    counts[n] = (counts[n] || 0) + 1;
  }
  const ordered = orderStructuralCandidates(list.filter(Boolean));
  const ordinalByEl = new Map();
  const seen = Object.create(null);
  for (const el of ordered) {
    if (ordinalByEl.has(el)) continue;
    const n = nameByEl.get(el) || '';
    if (!n) continue;
    seen[n] = (seen[n] || 0) + 1;
    ordinalByEl.set(el, { name: n, matchIndex: seen[n], total: counts[n] || 1 });
  }
  return list.map((el) => {
    const hit = ordinalByEl.get(el);
    if (!hit) return null;
    return formatIconClickCmd(hit.name, hit.matchIndex, hit.total);
  });
};

const iconNameMatches = (el, iconName) => {
  const want = normalizeIconName(iconName);
  if (!want || !el) return false;
  // 只比最终抽取名（与探针 buildIconProbeTextCmdsForEls 一致）：
  // 有 icon-plus 时最终名是 plus，不会因同有 picture-upload_default 误入该名命中列表。
  return iconNamesEqual(extractIconHintFromElement(el), want);
};

const findIconButtonHits = (iconName, scope) => {
  const q = (iconName || '').trim();
  if (!q) return [];
  const root = scope && scope.querySelectorAll ? scope : document;
  return orderStructuralCandidates(
    collectIconButtons(root).filter((el) => structuralElementVisible(el) && iconNameMatches(el, q))
  );
};

/**
 * 仅识别「图标:xxx / 图标：xxx」——必须带中/英文冒号，避免「点击图标按钮#1」或普通文案误入。
 * 返回 null = 非图标语法，交给文字/结构化路径。
 */
const parseIconClickArgs = (raw) => {
  let matchIndex = 1;
  let query = stripTrailingStepSeps(raw || '');
  const idxMatch = query.match(/(?<!\\)#(\d+)$/);
  if (idxMatch) {
    matchIndex = parseInt(idxMatch[1], 10);
    if (!Number.isFinite(matchIndex) || matchIndex < 1) {
      return { ok: false, error: `序号无效：#${idxMatch[1]}（须为 ≥1 的整数）` };
    }
    query = query.slice(0, -idxMatch[0].length).trimEnd();
  }
  query = query.replace(/\\#/g, '#').replace(/\\\\/g, '\\');
  // 必须冒号；允许「图标:name」或「图标: name」
  const m = query.match(/^图标\s*[:：]\s*(.+)$/);
  if (!m) return null;
  const iconName = normalizeIconName(m[1]);
  if (!iconName) return { ok: false, error: '图标名不能为空（写法：点击图标: close-circle）' };
  if (isLibraryShellClass(iconName)) {
    return {
      ok: false,
      error: `「${iconName}」是图标库壳 class，不能用于定位。请改用探针的「点击图标按钮#N」，或带具体名的写法（如 点击图标:el-icon-search）`,
    };
  }
  return { ok: true, iconName, matchIndex };
};

  const buildProbeTextCmd = (el, type, scope) => {
  if (type === '输入框') {
    const label = probeLabelForInput(el);
    return label ? ('输入' + label + '=') : null;
  }
  if (type === '图标按钮') {
    return buildIconProbeTextCmd(el, scope);
  }
  if (type === '按钮') {
    const label = probeLabelForClickable(el);
    return label ? ('点击' + label) : null;
  }
  return null;
};

/** 是否为真正挡操作的遮罩（避免 OA 等页面 class 含 loading 的装饰节点空等 10s） */
const isActiveBlockingOverlay = (el, trusted) => {
  if (!el || !(el instanceof Element)) return false;
  const s = window.getComputedStyle(el);
  if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false;
  if (s.pointerEvents === 'none') return false;
  const r = el.getBoundingClientRect();
  if (r.width < 40 || r.height < 40) return false;
  if (!(r.bottom > 0 && r.right > 0 && r.top < window.innerHeight && r.left < window.innerWidth)) return false;
  if (trusted) return true;
  const pos = s.position;
  if (pos !== 'fixed' && pos !== 'absolute') return false;
  const area = r.width * r.height;
  const vp = Math.max(1, window.innerWidth * window.innerHeight);
  if (area / vp < 0.12) return false;
  const z = parseInt(s.zIndex || '0', 10);
  if (Number.isFinite(z) && z > 0 && z < 5) return false;
  return true;
};

const hasBlockingOverlay = () => {
  const specific =
    '.el-loading-mask, .el-loading-spinner, .ant-spin-spinning, .ant-modal-mask, .n-spin-container .n-base-loading, [class*="loading-mask"], [class*="LoadingMask"]';
  try {
    for (const el of document.querySelectorAll(specific)) {
      if (isActiveBlockingOverlay(el, true)) return true;
    }
  } catch (_) {}
  try {
    for (const el of document.querySelectorAll('[class*="loading"], [class*="Loading"], [class*="spinner"], [class*="Spinner"]')) {
      if (isActiveBlockingOverlay(el, false)) return true;
    }
  } catch (_) {}
  return false;
};

const waitForBlockingMaskGone = async (timeoutSec = 2.5) => {
  if (!hasBlockingOverlay()) return;
  const deadline = Date.now() + timeoutSec * 1000;
  while (Date.now() < deadline) {
    try { if (window.__webclawCancelRequested) return; } catch (_) {}
    if (!hasBlockingOverlay()) return;
    await new Promise((r) => setTimeout(r, 150));
  }
};
"#;

/// 页内探针模块：单次安装、IIFE 隔离 resolver，resize/scroll 自动重绘；支持文字搜索
pub const PROBE_MODULE: &str = r#"
function __webclawInstallProbe() {
  const PROBE_VERSION = 28;
  if (window.__webclawProbe && window.__webclawProbe.version === PROBE_VERSION) {
    return window.__webclawProbe;
  }
  if (window.__webclawProbe && typeof window.__webclawProbe.clear === 'function') {
    try { window.__webclawProbe.clear(); } catch (_) {}
  }
  __WEBCLAW_RESOLVER__
  const COLORS = {
    '图标按钮': '#f97316',
    '按钮': '#3b82f6',
    '输入框': '#22c55e',
    '表格': '#a855f7',
    '行': '#6b7280',
    '列': '#0d9488',
    '文字': '#e11d48',
  };
  const OVERLAY_ID = '__webclaw_probe_overlay__';
  const TEXT_TYPE = '文字';
  const TEXT_CARRIER_SEL = 'a,button,label,span,div,p,li,td,th,h1,h2,h3,h4,h5,h6,input[type="submit"],input[type="button"],input[type="reset"],input[type="image"],[role="button"],[role="link"],[role="menuitem"]';
  const itemType = (item) => (item && (item.type || item.elementType)) || '';
  const typeOffset = (offsets, type) => {
    if (!offsets || typeof offsets !== 'object') return 0;
    const n = Number(offsets[type]);
    return Number.isFinite(n) && n > 0 ? Math.floor(n) : 0;
  };
  const getProbeText = (el) => {
    if (!el || !(el instanceof Element)) return '';
    const parts = [];
    try {
      const it = (el.innerText || '').trim();
      if (it) parts.push(it);
    } catch (_) {}
    for (const p of [
      el.value,
      el.getAttribute && el.getAttribute('value'),
      el.getAttribute && el.getAttribute('placeholder'),
      el.getAttribute && el.getAttribute('aria-label'),
      el.getAttribute && el.getAttribute('title'),
      el.textContent,
    ]) {
      const t = (p || '').trim();
      if (t) parts.push(t);
    }
    return parts.join(' ').trim().slice(0, 200);
  };
  const textMatchScore = (text, qNorm) => {
    const t = normUiText(text);
    if (!t || !qNorm) return 0;
    if (t === qNorm) return 1.0;
    if (t.startsWith(qNorm)) return 0.98;
    if (t.includes(qNorm)) {
      const ratio = qNorm.length / Math.max(t.length, 1);
      return 0.92 + 0.07 * Math.min(ratio, 1);
    }
    return 0;
  };
  const findClickableForText = (el) => {
    if (!el || !(el instanceof Element)) return null;
    if (el.tagName === 'A' && structuralIsClickable(el)) return el;
    if (el.tagName === 'INPUT') {
      const t = ((el.type || 'text') + '').toLowerCase();
      if (['submit', 'button', 'reset', 'image'].includes(t) && structuralIsClickable(el)) return el;
    }
    try {
      const descs = el.querySelectorAll('a, button, input[type="submit"], input[type="button"], input[type="reset"], input[type="image"], [role="link"], [role="button"], [onclick], [ng-click], [data-ng-click]');
      for (const c of descs) {
        if (c.tagName === 'A' && structuralElementVisible(c) && structuralIsClickable(c)) return c;
      }
      for (const c of descs) {
        if (structuralElementVisible(c) && structuralIsClickable(c)) return c;
      }
    } catch (_) {}
    if (structuralIsClickable(el)) return el;
    let p = el.parentElement;
    let depth = 0;
    while (p && p !== document.body && depth < 6) {
      if (structuralIsClickable(p)) return p;
      p = p.parentElement;
      depth++;
    }
    return el;
  };
  const scanTextMatches = (query, scope) => {
    const q = (query || '').trim();
    const qNorm = normUiText(q);
    if (!qNorm) return [];
    const root = scope === document || scope === document.documentElement ? document : scope;
    let nodes = [];
    try {
      nodes = root.querySelectorAll ? [...root.querySelectorAll(TEXT_CARRIER_SEL)] : [...document.querySelectorAll(TEXT_CARRIER_SEL)];
    } catch (_) {
      nodes = [...document.querySelectorAll(TEXT_CARRIER_SEL)];
    }
    const hits = [];
    const seen = new Map();
    const vp = Math.max(1, window.innerWidth * window.innerHeight);
    const scrollX = window.scrollX || 0;
    const scrollY = window.scrollY || 0;
    for (const el of nodes) {
      if (!structuralElementVisible(el)) continue;
      let text = getProbeText(el);
      if (el.tagName === 'A') {
        const href = el.getAttribute('href') || el.getAttribute('ng-href') || '';
        const m = href.match(/(?:file_name|download_name|filename)=([^&]+)/i);
        if (m) {
          try { text = text + ' ' + decodeURIComponent(m[1]); } catch (_) { text = text + ' ' + m[1]; }
        }
      }
      const ts = textMatchScore(text, qNorm);
      if (ts < 0.92) continue;
      const direct = [...el.childNodes].filter((n) => n.nodeType === 3).map((n) => n.textContent).join('');
      const r = el.getBoundingClientRect();
      const areaRatio = (r.width * r.height) / Math.max(vp, 1);
      if (areaRatio > 0.35 && textMatchScore(direct.trim(), qNorm) < 0.92 && el.tagName !== 'A') continue;
      // 通用：松散包含 + 超大面积/过长文案 → 跳过（避免整页壳误匹配）
      if (ts < 0.98) {
        const tNorm = normUiText(text);
        const lenRatio = qNorm.length / Math.max(tNorm.length, 1);
        if (areaRatio > 0.12 || (lenRatio > 0 && lenRatio < 0.18)) continue;
      }
      const target = findClickableForText(el);
      if (!target) continue;
      const tr = target.getBoundingClientRect();
      const key = Math.round(tr.left) + ':' + Math.round(tr.top) + ':' + target.tagName;
      const rankingScore = ts + (structuralIsClickable(target) ? 0.15 : 0) + (ts >= 1 ? 0.05 : 0);
      const item = {
        text,
        textScore: ts,
        score: rankingScore,
        x: tr.left,
        y: tr.top,
        target,
        rect: { pageX: tr.left + scrollX, pageY: tr.top + scrollY, width: tr.width, height: tr.height },
      };
      if (seen.has(key)) {
        const idx = seen.get(key);
        const prev = hits[idx];
        if (item.textScore > prev.textScore + 1e-9) {
          hits[idx] = item;
          continue;
        }
        if (prev.textScore > item.textScore + 1e-9) continue;
        if (prev.score >= item.score) continue;
        hits[idx] = item;
        continue;
      }
      seen.set(key, hits.length);
      hits.push(item);
    }
    // 与点击指令一致：先文字匹配度，再从上到下、从左到右（#1 = 点击HCM）
    hits.sort((a, b) => b.textScore - a.textScore || a.y - b.y || a.x - b.x);
    return hits;
  };
  const resolveScope = (scopeRaw) => {
    const raw = (scopeRaw || '').trim();
    if (!raw) return { ok: true, scope: document, scopePrefix: '' };
    const sp = parseStructuralTarget(raw);
    if (!sp) return { ok: false, error: '无效的 scope：' + raw };
    const scopeEl = resolveStructuralTarget(sp.path);
    if (!scopeEl) return { ok: false, error: '未找到 scope「' + raw + '」' };
    return { ok: true, scope: scopeEl, scopePrefix: sp.label };
  };
  const scanProbe = (scopeRaw, searchRaw, indexOffsets, preserveIndices, typesFilter, overlayOnly) => {
    const scoped = resolveScope(scopeRaw);
    if (!scoped.ok) return scoped;
    const { scope, scopePrefix } = scoped;
    const items = [];
    const countsByType = {};
    const scrollX = window.scrollX || 0;
    const scrollY = window.scrollY || 0;
    const skipCmds = !!overlayOnly;
    const allTypes = ['图标按钮', '按钮', '输入框', '表格', '行', '列'];
    const types = (Array.isArray(typesFilter) && typesFilter.length)
      ? typesFilter.filter((t) => allTypes.indexOf(t) >= 0)
      : allTypes;
    const stampPrefixForType = (type) => (scopePrefix ? scopePrefix + type + '#' : type + '#');
    const parseStampIndex = (stamp, type) => {
      const prefix = stampPrefixForType(type);
      if (!stamp || stamp.indexOf(prefix) !== 0) return 0;
      const rest = stamp.slice(prefix.length);
      // 无 scope 时 stamp 即 类型#N；有 scope 时已含 scopePrefix+类型#
      if (scopePrefix) {
        const n = parseInt(rest, 10);
        return Number.isFinite(n) && n > 0 ? n : 0;
      }
      const n = parseInt(rest, 10);
      return Number.isFinite(n) && n > 0 && String(n) === rest ? n : 0;
    };
    if (!preserveIndices) {
      // 全新扫描：清掉旧打标，按当前布局重新编号
      try {
        const root = scope === document || scope === document.documentElement ? document : scope;
        clearProbeStamps(root);
      } catch (_) {
        clearProbeStamps(document);
      }
    }
    for (const type of types) {
      const rawEls = collectStructuralCandidates(type, scope).filter(structuralElementVisible);
      const base = scopePrefix ? 0 : typeOffset(indexOffsets, type);
      const used = new Set();
      const paired = [];
      if (preserveIndices) {
        const fresh = [];
        for (const el of rawEls) {
          const stamp = readProbeStamp(el);
          const idx = parseStampIndex(stamp, type);
          if (idx > 0 && !used.has(idx)) {
            used.add(idx);
            paired.push({ el, index: idx, targetSuffix: stamp });
          } else {
            fresh.push(el);
          }
        }
        // 仍按 DOM 序给新出现的元素接号，但不打乱已打标元素的旧序号
        const sortedFresh = orderStructuralCandidates(fresh);
        let next = base + 1;
        const takeNext = () => {
          while (used.has(next)) next += 1;
          const n = next;
          used.add(n);
          next += 1;
          return n;
        };
        for (const el of sortedFresh) {
          const index = takeNext();
          const targetSuffix = scopePrefix
            ? scopePrefix + formatStructuralSegment(index, type)
            : formatStructuralSegment(index, type);
          writeProbeStamp(el, targetSuffix);
          paired.push({ el, index, targetSuffix });
        }
        paired.sort((a, b) => a.index - b.index);
      } else {
        const els = orderStructuralCandidates(rawEls);
        els.forEach((el, i) => {
          const index = i + 1 + base;
          const targetSuffix = scopePrefix
            ? scopePrefix + formatStructuralSegment(index, type)
            : formatStructuralSegment(index, type);
          writeProbeStamp(el, targetSuffix);
          paired.push({ el, index, targetSuffix });
        });
      }
      countsByType[type] = paired.length;
      // 图标指令批量 O(n)：禁止对每个图标再全表 findIconButtonHits
      const iconTextCmds = (!skipCmds && type === '图标按钮')
        ? buildIconProbeTextCmdsForEls(paired.map((p) => p.el))
        : null;
      for (let pi = 0; pi < paired.length; pi++) {
        const { el, index, targetSuffix } = paired[pi];
        const r = el.getBoundingClientRect();
        const lock = (!skipCmds && type === '输入框') ? structuralInputLockHint(el) : '';
        const indexCmd = skipCmds
          ? ('点击' + targetSuffix)
          : (type === '输入框'
            ? (lock ? ('点击' + targetSuffix) : ('输入' + targetSuffix + '='))
            : ('点击' + targetSuffix));
        let textCmd = null;
        if (!skipCmds) {
          if (type === '图标按钮') {
            textCmd = (iconTextCmds && iconTextCmds[pi]) || null;
          } else if (!lock) {
            textCmd = buildProbeTextCmd(el, type, scope);
          }
        }
        items.push({
          index,
          type,
          tag: (el.tagName || '').toString(),
          cls: structuralClassName(el),
          snippet: skipCmds ? '' : structuralTextSnippet(el),
          desc: skipCmds ? '' : describeStructuralElement(el),
          clickCmd: '点击' + targetSuffix,
          inputCmd: (!skipCmds && type === '输入框') ? ('输入' + targetSuffix + '=') : null,
          textCmd,
          indexCmd,
          rect: { pageX: r.left + scrollX, pageY: r.top + scrollY, width: r.width, height: r.height },
        });
      }
    }
    const search = (searchRaw || '').trim();
    if (search) {
      const hits = scanTextMatches(search, scope);
      countsByType[TEXT_TYPE] = hits.length;
      const textBase = scopePrefix ? 0 : typeOffset(indexOffsets, TEXT_TYPE);
      const textPrefix = '文字#' + (search ? encodeURIComponent(search).slice(0, 48) + '#' : '');
      if (!preserveIndices) {
        try {
          document.querySelectorAll('[' + PROBE_STAMP_ATTR + '^="文字#"]').forEach((el) => {
            try { el.removeAttribute(PROBE_STAMP_ATTR); } catch (_) {}
          });
        } catch (_) {}
      }
      const usedText = new Set();
      const textPaired = [];
      if (preserveIndices) {
        const fresh = [];
        for (const h of hits) {
          const stamp = readProbeStamp(h.target);
          let idx = 0;
          if (stamp && stamp.indexOf(textPrefix) === 0) {
            idx = parseInt(stamp.slice(textPrefix.length), 10) || 0;
          }
          if (idx > 0 && !usedText.has(idx)) {
            usedText.add(idx);
            textPaired.push({ h, index: idx });
          } else {
            fresh.push(h);
          }
        }
        let next = textBase + 1;
        for (const h of fresh) {
          while (usedText.has(next)) next += 1;
          const index = next;
          usedText.add(index);
          next += 1;
          writeProbeStamp(h.target, textPrefix + index);
          textPaired.push({ h, index });
        }
        textPaired.sort((a, b) => a.index - b.index);
      } else {
        hits.forEach((h, i) => {
          const index = i + 1 + textBase;
          writeProbeStamp(h.target, textPrefix + index);
          textPaired.push({ h, index });
        });
      }
      countsByType[TEXT_TYPE] = textPaired.length;
      for (const { h, index } of textPaired) {
        const textCmd = '点击' + search;
        const indexCmd = index === 1 ? ('点击' + search) : ('点击' + search + '#' + index);
        const snippet = (h.text || '').replace(/\s+/g, ' ').trim().slice(0, 48);
        items.push({
          index,
          type: TEXT_TYPE,
          desc: describeStructuralElement(h.target) + (snippet ? ' 「' + snippet + '」' : ''),
          clickCmd: indexCmd,
          inputCmd: null,
          textCmd,
          indexCmd,
          textScore: h.textScore,
          rect: h.rect,
        });
      }
    } else {
      countsByType[TEXT_TYPE] = 0;
    }
    return {
      ok: true,
      url: location.href,
      title: document.title,
      scope: scopePrefix || null,
      search: search || null,
      frameLabel: (() => {
        try {
          if (window.top === window) return null;
        } catch (_) {
          return 'iframe';
        }
        return 'iframe';
      })(),
      frameUrl: location.href,
      items,
      countsByType,
      indexOffsets: indexOffsets || {},
    };
  };
  const paintCtl = { scrollBound: false, paintRaf: 0, paintTimer: 0, lastPaintAt: 0, onScrollPaint: null };
  const ensureProbeCanvas = () => {
    let canvas = document.getElementById(OVERLAY_ID);
    if (canvas && canvas.tagName === 'CANVAS') return canvas;
    try {
      document.querySelectorAll('[id="' + OVERLAY_ID + '"]').forEach((n) => {
        try { n.remove(); } catch (_) {}
      });
    } catch (_) {
      if (canvas) try { canvas.remove(); } catch (_) {}
    }
    canvas = document.createElement('canvas');
    canvas.id = OVERLAY_ID;
    canvas.style.cssText =
      'position:fixed;left:0;top:0;width:100vw;height:100vh;pointer-events:none;z-index:2147483647;';
    document.documentElement.appendChild(canvas);
    return canvas;
  };
  const paintProbeItems = (canvas, items, visibleTypes, highlight) => {
    const visibleSet = new Set(visibleTypes && visibleTypes.length ? visibleTypes : ['图标按钮']);
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const cssW = Math.max(1, window.innerWidth || document.documentElement.clientWidth || 1);
    const cssH = Math.max(1, window.innerHeight || document.documentElement.clientHeight || 1);
    const bw = Math.floor(cssW * dpr);
    const bh = Math.floor(cssH * dpr);
    if (canvas.width !== bw || canvas.height !== bh) {
      canvas.width = bw;
      canvas.height = bh;
      canvas.style.width = cssW + 'px';
      canvas.style.height = cssH + 'px';
    }
    const ctx = canvas.getContext('2d');
    if (!ctx) return 0;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, cssW, cssH);
    const scrollX = window.scrollX || document.documentElement.scrollLeft || 0;
    const scrollY = window.scrollY || document.documentElement.scrollTop || 0;
    const pad = 4;
    let drawn = 0;
    ctx.textBaseline = 'middle';
    ctx.textAlign = 'center';
    ctx.font = 'bold 11px sans-serif';
    for (const item of items || []) {
      const t = itemType(item);
      if (!visibleSet.has(t)) continue;
      const color = COLORS[t] || '#f97316';
      const isHi = highlight && itemType(highlight) === t && highlight.index === item.index;
      const r = item.rect || {};
      const x = (r.pageX || 0) - scrollX;
      const y = (r.pageY || 0) - scrollY;
      const w = Math.max(r.width || 0, 4);
      const h = Math.max(r.height || 0, 4);
      if (x + w < -pad || y + h < -pad || x > cssW + pad || y > cssH + pad) continue;
      ctx.strokeStyle = color;
      ctx.lineWidth = 2;
      ctx.fillStyle = isHi ? color + '44' : color + '22';
      ctx.fillRect(x, y, w, h);
      ctx.strokeRect(x, y, w, h);
      const label = String(item.index);
      const tw = Math.max(18, ctx.measureText(label).width + 8);
      const th = 18;
      const bx = x - 1;
      const by = y - 1;
      ctx.fillStyle = color;
      ctx.fillRect(bx, by, tw, th);
      ctx.fillStyle = '#fff';
      ctx.fillText(label, bx + tw / 2, by + th / 2);
      drawn++;
    }
    paintCtl.lastPaintAt = Date.now();
    return drawn;
  };
  const bindProbeScrollPaint = (getState) => {
    if (paintCtl.scrollBound) return;
    paintCtl.scrollBound = true;
    paintCtl.onScrollPaint = () => {
      if (paintCtl.paintRaf || paintCtl.paintTimer) return;
      const wait = Math.max(0, 48 - (Date.now() - (paintCtl.lastPaintAt || 0)));
      const schedule = (fn) => {
        if (wait <= 0) {
          paintCtl.paintRaf = requestAnimationFrame(() => {
            paintCtl.paintRaf = 0;
            fn();
          });
        } else {
          paintCtl.paintTimer = setTimeout(() => {
            paintCtl.paintTimer = 0;
            paintCtl.paintRaf = requestAnimationFrame(() => {
              paintCtl.paintRaf = 0;
              fn();
            });
          }, wait);
        }
      };
      schedule(() => {
        const el = document.getElementById(OVERLAY_ID);
        if (!el || el.tagName !== 'CANVAS') return;
        const st = getState();
        if (!st || !st.lastItems || !st.lastItems.length) return;
        paintProbeItems(el, st.lastItems, st.visibleTypes, st.highlight);
      });
    };
    window.addEventListener('scroll', paintCtl.onScrollPaint, { capture: true, passive: true });
    window.addEventListener('resize', paintCtl.onScrollPaint, { passive: true });
  };
  const unbindProbeScrollPaint = () => {
    if (!paintCtl.scrollBound) return;
    if (paintCtl.onScrollPaint) {
      try { window.removeEventListener('scroll', paintCtl.onScrollPaint, true); } catch (_) {}
      try { window.removeEventListener('resize', paintCtl.onScrollPaint); } catch (_) {}
    }
    if (paintCtl.paintRaf) {
      try { cancelAnimationFrame(paintCtl.paintRaf); } catch (_) {}
      paintCtl.paintRaf = 0;
    }
    if (paintCtl.paintTimer) {
      try { clearTimeout(paintCtl.paintTimer); } catch (_) {}
      paintCtl.paintTimer = 0;
    }
    paintCtl.scrollBound = false;
    paintCtl.onScrollPaint = null;
  };
  const renderProbe = (items, visibleTypes, highlight) => {
    const canvas = ensureProbeCanvas();
    bindProbeScrollPaint(() => (window.__webclawProbe && window.__webclawProbe.state) || null);
    return paintProbeItems(canvas, items, visibleTypes, highlight);
  };
  const api = {
    version: PROBE_VERSION,
    state: { scope: '', search: '', visibleTypes: ['图标按钮'], highlight: null, lastItems: [], indexOffsets: {}, lastTypesKey: '' },
    /** 仅采集候选，不绘制 overlay（AI 修复 / 列表扫描用，避免整页图层撑爆内存） */
    scan(opts) {
      if (opts && opts.scope !== undefined) this.state.scope = opts.scope || '';
      if (opts && opts.search !== undefined) this.state.search = opts.search || '';
      if (opts && opts.indexOffsets !== undefined) this.state.indexOffsets = opts.indexOffsets || {};
      // 若此前开过探针 overlay，先摘掉，避免修复期间残留大图层
      try {
        document.querySelectorAll('[id="' + OVERLAY_ID + '"]').forEach((n) => {
          try { n.remove(); } catch (_) {}
        });
      } catch (_) {
        const old = document.getElementById(OVERLAY_ID);
        if (old) old.remove();
      }
      const preserve = !!(opts && opts.preserveIndices);
      const typesFilter = (opts && Array.isArray(opts.types)) ? opts.types : null;
      const result = scanProbe(this.state.scope, this.state.search, this.state.indexOffsets, preserve, typesFilter, false);
      if (!result.ok) return result;
      // 不缓存整表到 window，减少 SPA 长驻内存
      this.state.lastItems = [];
      this.state.lastTypesKey = '';
      return result;
    },
    apply(opts) {
      if (opts && opts.scope !== undefined) this.state.scope = opts.scope || '';
      if (opts && opts.search !== undefined) this.state.search = opts.search || '';
      if (opts && opts.visibleTypes !== undefined) this.state.visibleTypes = opts.visibleTypes;
      if (opts && opts.highlight !== undefined) this.state.highlight = opts.highlight;
      if (opts && opts.indexOffsets !== undefined) this.state.indexOffsets = opts.indexOffsets || {};
      // overlay 重绘：默认锁定序号 + 不生成指令文本（仅刷新/扫描才全量扫）
      const preserve = opts && opts.preserveIndices === false ? false : true;
      const overlayOnly = opts && opts.overlayOnly === false ? false : true;
      const typesFilter = Array.isArray(this.state.visibleTypes) && this.state.visibleTypes.length
        ? this.state.visibleTypes
        : ['图标按钮'];
      const typesKey = typesFilter.slice().sort().join('\0');
      // 悬停高亮：复用上次 items，避免再次 collect + getComputedStyle 拖死页面
      const canReuse = !!(opts && opts.reuseItems)
        && overlayOnly
        && preserve
        && this.state.lastItems
        && this.state.lastItems.length > 0
        && this.state.lastTypesKey === typesKey;
      if (canReuse) {
        renderProbe(this.state.lastItems, this.state.visibleTypes, this.state.highlight);
        return {
          ok: true,
          reused: true,
          items: this.state.lastItems,
          countsByType: {},
          url: location.href,
          title: document.title,
        };
      }
      const result = scanProbe(this.state.scope, this.state.search, this.state.indexOffsets, preserve, typesFilter, overlayOnly);
      if (!result.ok) return result;
      this.state.lastItems = result.items || [];
      this.state.lastTypesKey = typesKey;
      renderProbe(this.state.lastItems, this.state.visibleTypes, this.state.highlight);
      return result;
    },
    clear() {
      unbindProbeScrollPaint();
      try {
        document.querySelectorAll('[id="' + OVERLAY_ID + '"]').forEach((n) => {
          try { n.remove(); } catch (_) {}
        });
      } catch (_) {
        const old = document.getElementById(OVERLAY_ID);
        if (old) old.remove();
      }
      this.state.lastItems = [];
      this.state.lastTypesKey = '';
      this.state.highlight = null;
      this.state.search = '';
      this.state.indexOffsets = {};
      clearProbeStamps(document);
      try { delete window.__webclawProbe; } catch (_) { try { window.__webclawProbe = null; } catch (_) {} }
    },
  };
  // 滚动/缩放只触发 canvas 轻量重绘（不 collect）。全量扫描仍仅由 UI「刷新/扫描」触发。
  window.__webclawProbe = api;
  return api;
}
"#;

pub fn probe_module_js() -> String {
    PROBE_MODULE.replace("__WEBCLAW_RESOLVER__", STRUCTURAL_TARGET_RESOLVER)
}

pub const FOCUS_HELPERS: &str = r#"
const describeActiveElement = (el) => {
  if (!el) el = document.activeElement;
  if (!el || el === document.body) return 'body';
  const tag = el.tagName || '?';
  const id = el.id || '';
  const cls = (el.className || '').toString().split(/\s+/).filter(Boolean)[0] || '';
  return `${tag}${id ? '#' + id : ''}${cls ? '.' + cls : ''}`.slice(0, 60);
};

const findInputByHint = (hint) => {
  const h = (hint || '').trim().toLowerCase();
  const selector = 'input:not([type=hidden]):not([type=checkbox]):not([type=radio]):not([type=submit]):not([type=button]):not([type=reset]):not([type=file]), textarea';
  const isVisible = (el) => {
    if (!el || !(el instanceof Element)) return false;
    if (el.disabled || el.readOnly) return false;
    const s = window.getComputedStyle(el);
    if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false;
    const r = el.getBoundingClientRect();
    return r.width >= 4 && r.height >= 4 && r.bottom > 0 && r.right > 0;
  };
  const meta = (el) => {
    const blob = `${el.placeholder || ''} ${el.name || ''} ${el.id || ''} ${(el.className || '').toString()} ${el.getAttribute('aria-label') || ''}`.toLowerCase();
    return blob;
  };
  const ranked = [...document.querySelectorAll(selector)]
    .filter(isVisible)
    .map((el) => {
      let score = 0;
      const blob = meta(el);
      if (h && blob.includes(h)) score = 20;
      else if (!h) score = 1;
      if (document.activeElement === el) score += 3;
      return { el, score };
    })
    .filter((x) => x.score > 0)
    .sort((a, b) => b.score - a.score);
  return ranked[0]?.el || null;
};

const refocusFromAnchor = (anchor) => {
  if (!anchor) return { ok: false, error: '无焦点锚点' };
  let el = null;
  const kind = anchor.kind || '';
  const value = (anchor.value || '').trim();
  if (!value) return { ok: false, error: '焦点锚点为空' };
  if (kind === 'structural') {
    const sp = parseStructuralTarget(value);
    if (sp) el = resolveStructuralTarget(sp.path);
  } else {
    el = findInputByHint(value);
  }
  if (!el) return { ok: false, error: `未找到焦点目标「${value}」` };
  try { el.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'instant' }); } catch (_) {}
  el.focus();
  try { el.click(); } catch (_) {}
  const focused = document.activeElement === el;
  return { ok: true, focused, summary: describeActiveElement(el), value: value };
};

const syncControlledInputValue = (text) => {
  const el = document.activeElement;
  if (!el || !text) return { ok: false, error: '无焦点元素' };
  try {
    if (typeof el.setCustomValidity === 'function') el.setCustomValidity('');
  } catch (_) {}
  try {
    el.dispatchEvent(new InputEvent('input', { bubbles: true, data: text, inputType: 'insertText' }));
  } catch (_) {
    el.dispatchEvent(new Event('input', { bubbles: true }));
  }
  try {
    el.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true, data: text }));
  } catch (_) {}
  el.dispatchEvent(new Event('change', { bubbles: true }));
  return {
    ok: true,
    summary: describeActiveElement(el),
    value: ((el.value || el.textContent || '') + '').slice(0, 80),
  };
};

const tryGenericSubmitFromFocused = () => {
  const el = document.activeElement;
  if (!el) return { ok: false, method: 'none' };
  const form = el.form || el.closest('form');
  if (form) {
    try {
      if (typeof form.requestSubmit === 'function') {
        form.requestSubmit();
        return { ok: true, method: 'form.requestSubmit' };
      }
      form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
      return { ok: true, method: 'form.submit' };
    } catch (_) {}
  }
  const container = el.closest('[role="search"], form, [class*="search"], [class*="Search"]') || el.parentElement;
  if (container) {
    const nodes = [...container.querySelectorAll('button, input[type="submit"], a, div, span')];
    for (const btn of nodes) {
      if (btn === el) continue;
      const cls = (btn.className || '').toString();
      const aria = (btn.getAttribute('aria-label') || '').trim();
      const type = ((btn.type || '') + '').toLowerCase();
      const text = (btn.innerText || '').trim();
      const searchLike = type === 'submit' || /search|搜索|submit|btn/i.test(cls + aria + text);
      if (!searchLike) continue;
      const r = btn.getBoundingClientRect();
      if (r.width < 8 || r.height < 8) continue;
      const s = window.getComputedStyle(btn);
      if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) continue;
      return {
        ok: true,
        method: 'click',
        x: r.x + r.width / 2,
        y: r.y + r.height / 2,
        label: (aria || text || cls).slice(0, 40),
      };
    }
  }
  return { ok: false, method: 'none' };
};
"#;

/// 点击文字候选采集、排序、遮罩重排 — 探针与 CLICK_DOM 共用（改一处两边同步）
pub const CLICK_TEXT_RANKING: &str = r#"
// click-text-ranking-v6 — 点击文字本 frame #N（探针与 CLICK_DOM 同源；跨 frame 连续号在 Rust 合成）
// v6：EasyUI/Ant 下拉选项点 .combobox-item 自身，禁止上浮到 .combo-p
// v5：#N 以 textScore（文字匹配分）优先；同分硬可点击优先；软命中坐标点击
// v4：仅匹配「本元素连续文字」；软命中坐标点击；文案未命中不靠祖先整段 innerText 凑数
const MIN_TEXT_SCORE = 0.92;
const TEXT_CARRIER_SEL = [
  'a','button','label','span','div','p','li','td','th',
  'h1','h2','h3','h4','h5','h6',
  'input[type="submit"]','input[type="button"]','input[type="reset"]','input[type="image"]',
  '[role="button"]','[role="link"]','[role="menuitem"]'
].join(', ');
const clickTextNormUi = (s) => (s || '').trim().toLowerCase().replace(/\s+/g, '');
const clickTextNorm = (s) => (typeof normUiText === 'function' ? normUiText(s) : clickTextNormUi(s));
const isLooseContainsScore = (score) => score >= MIN_TEXT_SCORE && score < 0.98;
const isInputButtonLike = (el) => {
  if (!el || el.tagName !== 'INPUT') return false;
  return ['submit', 'button', 'reset', 'image'].includes((el.type || 'text').toLowerCase());
};
const isDisplayed = (el) => {
  if (!el || !(el instanceof Element)) return false;
  const s = window.getComputedStyle(el);
  if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false;
  if (s.pointerEvents === 'none') return false;
  const r = el.getBoundingClientRect();
  return r.width >= 2 && r.height >= 2;
};
/** 布局可见（软命中用）：不要求 pointer-events，占位 span 常为 none */
const isLayoutVisible = (el) => {
  if (!el || !(el instanceof Element)) return false;
  const s = window.getComputedStyle(el);
  if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false;
  const r = el.getBoundingClientRect();
  return r.width >= 2 && r.height >= 2;
};
const inViewportRect = (r) => r.bottom > 0 && r.right > 0
  && r.top < window.innerHeight && r.left < window.innerWidth;
const isVisible = (el) => {
  if (!isDisplayed(el)) return false;
  return inViewportRect(el.getBoundingClientRect());
};
const isLayoutInViewport = (el) => {
  if (!isLayoutVisible(el)) return false;
  return inViewportRect(el.getBoundingClientRect());
};
const isTrulyDisabled = (el) => {
  if (!el || !(el instanceof Element)) return true;
  try { if (el.disabled === true) return true; } catch (_) {}
  try { if ((el.getAttribute && el.getAttribute('aria-disabled')) === 'true') return true; } catch (_) {}
  return false;
};
const isDisabled = (el) => {
  if (isTrulyDisabled(el)) return true;
  const cls = (el.className || '').toString();
  return /\bdisabled\b/i.test(cls);
};
/** EasyUI / Ant / Element 等下拉选项：应点条目本身，勿上浮到整个 combo-p / dropdown 面板 */
const isOptionLikeElement = (el) => {
  if (!el || !(el instanceof Element)) return false;
  const role = (el.getAttribute('role') || '').toLowerCase();
  if (role === 'option' || role === 'treeitem' || role === 'menuitem') return true;
  const cls = (el.className || '').toString();
  if (/\b(combobox-item|combo-item|list-item|select-item|dropdown-item|menu-item|option-item|ant-select-item|el-select-dropdown__item|el-option|n-base-select-option|semi-select-option)\b/i.test(cls)) {
    return true;
  }
  return false;
};
const isClickable = (el) => {
  if (!el || el === document.body || isDisabled(el)) return false;
  const tag = el.tagName;
  if (tag === 'A' || tag === 'BUTTON' || tag === 'SUMMARY' || tag === 'LABEL') return true;
  if (tag === 'INPUT' && ['submit', 'button', 'reset', 'image'].includes((el.type || 'text').toLowerCase())) return true;
  const role = (el.getAttribute('role') || '').toLowerCase();
  if (['button', 'link', 'menuitem', 'tab', 'option'].includes(role)) return true;
  if (el.hasAttribute('onclick') || el.hasAttribute('ng-click') || el.hasAttribute('data-ng-click')) return true;
  const cls = (el.className || '').toString();
  if (/\b(btn|button|clickable)\b/i.test(cls)) return true;
  // 下拉选项常无 cursor:pointer，但仍须点条目而非整块 panel
  if (isOptionLikeElement(el)) return true;
  return window.getComputedStyle(el).cursor === 'pointer';
};
const getVisibleText = (el) => {
  if (!el || !(el instanceof Element)) return '';
  const parts = [];
  try {
    const it = (el.innerText || '').trim();
    if (it) parts.push(it);
  } catch (_) {}
  for (const p of [
    el.value,
    el.getAttribute('value'),
    el.getAttribute('placeholder'),
    el.getAttribute('aria-label'),
    el.getAttribute('title'),
    el.textContent,
  ]) {
    const t = (p || '').trim();
    if (t) parts.push(t);
  }
  return parts.join(' ').trim().slice(0, 200);
};
/** 本元素连续文字：直接文本节点，或仅含 phrasing 子节点时的短 innerText；不含祖先/深层块级拼凑 */
const getOwnContinuousText = (el) => {
  if (!el || !(el instanceof Element)) return '';
  const attrs = [
    el.value,
    el.getAttribute && el.getAttribute('value'),
    el.getAttribute && el.getAttribute('placeholder'),
    el.getAttribute && el.getAttribute('aria-label'),
    el.getAttribute && el.getAttribute('title'),
  ]
    .map((p) => (p || '').toString().trim())
    .filter(Boolean);

  let direct = '';
  try {
    direct = [...el.childNodes]
      .filter((n) => n.nodeType === 3)
      .map((n) => n.textContent || '')
      .join('')
      .replace(/\s+/g, ' ')
      .trim();
  } catch (_) {}

  let shallow = '';
  try {
    const childEls = [...el.children];
    const phrasing = /^(SPAN|B|I|EM|STRONG|FONT|LABEL|SMALL|U|S|CODE|A|WBR|BR)$/i;
    if (childEls.length === 0) {
      shallow = ((el.innerText || el.textContent || '') + '').replace(/\s+/g, ' ').trim();
    } else if (childEls.every((c) => phrasing.test(c.tagName))) {
      const it = ((el.innerText || '') + '').replace(/\s+/g, ' ').trim();
      // 过长则视为容器文案，不当地「一段连续字」
      if (it && it.length <= 64) shallow = it;
    }
  } catch (_) {}

  const parts = [];
  if (direct) parts.push(direct);
  else if (shallow) parts.push(shallow);
  for (const a of attrs) {
    if (!parts.includes(a)) parts.push(a);
  }
  return parts.join(' ').trim().slice(0, 200);
};
const hrefFileNames = (el) => {
  if (!el || el.tagName !== 'A') return [];
  const out = [];
  const href = el.getAttribute('href') || '';
  const ngHref = el.getAttribute('ng-href') || '';
  for (const raw of [href, ngHref]) {
    if (!raw) continue;
    out.push(raw);
    try {
      const u = new URL(raw, location.href);
      for (const key of ['file_name', 'download_name', 'filename', 'name']) {
        const v = u.searchParams.get(key);
        if (v) {
          try { out.push(decodeURIComponent(v)); } catch (_) { out.push(v); }
        }
      }
      const base = (u.pathname || '').split('/').pop();
      if (base) {
        try { out.push(decodeURIComponent(base)); } catch (_) { out.push(base); }
      }
    } catch (_) {
      const m = raw.match(/(?:file_name|download_name|filename)=([^&]+)/i);
      if (m) {
        try { out.push(decodeURIComponent(m[1])); } catch (_) { out.push(m[1]); }
      }
    }
  }
  const dl = el.getAttribute('download');
  if (dl) out.push(dl);
  return out;
};
const getMatchText = (el) => {
  // 主匹配：本元素连续文字；链接额外拼 filename（不用整棵子树 innerText）
  const parts = [getOwnContinuousText(el)];
  for (const n of hrefFileNames(el)) parts.push(n);
  return parts.join(' ').trim().slice(0, 400);
};
const makeMatcher = (qNorm) => (text) => {
  const t = clickTextNorm(text);
  if (!t) return 0;
  if (t === qNorm) return 1.0;
  if (t.startsWith(qNorm)) return 0.98;
  if (t.includes(qNorm)) {
    const ratio = qNorm.length / Math.max(t.length, 1);
    return 0.92 + 0.07 * Math.min(ratio, 1);
  }
  let score = 0;
  for (const ch of qNorm) if (t.includes(ch)) score += 0.05;
  return Math.min(score, 0.55);
};
const findClickTarget = (el, matcher) => {
  if (!el || !(el instanceof Element)) return el;
  // 选项条目：点自己，禁止上浮到 .combo-p / dropdown 外壳
  if (isOptionLikeElement(el) && isLayoutVisible(el)) return el;
  if (el.tagName === 'A' || el.tagName === 'BUTTON' || el.tagName === 'SUMMARY' || el.tagName === 'LABEL' || isInputButtonLike(el)) {
    if (isClickable(el)) return el;
  }
  try {
    const descs = el.querySelectorAll(
      'a, button, input[type="submit"], input[type="button"], input[type="reset"], input[type="image"], [role="link"], [role="button"], [onclick], [ng-click], [data-ng-click]'
    );
    let best = null;
    let bestScore = 0;
    for (const c of descs) {
      if (!isDisplayed(c) || !isClickable(c) || isDisabled(c)) continue;
      const s = matcher(getMatchText(c));
      if (s > bestScore) { bestScore = s; best = c; }
    }
    if (best && bestScore >= MIN_TEXT_SCORE) return best;
    for (const c of descs) {
      if (c.tagName === 'A' && isDisplayed(c) && isClickable(c) && !isDisabled(c)) return c;
    }
  } catch (_) {}
  const near = el.closest?.(
    'a, button, summary, label, input[type="submit"], input[type="button"], input[type="reset"], input[type="image"], [onclick], [ng-click], [data-ng-click], [role="button"], [role="link"], [role="menuitem"], [role="tab"], [role="option"]'
  );
  if (near && isClickable(near)) return near;
  let n = el;
  let fallback = el;
  while (n && n !== document.body) {
    if (isOptionLikeElement(n) && isLayoutVisible(n)) return n;
    if (isClickable(n)) return n;
    if (n.tagName === 'BUTTON' || isInputButtonLike(n) || n.hasAttribute('onclick') || n.hasAttribute('ng-click') || n.hasAttribute('data-ng-click')) {
      fallback = n;
    }
    n = n.parentElement;
  }
  return fallback;
};
/** 软命中点击壳：选项条目用自身；勿把 select/combo 整块面板当成壳 */
const findSoftClickTarget = (el) => {
  if (!el || !(el instanceof Element)) return el;
  if (isOptionLikeElement(el) && isLayoutVisible(el)) return el;
  try {
    const shell = el.closest(
      '[class*="select" i], [class*="Select"], [class*="picker" i], [class*="Picker"], [class*="cascader" i], [role="combobox"]'
    );
    // 排除下拉面板/popup（combo-p、select-dropdown），否则三点选项会合成一个壳
    if (shell && isLayoutVisible(shell) && shell !== document.body && !isComboDropdownPanel(shell)) {
      // 若起点已是选项，仍用选项
      if (!isOptionLikeElement(el)) return shell;
    }
  } catch (_) {}
  let n = el;
  let best = el;
  let depth = 0;
  while (n && n !== document.body && depth < 6) {
    if (isOptionLikeElement(n) && isLayoutVisible(n)) return n;
    if (isComboDropdownPanel(n)) break;
    if (isLayoutVisible(n)) {
      const r = n.getBoundingClientRect();
      if (r.width >= 24 && r.height >= 14) best = n;
    }
    n = n.parentElement;
    depth += 1;
  }
  return best;
};
const isComboDropdownPanel = (el) => {
  if (!el || !(el instanceof Element)) return false;
  const cls = (el.className || '').toString();
  if (/\b(combo-p|combo-panel|combobox-panel|select-dropdown|ant-select-dropdown|el-select-dropdown|el-popper|n-base-select-menu|semi-select-option-list|datepicker|date-picker|picker-panel|cascader-menus|cascader-menu)\b/i.test(cls)) {
    return true;
  }
  // EasyUI：panel.combo-p
  if (/\bpanel\b/i.test(cls) && /\bcombo/i.test(cls)) return true;
  return false;
};
const scoreTarget = (target, textScore, vp) => {
  let s = textScore;
  const tr = target.getBoundingClientRect();
  const area = tr.width * tr.height;
  const areaRatio = area / Math.max(vp, 1);
  const tag = target.tagName;
  if (tag === 'BUTTON' || isInputButtonLike(target)) s += 0.18;
  if (tag === 'A') s += 0.22;
  const role = (target.getAttribute('role') || '').toLowerCase();
  if (role === 'button' || role === 'menuitem' || role === 'tab') s += 0.15;
  if (role === 'link') s += 0.08;
  if (target.hasAttribute('onclick') || target.hasAttribute('ng-click') || target.hasAttribute('data-ng-click')) s += 0.15;
  const cls = (target.className || '').toString();
  if (/\b(btn|button)\b/i.test(cls)) s += 0.1;
  if (area > 0 && areaRatio < 0.02) s += 0.08;
  if (areaRatio > 0.4) s -= 0.2;
  if (isLooseContainsScore(textScore) && areaRatio > 0.08) s -= 0.35;
  if (isVisible(target)) s += 0.05;
  return s;
};
const isBloatedContainsHit = (el, target, text, textScore, vp, qNorm) => {
  if (el === document.documentElement || el === document.body) return true;
  if (target === document.documentElement || target === document.body) return true;
  const tag = (target && target.tagName) || '';
  if (tag === 'HTML' || tag === 'BODY') return true;
  // 选项文字命中却点到整块下拉面板
  if (target && target !== el && isComboDropdownPanel(target)) return true;
  if (target && target !== el && isOptionLikeElement(el) && target.contains(el)) {
    const tr = target.getBoundingClientRect();
    const er = el.getBoundingClientRect();
    const tArea = Math.max(tr.width * tr.height, 1);
    const eArea = Math.max(er.width * er.height, 1);
    if (tArea > eArea * 3) return true;
  }
  if (!isLooseContainsScore(textScore)) return false;
  const r = (target || el).getBoundingClientRect();
  const areaRatio = (r.width * r.height) / Math.max(vp, 1);
  if (areaRatio > 0.12) return true;
  const tNorm = clickTextNorm(text);
  const lenRatio = qNorm.length / Math.max(tNorm.length, 1);
  if (lenRatio > 0 && lenRatio < 0.18) return true;
  return false;
};
const pushHit = (hits, seen, item) => {
  const tr = item.target.getBoundingClientRect();
  // 软命中按点击壳去重；硬命中按坐标+标签
  const key = item.coordOnly
    ? ('soft:' + Math.round(tr.left) + ':' + Math.round(tr.top) + ':' + Math.round(tr.width) + 'x' + Math.round(tr.height))
    : (Math.round(tr.left) + ':' + Math.round(tr.top) + ':' + item.target.tagName);
  if (seen.has(key)) {
    const idx = seen.get(key);
    const prev = hits[idx];
    const prevHard = prev.clickable ? 1 : 0;
    const nextHard = item.clickable ? 1 : 0;
    if (nextHard !== prevHard) {
      if (nextHard > prevHard) hits[idx] = item;
      return;
    }
    // 同壳软命中：保留更贴文字的（更高 textScore；同分取更小载体面积）
    if (item.coordOnly && prev.coordOnly) {
      if (item.textScore > prev.textScore + 1e-9) { hits[idx] = item; return; }
      if (prev.textScore > item.textScore + 1e-9) return;
      const prevArea = (prev._area != null) ? prev._area : Infinity;
      const nextArea = (item._area != null) ? item._area : Infinity;
      if (nextArea < prevArea) hits[idx] = item;
      return;
    }
    if (item.textScore > prev.textScore + 1e-9) { hits[idx] = item; return; }
    if (prev.textScore > item.textScore + 1e-9) return;
    if (prev.score >= item.score) return;
    hits[idx] = item;
    return;
  }
  seen.set(key, hits.length);
  hits.push(item);
};
const considerElement = (hits, seen, el, vp, qNorm, matcher, displayedOnly) => {
  if (el === document.documentElement || el === document.body) return;
  const hardVisible = displayedOnly ? isDisplayed(el) : isVisible(el);
  const softVisible = displayedOnly ? isLayoutVisible(el) : isLayoutInViewport(el);
  if (!hardVisible && !softVisible) return;
  // 只用本元素连续文字打分（避免父级整段文案误命中）
  const ownText = getMatchText(el);
  const textScore = matcher(ownText);
  if (textScore < MIN_TEXT_SCORE) return;
  const direct = [...el.childNodes].filter((n) => n.nodeType === 3).map((n) => n.textContent).join('');
  const rect = el.getBoundingClientRect();
  const areaRatio = (rect.width * rect.height) / Math.max(vp, 1);
  if (areaRatio > 0.35 && matcher(direct.trim()) < MIN_TEXT_SCORE && el.tagName !== 'A') return;
  let target = findClickTarget(el, matcher);
  if (!target || isTrulyDisabled(target)) return;
  if (target === document.documentElement || target === document.body) return;
  let clickable = hardVisible && isClickable(target);
  let carrierArea = rect.width * rect.height;
  if (!clickable) {
    if (!softVisible) return;
    // 精确/前缀连续字才做软命中，避免松散包含扫出一堆壳
    if (textScore < 0.98) return;
    target = findSoftClickTarget(el) || el;
    if (!target || isTrulyDisabled(target) || !isLayoutVisible(target)) return;
    if (target === document.documentElement || target === document.body) return;
    // 软命中也拒绝「选项 → 整块 combo-p」
    if (isBloatedContainsHit(el, target, ownText, textScore, vp, qNorm)) {
      target = el;
    }
    const tr0 = target.getBoundingClientRect();
    // 点击壳过大（整页/整弹窗）时改回文字载体本身中心
    if ((tr0.width * tr0.height) / Math.max(vp, 1) > 0.2) {
      target = el;
    }
  } else {
    // 硬可点：目标自身连续字也需达标（或载体已达标且目标是按钮壳）
    const targetOwn = getMatchText(target);
    const targetScore = Math.max(textScore, matcher(targetOwn));
    if (targetScore < MIN_TEXT_SCORE) return;
    if (isBloatedContainsHit(el, target, targetOwn || ownText, targetScore, vp, qNorm)) {
      // 精确选项被上浮到面板时：改回条目本身
      if (isOptionLikeElement(el) && isLayoutVisible(el)) target = el;
      else return;
    }
  }
  const tr = target.getBoundingClientRect();
  let score = scoreTarget(target, textScore, vp);
  if (!clickable) score -= 0.45;
  // 更短连续字 / 更小载体优先
  score += 0.05 * Math.min(1, qNorm.length / Math.max(clickTextNorm(ownText).length, 1));
  pushHit(hits, seen, {
    text: ownText,
    x: tr.left + tr.width / 2,
    y: tr.top + tr.height / 2,
    score,
    textScore,
    target,
    clickable,
    coordOnly: !clickable,
    _area: carrierArea,
    inViewport: inViewportRect(tr),
  });
};
const collectHits = (displayedOnly, qNorm, matcher) => {
  const hits = [];
  const seen = new Map();
  const vp = Math.max(1, window.innerWidth * window.innerHeight);
  for (const el of document.querySelectorAll(TEXT_CARRIER_SEL)) {
    considerElement(hits, seen, el, vp, qNorm, matcher, displayedOnly);
  }
  return hits;
};
const sortHits = (hits) => hits.slice().sort((a, b) => b.textScore - a.textScore || b.score - a.score || a.y - b.y || a.x - b.x);
/** #N：textScore（文字匹配分）优先；同分硬可点击在前（含遮挡重排） */
const rankHitsForIndex = (hits) => {
  const filtered = hits.filter((h) => h.textScore >= MIN_TEXT_SCORE);
  const hard = reorderByOcclusion(sortHits(filtered.filter((h) => h.clickable)));
  const soft = sortHits(filtered.filter((h) => !h.clickable));
  return hard.concat(soft).sort((a, b) =>
    b.textScore - a.textScore
    || (Number(!!b.clickable) - Number(!!a.clickable))
    || b.score - a.score
    || a.y - b.y
    || a.x - b.x
  );
};
const isTargetHitOk = (target, x, y) => {
  let el = null;
  try { el = document.elementFromPoint(x, y); } catch (_) { return false; }
  if (!el) return false;
  if (el === target || target.contains(el) || el.contains(target)) return true;
  try {
    if (typeof target.checkVisibility === 'function' && !target.checkVisibility()) return false;
  } catch (_) {}
  return false;
};
const isTargetOccluded = (target) => {
  if (!target || !(target instanceof Element)) return true;
  const candidates = [];
  try {
    const rects = target.getClientRects ? [...target.getClientRects()] : [];
    for (const r of rects) {
      if (r.width < 2 || r.height < 2) continue;
      if (r.bottom <= 0 || r.right <= 0) continue;
      if (r.top >= window.innerHeight || r.left >= window.innerWidth) continue;
      candidates.push(r);
    }
    candidates.sort((a, b) => (b.width * b.height) - (a.width * a.height));
  } catch (_) {}
  if (!candidates.length) {
    const tr = target.getBoundingClientRect();
    if (tr.width >= 2 && tr.height >= 2
      && tr.bottom > 0 && tr.right > 0
      && tr.top < window.innerHeight && tr.left < window.innerWidth) {
      candidates.push(tr);
    }
  }
  if (!candidates.length) return true;
  for (const r of candidates) {
    const cx = r.left + r.width / 2;
    const cy = r.top + r.height / 2;
    if (isTargetHitOk(target, cx, cy)) return false;
    for (const [px, py] of [[0.25, 0.5], [0.75, 0.5], [0.5, 0.35], [0.5, 0.65], [0.15, 0.5]]) {
      if (isTargetHitOk(target, r.left + r.width * px, r.top + r.height * py)) return false;
    }
  }
  return true;
};
const reorderByOcclusion = (hits) => {
  const open = [];
  const masked = [];
  for (const h of hits) {
    if (isTargetOccluded(h.target)) masked.push(h);
    else open.push(h);
  }
  return open.concat(masked);
};
const clickTextHitKey = (h) => {
  const tr = h.target.getBoundingClientRect();
  return Math.round(tr.left) + ':' + Math.round(tr.top) + ':' + h.target.tagName;
};
const __webclawRankClickTextHits = (searchRaw) => {
  const search = (searchRaw || '').trim();
  if (!search) {
    return {
      ok: false,
      search: '',
      qNorm: '',
      matcher: null,
      scanMode: null,
      rankedSource: [],
      ranked: [],
      viewportHits: [],
    };
  }
  const qNorm = clickTextNorm(search);
  const matcher = makeMatcher(qNorm);
  const viewportHits = collectHits(false, qNorm, matcher);
  const strongViewport = viewportHits.filter((h) => h.textScore >= MIN_TEXT_SCORE);
  let scanMode = 'viewport';
  let rankedSource = viewportHits;
  if (!strongViewport.length) {
    rankedSource = collectHits(true, qNorm, matcher);
    scanMode = 'displayed';
  }
  const ranked = rankHitsForIndex(rankedSource);
  return { ok: true, search, qNorm, matcher, scanMode, rankedSource, ranked, viewportHits };
};
"#;

pub const CLICK_DOM: &str = r#"
// click-target-v6.16 — 剥除参数尾部分隔符；同源 iframe topX/topY；Rust 优先顶层坐标点击
const rawArgs = stripTrailingStepSeps((ctx.args || '').trim());
const DEFAULT_WAIT_TIMEOUT = 15;
const OR_PROBE_WAIT_TIMEOUT = 2;
const WAIT_POLL_MS = 300;
const structuralTarget = parseStructuralTarget(rawArgs);
// 扇出短探由 Rust 注入 elementWaitTimeout；未注入时仍用 15s，避免 SPA/iframe 晚出现误判
let elementWaitTimeout = DEFAULT_WAIT_TIMEOUT;
{
  const n = Number(ctx.elementWaitTimeout);
  if (n > 0) elementWaitTimeout = n;
  else if (ctx.orProbe) elementWaitTimeout = OR_PROBE_WAIT_TIMEOUT;
}
const maskWaitTimeout = ctx.orProbe ? 0.4 : Math.min(2.5, elementWaitTimeout);

const parseClickArgs = (raw) => {
  let matchIndex = 1;
  let query = stripTrailingStepSeps(raw || '');
  const idxMatch = query.match(/(?<!\\)#(\d+)$/);
  if (idxMatch) {
    matchIndex = parseInt(idxMatch[1], 10);
    if (!Number.isFinite(matchIndex) || matchIndex < 1) {
      return { ok: false, error: `序号无效：#${idxMatch[1]}（须为 ≥1 的整数）` };
    }
    query = stripTrailingStepSeps(query.slice(0, -idxMatch[0].length));
  }
  query = query.replace(/\\#/g, '#').replace(/\\\\/g, '\\');
  query = stripTrailingStepSeps(query);
  if (!query) {
    return { ok: false, error: '点击目标文字不能为空' };
  }
  return { ok: true, query, matchIndex };
};

const describeClickElement = (el) => {
  if (!el || !(el instanceof Element)) return '';
  const tag = el.tagName || '?';
  const id = el.id ? `#${el.id}` : '';
  let cls = '';
  try {
    const parts = (el.className || '').toString().trim().split(/\s+/).filter(Boolean).slice(0, 2);
    if (parts.length) cls = '.' + parts.join('.');
  } catch (_) {}
  const aria = (el.getAttribute('aria-label') || '').trim();
  const text = getVisibleText(el).trim().slice(0, 40);
  const label = text || aria || ((el.value || '') + '').trim().slice(0, 40);
  return `${tag}${id}${cls}${label ? `「${label}」` : ''}`;
};
const flushFormBeforeSubmit = (target) => {
  const form = target?.form || target?.closest?.('form');
  if (!form) return;
  for (const inp of form.querySelectorAll('input, textarea, select')) {
    const type = ((inp.type || 'text') + '').toLowerCase();
    if (['hidden', 'submit', 'button', 'reset', 'file', 'image'].includes(type)) continue;
    if (!inp.offsetParent && type !== 'hidden') continue;
    try {
      if (typeof inp.setCustomValidity === 'function' && (inp.value || '').trim()) {
        inp.setCustomValidity('');
      }
    } catch (_) {}
    try {
      inp.dispatchEvent(new Event('input', { bubbles: true }));
      inp.dispatchEvent(new Event('change', { bubbles: true }));
    } catch (_) {}
  }
  try {
    if (document.activeElement && document.activeElement !== target) {
      document.activeElement.blur();
    }
  } catch (_) {}
};
const prepareClickCoords = async (target, opts) => {
  const coordOnly = !!(opts && opts.coordOnly);
  const tag = target.tagName;
  const type = ((target.type || '') + '').toLowerCase();
  if (tag === 'INPUT' && type === 'submit') flushFormBeforeSubmit(target);
  try {
    target.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'instant' });
  } catch (_) {}
  if (!coordOnly) {
    try { target.focus({ preventScroll: true }); } catch (_) {}
  }
  await sleep(50);
  // 折行 <a>/内联控件：外接框中心常落在行间空白，CDP 点中父级仅高亮不触发；改用文字行矩形
  const centerOf = (r) => ({
    x: r.left + Math.max(r.width, 1) / 2,
    y: r.top + Math.max(r.height, 1) / 2,
  });
  const candidates = [];
  try {
    const rects = target.getClientRects ? [...target.getClientRects()] : [];
    for (const r of rects) {
      if (r.width < 2 || r.height < 2) continue;
      if (r.bottom <= 0 || r.right <= 0) continue;
      if (r.top >= window.innerHeight || r.left >= window.innerWidth) continue;
      candidates.push(r);
    }
    // 面积大的行优先（通常是主文案行）
    candidates.sort((a, b) => (b.width * b.height) - (a.width * a.height));
  } catch (_) {}
  if (!candidates.length) {
    const tr = target.getBoundingClientRect();
    candidates.push(tr);
  }
  let local;
  // 软命中：不做 elementFromPoint 校验，直接点几何中心（一次坐标点击）
  if (coordOnly) {
    local = centerOf(candidates[0]);
  } else {
    local = null;
    for (const r of candidates) {
      const c = centerOf(r);
      if (isTargetHitOk(target, c.x, c.y)) { local = c; break; }
      // 行内再试几个点：避开空白角
      const pads = [
        [0.25, 0.5], [0.75, 0.5], [0.5, 0.35], [0.5, 0.65], [0.15, 0.5],
      ];
      for (const [px, py] of pads) {
        const x = r.left + r.width * px;
        const y = r.top + r.height * py;
        if (isTargetHitOk(target, x, y)) { local = { x, y }; break; }
      }
      if (local) break;
    }
    // 兜底：仍返回最大行中心（比外接框中心更接近真实文字）
    if (!local) local = centerOf(candidates[0]);
  }
  // 同源 iframe：沿 frameElement 累加到顶层视口，供 CDP Input 直接点击（避免 Rust getBoxModel 偏移误差）
  return toTopViewportCoords(local);
};

/** 将本 frame 视口坐标转为 top 视口；跨域无法读取 parent 时 topX/topY 为 null */
const toTopViewportCoords = (local) => {
  const x = local.x;
  const y = local.y;
  let topX = x;
  let topY = y;
  let inFrame = false;
  try {
    let w = window;
    while (w !== w.top) {
      const f = w.frameElement;
      if (!f) break;
      inFrame = true;
      const r = f.getBoundingClientRect();
      topX += r.left + (f.clientLeft || 0);
      topY += r.top + (f.clientTop || 0);
      w = w.parent;
    }
    return { x, y, topX, topY, inFrame };
  } catch (_) {
    return { x, y, topX: null, topY: null, inFrame: true };
  }
};

const clickOpensNewTab = (el) => {
  if (!el || !(el instanceof Element)) return false;
  if (el.tagName === 'A') {
    const t = (el.getAttribute('target') || '').toLowerCase();
    if (t === '_blank' || t === 'blank' || t === '_new') return true;
  }
  const blob = `${el.getAttribute('onclick') || ''} ${el.getAttribute('ng-click') || ''} ${el.getAttribute('href') || ''}`;
  if (/window\.open\s*\(/i.test(blob)) return true;
  return false;
};

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const isCancelRequested = () => {
  try { return !!window.__webclawCancelRequested; } catch (_) { return false; }
};
try { window.__webclawCancelRequested = false; } catch (_) {}
const waitUntil = async (check, timeoutSec) => {
  const deadline = Date.now() + timeoutSec * 1000;
  while (Date.now() < deadline) {
    if (isCancelRequested()) throw new Error('已取消');
    const hit = check();
    if (hit) return hit;
    await sleep(WAIT_POLL_MS);
  }
  return null;
};

if (structuralTarget) {
  const path = structuralTarget.path;
  const label = structuralTarget.label;
  let resolved = (() => {
    const el = resolveStructuralTarget(path);
    return el && structuralElementVisible(el) ? el : null;
  })();
  let waitedMs = 0;
  if (!resolved) {
    await waitForBlockingMaskGone(maskWaitTimeout);
    const waitStarted = Date.now();
    resolved = await waitUntil(() => {
      const el = resolveStructuralTarget(path);
      return el && structuralElementVisible(el) ? el : null;
    }, elementWaitTimeout);
    if (!resolved) waitedMs = Date.now() - waitStarted;
  }
  const candidateCount = countStructuralCandidates(path);
  if (!resolved) {
    const countHint = candidateCount > 0 ? `（当前共 ${candidateCount} 个候选）` : '';
    const msHint = waitedMs > 0 ? `（实际 ${waitedMs} ms）` : '';
    return {
      ok: false,
      error: `等待 ${elementWaitTimeout}s 后仍未找到「${label}」${countHint}${msHint}`,
      structural: true,
      label,
      candidateCount,
      waited: elementWaitTimeout,
      waitedMs,
    };
  }
  const coords = await prepareClickCoords(resolved);
  const matched = describeClickElement(resolved) || describeStructuralElement(resolved) || label;
  return {
    ok: true,
    clicked: false,
    opensNewTab: clickOpensNewTab(resolved),
    x: coords.x,
    y: coords.y,
    topX: coords.topX,
    topY: coords.topY,
    inFrame: !!coords.inFrame,
    structural: true,
    label,
    matched,
    message: `已点击「${label}」→ ${matched}`,
  };
}

const iconParsed = parseIconClickArgs(rawArgs);
if (iconParsed && iconParsed.ok === false) return iconParsed;
if (iconParsed && iconParsed.iconName) {
  const { iconName, matchIndex } = iconParsed;
  const labelCmd = formatIconClickCmd(iconName, matchIndex, matchIndex > 1 ? matchIndex : 1)
    || ('点击图标:' + iconName + (matchIndex > 1 ? '#' + matchIndex : ''));
  let hits = findIconButtonHits(iconName);
  let waitedMs = 0;
  if (hits.length < matchIndex) {
    await waitForBlockingMaskGone(maskWaitTimeout);
    const waitStarted = Date.now();
    const waited = await waitUntil(() => {
      const h = findIconButtonHits(iconName);
      return h.length >= matchIndex ? h : null;
    }, elementWaitTimeout);
    if (waited) hits = waited;
    else waitedMs = Date.now() - waitStarted;
    if (hits.length < matchIndex) hits = findIconButtonHits(iconName);
  }
  const target = hits[matchIndex - 1];
  if (!target) {
    const indexHint = matchIndex > 1 ? `第 ${matchIndex} 个` : '';
    const countHint = hits.length > 0 ? `（当前共 ${hits.length} 个同名图标）` : '';
    const msHint = waitedMs > 0 ? `（实际 ${waitedMs} ms）` : '';
    return {
      ok: false,
      error: `等待 ${elementWaitTimeout}s 后仍未找到${indexHint}「${labelCmd}」${countHint}${msHint}`,
      iconClick: true,
      iconName,
      matchIndex,
      candidateCount: hits.length,
      waited: elementWaitTimeout,
      waitedMs,
    };
  }
  const coords = await prepareClickCoords(target);
  const matched = describeClickElement(target) || describeStructuralElement(target) || labelCmd;
  const multiHint = hits.length > 1
    ? `（共 ${hits.length} 个「${iconName}」；请用探针复制带 #N 的「点击图标:${iconName}#N」）`
    : '';
  return {
    ok: true,
    clicked: false,
    opensNewTab: clickOpensNewTab(target),
    x: coords.x,
    y: coords.y,
    topX: coords.topX,
    topY: coords.topY,
    inFrame: !!coords.inFrame,
    iconClick: true,
    label: labelCmd,
    iconName,
    matchIndex,
    matched,
    message: `已点击「${labelCmd}」→ ${matched}${multiHint}`,
  };
}

const parsedArgs = parseClickArgs(rawArgs);
if (!parsedArgs.ok) return parsedArgs;
const query = parsedArgs.query;
const matchIndex = parsedArgs.matchIndex;

const findRankedHits = () => __webclawRankClickTextHits(query).ranked;

let best = (() => {
  const strong = findRankedHits();
  return strong.length >= matchIndex ? strong[matchIndex - 1] : null;
})();
let waitedMs = 0;
if (!best) {
  await waitForBlockingMaskGone(maskWaitTimeout);
  const waitStarted = Date.now();
  best = await waitUntil(() => {
  const strong = findRankedHits();
  return strong.length >= matchIndex ? strong[matchIndex - 1] : null;
  }, elementWaitTimeout);
  if (!best) waitedMs = Date.now() - waitStarted;
}
const rankedNow = findRankedHits();
const bestTextScore = best ? best.textScore : (rankedNow[0]?.textScore || 0);
const strongCount = rankedNow.length;
if (!best) {
  const indexHint = matchIndex > 1 ? `第 ${matchIndex} 个` : '';
  const countHint = strongCount > 0 ? `（当前共 ${strongCount} 个足够匹配）` : '';
  const msHint = waitedMs > 0 ? `（实际 ${waitedMs} ms）` : '';
  return {
    ok: false,
    error: `等待 ${elementWaitTimeout}s 后仍未找到${indexHint}「${query}」${countHint}（最高 textScore: ${bestTextScore}，需要 ≥ ${MIN_TEXT_SCORE}）${msHint}`,
    query,
    matchIndex,
    strongCount,
    bestTextScore,
    waited: elementWaitTimeout,
    waitedMs,
  };
}
const target = best.target;
const coordOnly = !!(best.coordOnly || !best.clickable);
const { x, y, topX, topY, inFrame } = await prepareClickCoords(target, { coordOnly });
const label = getVisibleText(target) || best.text;
const elementDesc = describeClickElement(target) || label;
const indexLabel = matchIndex > 1 ? `第 ${matchIndex} 个` : '';
const multiHint = strongCount > 1
  ? `（共 ${strongCount} 个匹配，当前 #${matchIndex}；其它写 点击${query}#N）`
  : '';
const softHint = coordOnly ? '（坐标点击）' : '';
return {
  ok: true,
  clicked: false,
  opensNewTab: clickOpensNewTab(target),
  x,
  y,
  topX,
  topY,
  inFrame: !!inFrame,
  coordOnly,
  query,
  matchIndex,
  strongCount,
  score: Math.round(best.score * 1000) / 1000,
  textScore: best.textScore,
  matched: elementDesc,
  message: `已点击${indexLabel}「${query}」→ ${elementDesc}${multiHint}${softHint}`,
};
"#;

pub const INPUT_FIELD: &str = r#"
// input-target-v14 — 停止时可打断 waitUntil；ctx.elementWaitTimeout 扇出短探；真正禁用/只读才拦截
let raw = (ctx.args || '').trim();
let text = raw;
let field = '';
if (raw.includes('|')) {
  const p = raw.split('|');
  text = (p[0] || '').trim();
  field = (p[1] || '').trim();
} else if (raw.includes('=')) {
  const eq = raw.indexOf('=');
  field = raw.slice(0, eq).trim();
  text = raw.slice(eq + 1).trim();
} else {
  text = raw;
}
const fieldHint = field.toLowerCase();
const structuralField = field ? parseStructuralTarget(field) : null;
const DEFAULT_WAIT_TIMEOUT = 15;
const OR_PROBE_WAIT_TIMEOUT = 2;
const WAIT_POLL_MS = 300;
// 扇出短探由 Rust 注入 elementWaitTimeout；未注入时仍用 15s
let elementWaitTimeout = DEFAULT_WAIT_TIMEOUT;
{
  const n = Number(ctx.elementWaitTimeout);
  if (n > 0) elementWaitTimeout = n;
  else if (ctx.orProbe) elementWaitTimeout = OR_PROBE_WAIT_TIMEOUT;
}
const maskWaitTimeout = ctx.orProbe ? 0.4 : Math.min(2.5, elementWaitTimeout);

const TYPABLE = new Set(['text','search','tel','email','password','url','number','']);
const SKIP_TYPES = new Set(['hidden','checkbox','radio','submit','button','reset','file','image','color','date','datetime-local','month','week','time','range']);

const isDisplayed = (el) => {
  if (!el || !(el instanceof Element)) return false;
  const s = window.getComputedStyle(el);
  if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false;
  const r = el.getBoundingClientRect();
  return r.width >= 4 && r.height >= 4 && r.bottom > 0 && r.right > 0
    && r.top < window.innerHeight && r.left < window.innerWidth;
};

const isLockedInput = (el) => !!(el && (structuralIsTrulyDisabled(el) || structuralIsReadonly(el)));

const lockedInputError = (el) => {
  const why = structuralIsTrulyDisabled(el) ? '已禁用' : '只读';
  // fatal：原因已确定，扇出时无需等其它 frame，执行器也不必刷新重试
  return { ok: false, error: `输入框${why}，请用点击`, field, locked: true, fatal: true };
};

const isVisible = (el) => {
  if (!isDisplayed(el)) return false;
  if (isLockedInput(el)) return false;
  return true;
};

const labelTextFor = (el) => {
  const parts = [];
  if (el.id) {
    const lb = document.querySelector(`label[for="${CSS.escape(el.id)}"]`);
    if (lb) parts.push(lb.innerText || lb.textContent || '');
  }
  const wrap = el.closest('label');
  if (wrap) parts.push(wrap.innerText || wrap.textContent || '');
  return parts.join(' ').trim();
};

const metaOf = (el) => {
  const type = ((el.type || 'text') + '').toLowerCase();
  const ph = (el.placeholder || '').trim();
  const name = (el.name || '').trim();
  const id = (el.id || '').trim();
  const cls = (el.className || '').toString();
  const aria = (el.getAttribute('aria-label') || '').trim();
  const role = (el.getAttribute('role') || '').toLowerCase();
  const title = (el.getAttribute('title') || '').trim();
  const accesskey = (el.getAttribute('accesskey') || '').toLowerCase();
  const autocomplete = (el.getAttribute('autocomplete') || '').toLowerCase();
  const label = labelTextFor(el);
  const rect = el.getBoundingClientRect();
  const blob = `${ph} ${name} ${id} ${cls} ${aria} ${title} ${label}`.toLowerCase();
  return { type, ph, name, id, cls, aria, role, title, accesskey, autocomplete, label, rect, blob };
};

const fieldMatchScore = (m, hint) => {
  if (!hint) return 1;
  if (m.blob.includes(hint)) return 1;
  if (hint.includes('密码') && m.type === 'password') return 1;
  const accountHint = hint.includes('手机') || hint.includes('电话') || hint.includes('账号')
    || hint.includes('邮箱') || hint.includes('用户') || hint.includes('登录');
  if (accountHint && ['tel', 'email', 'text'].includes(m.type)
      && /手机|电话|账号|邮箱|用户|mobile|phone|user|account|tel|mail/i.test(m.blob)) return 0.95;
  return 0;
};

const scoreInput = (el, hint) => {
  const tag = el.tagName;
  if (tag !== 'INPUT' && tag !== 'TEXTAREA') return -1;
  if (!isVisible(el)) return -1;
  const m = metaOf(el);
  if (SKIP_TYPES.has(m.type)) return -1;
  if (tag === 'INPUT' && !TYPABLE.has(m.type)) return -1;

  if (hint) {
    const fm = fieldMatchScore(m, hint);
    if (fm <= 0) return -1;
    return 20 + fm * 5;
  }

  let score = 0;
  if (m.role === 'combobox' || m.role === 'searchbox') score += 5;
  if (m.accesskey === 's') score += 4;
  if (m.name === 'q' || m.id === 'q') score += 5;
  if (el.classList.contains('nav-search-input')) score += 6;
  if (/search|nav-search|combobox|keyword|query/i.test(m.cls)) score += 4;
  if (/搜索|search|keyword|query|请输入/i.test(m.blob)) score += 3;
  if (m.ph && m.autocomplete === 'off') score += 1;
  if (m.type === 'search') score += 3;
  if (m.type === 'password') score -= 6;
  if (m.type === 'number' && m.rect.width < 80) score -= 5;
  if (m.rect.top < 200) score += 2;
  else if (m.rect.top > window.innerHeight * 0.55) score -= 2;
  if (m.rect.width > 220) score += 2;
  else if (m.rect.width > 120) score += 1;
  else if (m.rect.width < 50) score -= 3;
  if (document.activeElement === el) score += 3;
  if (score <= 0 && m.type === 'text' && m.rect.width > 80) score = 1;
  return score;
};

const setNativeValue = (el, value) => {
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  const desc = Object.getOwnPropertyDescriptor(proto, 'value');
  if (desc && desc.set) desc.set.call(el, value);
  else el.value = value;
};

const commitInputValue = (target, value) => {
  setNativeValue(target, value);
  try {
    if (typeof target.setCustomValidity === 'function') target.setCustomValidity('');
  } catch (_) {}
  try {
    target.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true, data: value }));
  } catch (_) {}
  try {
    target.dispatchEvent(new InputEvent('input', { bubbles: true, data: value, inputType: 'insertText' }));
  } catch (_) {
    target.dispatchEvent(new Event('input', { bubbles: true }));
  }
  target.dispatchEvent(new Event('change', { bubbles: true }));
};

const clearTarget = (target) => {
  target.focus();
  target.click();
  if (target.isContentEditable) {
    target.textContent = '';
    target.dispatchEvent(new InputEvent('input', { bubbles: true }));
  } else if ('value' in target) {
    setNativeValue(target, '');
    target.dispatchEvent(new Event('input', { bubbles: true }));
  }
};

const prepareForCdpInsert = (target) => {
  try { target.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'instant' }); } catch (_) {}
  target.focus();
  target.click();
  try {
    if (typeof target.select === 'function') target.select();
    else if (typeof target.setSelectionRange === 'function') {
      const len = (target.value || '').length;
      target.setSelectionRange(0, len);
    }
  } catch (_) {}
};

const commitContentEditable = (el, value) => {
  el.focus();
  el.textContent = value;
  el.dispatchEvent(new InputEvent('input', { bubbles: true, data: value, inputType: 'insertText' }));
};

const selector = 'input:not([type=hidden]):not([type=checkbox]):not([type=radio]):not([type=submit]):not([type=button]):not([type=reset]):not([type=file]), textarea';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const isCancelRequested = () => {
  try { return !!window.__webclawCancelRequested; } catch (_) { return false; }
};
try { window.__webclawCancelRequested = false; } catch (_) {}
const waitUntil = async (check, timeoutSec) => {
  const deadline = Date.now() + timeoutSec * 1000;
  while (Date.now() < deadline) {
    if (isCancelRequested()) throw new Error('已取消');
    const hit = check();
    if (hit) return hit;
    await sleep(WAIT_POLL_MS);
  }
  return null;
};
const findBestInput = () => {
  if (structuralField) {
    const el = resolveStructuralTarget(structuralField.path);
    if (el && isDisplayed(el)) return el;
    return null;
  }
  const ranked = [...document.querySelectorAll(selector)]
    .map((el) => ({ el, s: scoreInput(el, fieldHint) }))
    .filter((x) => x.s >= 0)
    .sort((a, b) => b.s - a.s);
  if (ranked[0]?.el) return ranked[0].el;
  if (!fieldHint) {
    const a = document.activeElement;
    if (a && (a.tagName === 'INPUT' || a.tagName === 'TEXTAREA') && isVisible(a)) return a;
  }
  return null;
};

/** 命中只读/禁用框时给出简短提示（这类框不计入可输入候选）。
 *  `allowNoHint` 仅用于等待超时后兜底：未写字段名时避免把「只读」误报成加载未完成。 */
const findLockedInputByHint = (allowNoHint = false) => {
  if (structuralField) return null;
  if (!fieldHint && !allowNoHint) return null;
  const ranked = [...document.querySelectorAll(selector)]
    .filter((el) => isDisplayed(el) && isLockedInput(el))
    .map((el) => {
      const tag = el.tagName;
      if (tag !== 'INPUT' && tag !== 'TEXTAREA') return { el, s: -1 };
      const m = metaOf(el);
      if (SKIP_TYPES.has(m.type)) return { el, s: -1 };
      if (tag === 'INPUT' && !TYPABLE.has(m.type)) return { el, s: -1 };
      const fm = fieldHint ? fieldMatchScore(m, fieldHint) : 1;
      return { el, s: fm > 0 ? fm : -1 };
    })
    .filter((x) => x.s >= 0)
    .sort((a, b) => b.s - a.s);
  return ranked[0]?.el || null;
};

let target = findBestInput();
let waitedMs = 0;
if (!target) {
  const lockedHint = findLockedInputByHint();
  if (lockedHint) return lockedInputError(lockedHint);
  await waitForBlockingMaskGone(maskWaitTimeout);
  const waitStarted = Date.now();
  target = await waitUntil(() => findBestInput(), elementWaitTimeout);
  if (!target) waitedMs = Date.now() - waitStarted;
}
if (!target) {
  const lockedHint = findLockedInputByHint(true);
  if (lockedHint) return lockedInputError(lockedHint);
  const msHint = waitedMs > 0 ? `（实际 ${waitedMs} ms）` : '';
  return {
    ok: false,
    error: field
      ? (structuralField
        ? `等待 ${elementWaitTimeout}s 后仍未找到「${field}」${msHint}`
        : `等待 ${elementWaitTimeout}s 后仍未找到字段「${field}」${msHint}`)
      : `等待 ${elementWaitTimeout}s 后仍未找到可用输入框${msHint}`,
    field,
    waited: elementWaitTimeout,
    waitedMs,
  };
}

if (isLockedInput(target)) {
  return lockedInputError(target);
}

const m = metaOf(target);
const label = structuralField
  ? (field || describeStructuralElement(target))
  : (m.ph || m.aria || m.title || m.name || m.id || m.label || '输入框');
const preview = text
  ? (text.length <= 2 ? '*'.repeat(text.length) : text[0] + '***')
  : '';

const buildFocusAnchor = () => {
  if (structuralField && field) return { kind: 'structural', value: field };
  if (field) return { kind: 'fieldHint', value: field };
  return { kind: 'fieldHint', value: label.slice(0, 80) };
};

if (!text) {
  clearTarget(target);
  return {
    ok: true,
    text,
    field,
    matched: label.slice(0, 80),
    focusAnchor: buildFocusAnchor(),
    message: field ? `已清空 ${field}=` : `已清空「${label.slice(0, 40)}」`,
    needInsertText: '',
  };
}

if (target.isContentEditable) {
  commitContentEditable(target, text);
  return {
    ok: true,
    text,
    field,
    matched: label.slice(0, 80),
    focusAnchor: buildFocusAnchor(),
    message: field ? `已输入 ${field}=${preview}` : `已输入「${preview}」→ ${label.slice(0, 40)}`,
    needInsertText: '',
  };
}

prepareForCdpInsert(target);
commitInputValue(target, text);
return {
  ok: true,
  text,
  field,
  matched: label.slice(0, 80),
  focusAnchor: buildFocusAnchor(),
  message: field ? `已输入 ${field}=${preview}` : `已输入「${preview}」→ ${label.slice(0, 40)}`,
  needInsertText: '',
};
"#;

pub const SCROLL_PAGE: &str = r#"
// ctx.args = "500" / "下500" / "up 300" / "左200"
let raw = (ctx.args || '400').trim().toLowerCase();
let dx = 0, dy = 400;
if (raw.includes('上') || raw.startsWith('up')) dy = -Math.abs(parseInt(raw.replace(/\D/g,''))||400);
else if (raw.includes('下') || raw.startsWith('down')) dy = Math.abs(parseInt(raw.replace(/\D/g,''))||400);
else if (raw.includes('左') || raw.startsWith('left')) { dx = -Math.abs(parseInt(raw.replace(/\D/g,''))||400); dy = 0; }
else if (raw.includes('右') || raw.startsWith('right')) { dx = Math.abs(parseInt(raw.replace(/\D/g,''))||400); dy = 0; }
else dy = parseInt(raw.replace(/\D/g,'')) || 400;
window.scrollBy({ left: dx, top: dy, behavior: 'instant' });
return { ok: true, message: `滚动 dx=${dx} dy=${dy}` };
"#;

pub const PRESS_KEY: &str = r#"
// press-key-v3 — CDP 按键 + 焦点锚点 refocus + 通用 DOM 提交回退（Rust）
// ctx.args: "回车" / "Tab2" / "回车到输入框#1"
const raw = (ctx.args || '').trim();
if (!raw) return { ok: false, error: '按键不能为空，例如：按回车 / 按Tab / 按回车到输入框#1' };

let keyPart = raw;
let targetPart = '';
const toIdx = raw.indexOf('到');
if (toIdx > 0) {
  keyPart = raw.slice(0, toIdx).trim();
  targetPart = raw.slice(toIdx + 1).trim();
}

const parseRepeat = (s) => {
  const m = (s || '').trim().match(/(\d+)$/);
  if (!m) return { ok: true, keyPart: (s || '').trim(), repeat: 1 };
  const n = parseInt(m[1], 10);
  if (!Number.isFinite(n) || n < 1 || n > 20) return { ok: false, error: `重复次数无效：${m[1]}（1~20）` };
  return { ok: true, keyPart: (s || '').trim().slice(0, -(m[1] || '').length).trim(), repeat: n };
};

const rep = parseRepeat(keyPart);
if (!rep.ok) return rep;
keyPart = rep.keyPart;
const repeat = rep.repeat;

const norm = (s) => (s || '').trim().toLowerCase().replace(/\s+/g, '');
const k = norm(keyPart);
const map = [
  { keys: ['回车','enter','return','确定'], key: 'Enter', code: 'Enter', keyCode: 13, text: null },
  { keys: ['tab','制表','下一项','下一个'], key: 'Tab', code: 'Tab', keyCode: 9, text: null },
  { keys: ['esc','escape','退出','取消'], key: 'Escape', code: 'Escape', keyCode: 27, text: null },
  { keys: ['退格','backspace'], key: 'Backspace', code: 'Backspace', keyCode: 8, text: null },
  { keys: ['空格','space'], key: ' ', code: 'Space', keyCode: 32, text: ' ' },
  { keys: ['上','上箭头','arrowup','up'], key: 'ArrowUp', code: 'ArrowUp', keyCode: 38, text: null },
  { keys: ['下','下箭头','arrowdown','down'], key: 'ArrowDown', code: 'ArrowDown', keyCode: 40, text: null },
  { keys: ['左','左箭头','arrowleft','left'], key: 'ArrowLeft', code: 'ArrowLeft', keyCode: 37, text: null },
  { keys: ['右','右箭头','arrowright','right'], key: 'ArrowRight', code: 'ArrowRight', keyCode: 39, text: null },
];

const found = map.find((x) => x.keys.some((kk) => norm(kk) === k));
if (!found) {
  return {
    ok: false,
    error: `未知按键「${keyPart}」。支持：回车/Enter、Tab、Esc、空格、方向键（上/下/左/右）`,
  };
}

let focusAnchor = null;
if (targetPart) {
  const sp = parseStructuralTarget(targetPart);
  focusAnchor = sp
    ? { kind: 'structural', value: targetPart }
    : { kind: 'fieldHint', value: targetPart };
} else if (ctx.focusAnchor && ctx.focusAnchor.value) {
  focusAnchor = ctx.focusAnchor;
}

const label = found.key === ' ' ? '空格' : found.key;
return {
  ok: true,
  delegate: 'rust_press_key',
  key: found.key,
  code: found.code,
  keyCode: found.keyCode,
  text: found.text,
  repeat,
  focusAnchor,
  message: repeat > 1 ? `已按 ${label} x${repeat}` : `已按 ${label}`,
};
"#;

pub const WAIT_PAGE: &str = r#"
// wait-smart-v7 — 纯数字等待忽略尾部顿号/逗号；等待文字不要求在视口内
const DEFAULT_TIMEOUT = 30;
const DOWNLOAD_DEFAULT_TIMEOUT = 200;
const POLL_MS = 300;

const coalesce = (...vals) => {
  for (let i = 0; i < vals.length; i++) {
    const v = vals[i];
    if (v != null && v !== '') return v;
  }
  return vals.length ? vals[vals.length - 1] : null;
};

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** 菜谱拆分残留的尾部标点（如「等待1,」） */
const stripWaitTailPunct = (s) =>
  (s || '').trim().replace(/[,，。；;、．.]+$/g, '').trim();

/** 是否为固定秒数等待（允许尾部标点） */
const parseSleepSeconds = (raw) => {
  const s = stripWaitTailPunct((raw || '').trim().replace(/秒$/u, '').trim());
  if (!s) return null;
  if (!/^\d+(?:\.\d+)?$/.test(s)) return null;
  const n = parseFloat(s);
  return Number.isFinite(n) ? n : null;
};

/** CSS 已展示（有尺寸）；不要求落在视口内——局部滚动日志底部也算出现 */
const isDisplayed = (el) => {
  if (!el || !(el instanceof Element)) return false;
  const s = window.getComputedStyle(el);
  if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false;
  const rect = el.getBoundingClientRect();
  return rect.width >= 1 && rect.height >= 1;
};

/** 视口内可见：选择器/加载遮罩等仍用这个，避免点到屏外遮罩误判 */
const isVisible = (el) => {
  if (!isDisplayed(el)) return false;
  if (window.getComputedStyle(el).pointerEvents === 'none') return false;
  const rect = el.getBoundingClientRect();
  return rect.bottom > 0 && rect.right > 0
    && rect.top < window.innerHeight && rect.left < window.innerWidth;
};

const getVisibleText = (el) => {
  if (!el || !(el instanceof Element)) return '';
  const parts = [];
  try {
    const it = (el.innerText || '').trim();
    if (it) parts.push(it);
  } catch (_) {}
  for (const p of [
    el.value,
    el.getAttribute('value'),
    el.getAttribute('placeholder'),
    el.getAttribute('aria-label'),
    el.getAttribute('title'),
    el.textContent,
  ]) {
    const t = (p || '').trim();
    if (t) parts.push(t);
  }
  return parts.join(' ').trim().slice(0, 200);
};

const textMatches = (text, query) => {
  const tl = normUiText(text);
  const ql = normUiText(query);
  if (!tl || !ql) return false;
  return tl === ql || tl.includes(ql);
};

const pageHasVisibleText = (query) => {
  const q = (query || '').trim();
  if (!q) return false;
  const nodes = document.querySelectorAll(
    'a, button, input, textarea, label, span, div, p, li, td, th, h1, h2, h3, h4, h5, h6, pre, code, [role="button"], [role="link"], [role="menuitem"], [role="log"], [role="status"]'
  );
  for (const el of nodes) {
    // 等待文字：不要求滚进视口；日志在滚动容器底部也可命中
    if (!isDisplayed(el)) continue;
    if (textMatches(getVisibleText(el), q)) return true;
  }
  // 兜底：整页 innerText（含局部滚动区内未扫到的节点拼接）
  try {
    if (textMatches(document.body && document.body.innerText ? document.body.innerText : '', q)) return true;
  } catch (_) {}
  return false;
};

const parseTrailingTimeout = (part) => {
  const m = (part || '').trim().match(/(\d+(?:\.\d+)?)\s*$/);
  if (!m) return null;
  const n = parseFloat(m[1]);
  return Number.isFinite(n) ? n : null;
};

const splitBodyAndTimeout = (part) => {
  const timeout = coalesce(parseTrailingTimeout(part), DEFAULT_TIMEOUT);
  let body = (part || '').trim();
  const t = parseTrailingTimeout(body);
  if (t != null) body = body.replace(/\d+(?:\.\d+)?\s*$/, '').trim();
  return { body, timeout };
};

const splitStructuralWait = (part) => {
  const s = (part || '').trim();
  if (!s) return null;
  const preferredTimeouts = new Set([5, 10, 15, 20, 30, 45, 60, 90, 120, 180, 200, 300]);
  const candidates = [];
  if (parseStructuralTarget(s)) {
    candidates.push({ body: s, timeout: DEFAULT_TIMEOUT, take: 0 });
  }
  const spaced = s.match(/^(.+?)\s+(\d+(?:\.\d+)?)$/);
  if (spaced && parseStructuralTarget(spaced[1].trim())) {
    const t = parseFloat(spaced[2]);
    if (Number.isFinite(t) && t >= 1) {
      return { body: spaced[1].trim(), timeout: t };
    }
  }
  const m = s.match(/(\d+(?:\.\d+)?)$/);
  if (m) {
    const digits = m[1];
    for (let i = 1; i < digits.length; i++) {
      const timeoutStr = digits.slice(i);
      const body = s.slice(0, s.length - timeoutStr.length);
      const t = parseFloat(timeoutStr);
      if (!(Number.isFinite(t) && t >= 1)) continue;
      if (!parseStructuralTarget(body)) continue;
      candidates.push({ body, timeout: t, take: i });
    }
  }
  if (!candidates.length) return null;
  const preferred = candidates.filter((c) => c.take > 0 && preferredTimeouts.has(c.timeout));
  if (preferred.length) {
    preferred.sort((a, b) => b.take - a.take);
    return { body: preferred[0].body, timeout: preferred[0].timeout };
  }
  const full = candidates.find((c) => c.take === 0);
  if (full) return { body: full.body, timeout: full.timeout };
  return { body: candidates[0].body, timeout: candidates[0].timeout };
};

const parseWaitArgs = (raw) => {
  let s = (raw || '').trim().replace(/秒$/u, '').trim();
  if (!s) return { mode: 'sleep', timeout: DEFAULT_TIMEOUT, body: '' };
  const sleepSec = parseSleepSeconds(s);
  if (sleepSec != null) {
    return { mode: 'sleep', timeout: sleepSec, body: '' };
  }
  s = stripWaitTailPunct(s);
  if (s.startsWith('下载')) {
    const rest = s.slice(2).trim();
    let timeout = DOWNLOAD_DEFAULT_TIMEOUT;
    if (rest) {
      const t = parseTrailingTimeout(rest);
      const n = parseFloat(String(rest).replace(/\D/g, ''));
      timeout = coalesce(t, Number.isFinite(n) ? n : null, DOWNLOAD_DEFAULT_TIMEOUT);
    }
    return { mode: 'download', timeout, body: '' };
  }
  if (s.startsWith('加载')) {
    const { body, timeout } = splitBodyAndTimeout(s.slice(2));
    return { mode: 'load', timeout, body };
  }
  if (s.startsWith('文字')) {
    const { body, timeout } = splitBodyAndTimeout(s.slice(2).replace(/^[:：]/, '').trim());
    return { mode: 'text', timeout, body };
  }
  if (s.startsWith('选择器')) {
    const { body, timeout } = splitBodyAndTimeout(s.slice(3).replace(/^[:：]/, '').trim());
    return { mode: 'selector', timeout, body };
  }
  if (s.startsWith('消失')) {
    const { body, timeout } = splitBodyAndTimeout(s.slice(2).replace(/^[:：]/, '').trim());
    return { mode: 'gone', timeout, body };
  }
  if (s.startsWith('遮罩')) {
    const { body, timeout } = splitBodyAndTimeout(s.slice(2).replace(/^[:：]/, '').trim());
    return { mode: 'mask', timeout, body };
  }
  if (new RegExp('^(' + STRUCTURAL_TYPES + ')#').test(s)) {
    const split = splitStructuralWait(s);
    if (split) {
      return { mode: 'element', timeout: split.timeout, body: split.body };
    }
  }
  // shorthand: 登录 / 登录30 / 时间管理30 (same as 文字登录 / 文字登录30)
  const shorthand = splitBodyAndTimeout(s);
  if (shorthand.body) {
    return { mode: 'text', timeout: shorthand.timeout, body: shorthand.body };
  }
  return { mode: 'unknown', timeout: DEFAULT_TIMEOUT, body: s };
};

const waitUntil = async (check, timeoutSec, label) => {
  const started = Date.now();
  const deadline = started + timeoutSec * 1000;
  while (Date.now() < deadline) {
    try {
      if (window.__webclawCancelRequested) {
        return { ok: false, error: '已取消', cancelled: true, waited: 0, waitedMs: Date.now() - started };
      }
    } catch (_) {}
    const hit = check();
    if (hit && hit.ok) return hit;
    await sleep(POLL_MS);
  }
  const waitedMs = Date.now() - started;
  return {
    ok: false,
    error: '等待超时（' + timeoutSec + 's）：' + label + '（实际 ' + waitedMs + ' ms）',
    waited: timeoutSec,
    waitedMs,
  };
};

const parsed = parseWaitArgs(ctx.args || '');

switch (parsed.mode) {
  case 'sleep': {
    await sleep(parsed.timeout * 1000);
    return { ok: true, message: '已等待 ' + parsed.timeout + ' 秒' };
  }
  case 'download': {
    return {
      ok: true,
      delegate: 'rust_wait_download',
      timeoutSecs: Math.ceil(parsed.timeout),
      message: '等待下载完成（最多 ' + parsed.timeout + 's）',
    };
  }
  case 'load': {
    const r = await waitUntil(() => {
      const rs = document.readyState;
      if (rs === 'complete' || rs === 'interactive') return { ok: true, detail: rs };
      return null;
    }, parsed.timeout, '页面加载');
    if (!r.ok) return r;
    return { ok: true, message: '页面已加载 (' + r.detail + ')' };
  }
  case 'text': {
    if (!parsed.body) return { ok: false, error: '「文字」缺少目标文字，如：等待文字登录10 或 等待登录10' };
    const r = await waitUntil(() => {
      if (pageHasVisibleText(parsed.body)) return { ok: true, matched: parsed.body };
      return null;
    }, parsed.timeout, '文字「' + parsed.body + '」');
    if (!r.ok) return r;
    return { ok: true, message: '已出现文字「' + r.matched + '」' };
  }
  case 'selector': {
    if (!parsed.body) return { ok: false, error: '「选择器」缺少 CSS 选择器，如：等待选择器#login_submit15' };
    const r = await waitUntil(() => {
      try {
        const el = document.querySelector(parsed.body);
        if (el && isVisible(el)) return { ok: true, matched: parsed.body };
      } catch (_) {}
      return null;
    }, parsed.timeout, '选择器「' + parsed.body + '」');
    if (!r.ok) return r;
    return { ok: true, message: '元素已可见：' + r.matched };
  }
  case 'gone': {
    if (!parsed.body) return { ok: false, error: '「消失」缺少目标文字，如：等待消失加载中10' };
    const r = await waitUntil(() => {
      if (!pageHasVisibleText(parsed.body)) return { ok: true, matched: parsed.body };
      return null;
    }, parsed.timeout, '文字「' + parsed.body + '」消失');
    if (!r.ok) return r;
    return { ok: true, message: '文字已消失：' + r.matched };
  }
  case 'mask': {
    const sel = parsed.body || '.loading, .ant-spin, .el-loading-mask, [class*="loading"], [class*="Loading"], [class*="spinner"]';
    const r = await waitUntil(() => {
      let any = false;
      try {
        for (const el of document.querySelectorAll(sel)) {
          if (isVisible(el)) { any = true; break; }
        }
      } catch (_) {}
      if (!any) return { ok: true, matched: sel };
      return null;
    }, parsed.timeout, '遮罩消失');
    if (!r.ok) return r;
    return { ok: true, message: '加载遮罩已消失' };
  }
  case 'element': {
    if (!parsed.body) return { ok: false, error: '「元素」缺少结构化目标，如：等待按钮#2 10' };
    const sp = parseStructuralTarget(parsed.body);
    if (!sp) return { ok: false, error: '无法解析结构化目标「' + parsed.body + '」' };
    const r = await waitUntil(() => {
      const el = resolveStructuralTarget(sp.path);
      if (el && structuralElementVisible(el)) return { ok: true, matched: parsed.body };
      return null;
    }, parsed.timeout, '元素「' + parsed.body + '」');
    if (!r.ok) return r;
    return { ok: true, message: '元素已可见：' + r.matched };
  }
  default:
    return { ok: false, error: '无法识别的等待参数「' + parsed.body + '」。支持：2、登录、登录10、文字登录10、选择器#btn15、消失加载中10、加载10、下载（默认200）、下载60、按钮#2 10' };
}
"#;

pub const OPEN_TAB_HINT: &str = r#"
// 打开标签由 Rust CDP 完成；此脚本仅作占位说明
return { ok: true, delegate: 'rust_new_tab', url: (ctx.args||'').trim(), message: '将由 CDP 新建标签页' };
"#;

pub const CLOSE_TAB_HINT: &str = r#"
// 关闭标签由 Rust CDP 完成；此脚本仅作占位说明
return { ok: true, delegate: 'rust_close_tab', message: '将由 CDP 关闭当前标签页' };
"#;

pub const LOGIN_SYSTEM: &str = r#"
// login-system-v6 — v5 + 会话有效直达业务页时视为已登录（勿因无表单报错中断）
const cred = ctx.credentials || {};
const username = (cred.username || '').trim();
const password = (cred.password || '').trim();
const systemName = (cred.systemName || cred.systemId || ctx.args || '').trim();
const loginUrlHint = (cred.loginUrl || '').trim().toLowerCase();
if (!username || !password) {
  return { ok: false, error: '未配置登录账号或密码，请打开「网站账号设置」维护后重试' };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const normUiText = (s) => (s || '').trim().toLowerCase().replace(/\s+/g, '');

const TYPABLE = new Set(['text','search','tel','email','password','url','number','']);
const SKIP_TYPES = new Set(['hidden','checkbox','radio','submit','button','reset','file','image']);

const isVisible = (el) => {
  if (!el || !(el instanceof Element)) return false;
  if (el.disabled) return false;
  const s = window.getComputedStyle(el);
  if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false;
  const r = el.getBoundingClientRect();
  return r.width >= 4 && r.height >= 4 && r.bottom > 0 && r.right > 0
    && r.top < window.innerHeight && r.left < window.innerWidth;
};

const labelTextFor = (el) => {
  const parts = [];
  if (el.id) {
    const lb = document.querySelector(`label[for="${CSS.escape(el.id)}"]`);
    if (lb) parts.push(lb.innerText || lb.textContent || '');
  }
  const wrap = el.closest('label');
  if (wrap) parts.push(wrap.innerText || wrap.textContent || '');
  return parts.join(' ').trim();
};

const metaOf = (el) => {
  const type = ((el.type || 'text') + '').toLowerCase();
  const blob = `${el.placeholder||''} ${el.name||''} ${el.id||''} ${el.className||''} ${el.getAttribute('aria-label')||''} ${labelTextFor(el)}`.toLowerCase();
  return { type, blob, rect: el.getBoundingClientRect() };
};

const setNativeValue = (el, value) => {
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  const desc = Object.getOwnPropertyDescriptor(proto, 'value');
  if (desc && desc.set) desc.set.call(el, value);
  else el.value = value;
};

const dispatchInput = (el) => {
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
};

const fillInput = (el, value) => {
  try { if (el.readOnly) el.readOnly = false; } catch (_) {}
  el.focus();
  el.click();
  setNativeValue(el, value);
  dispatchInput(el);
};

const isCaptchaField = (blob) => /验证码|校验码|captcha|vcode|verifycode|verify.?code/i.test(blob || '');

const scoreAccountInput = (el) => {
  if (el.tagName !== 'INPUT' && el.tagName !== 'TEXTAREA') return -1;
  if (!isVisible(el)) return -1;
  const m = metaOf(el);
  if (SKIP_TYPES.has(m.type) || m.type === 'password') return -1;
  if (isCaptchaField(m.blob)) return -1;
  if (el.tagName === 'INPUT' && !TYPABLE.has(m.type)) return -1;
  let score = 0;
  if (['tel','email','text','search','number'].includes(m.type)) score += 5;
  if (/手机|电话|账号|邮箱|用户|工号|mobile|phone|user|account|tel|mail|username|login/i.test(m.blob)) score += 8;
  if (m.type === 'tel') score += 4;
  if (m.type === 'email') score += 3;
  if (m.rect.top < window.innerHeight * 0.6) score += 2;
  if (m.rect.width > 120) score += 2;
  return score;
};

const scorePasswordInput = (el) => {
  if (el.tagName !== 'INPUT') return -1;
  if (!isVisible(el)) return -1;
  const m = metaOf(el);
  if (m.type === 'password') return 20;
  if (/密码|password|pwd|pass/i.test(m.blob)) return 15;
  return -1;
};

const pickBest = (scorer) => {
  let best = null;
  let bestScore = -1;
  for (const el of document.querySelectorAll('input, textarea')) {
    const s = scorer(el);
    if (s > bestScore) { bestScore = s; best = el; }
  }
  return bestScore > 0 ? best : null;
};

const pickAccountInput = () => {
  const preferred = [
    'input#username',
    'input[name="username"]',
    'input[id="username"]',
    'input[placeholder*="用户名"]',
    'input[placeholder*="账号"]',
  ];
  for (const sel of preferred) {
    const el = document.querySelector(sel);
    if (el && scoreAccountInput(el) > 0) return el;
  }
  return pickBest(scoreAccountInput);
};

const pickPasswordInput = () => {
  const preferred = [
    'input#password',
    'input[name="password"]',
    'input[id="password"]',
    'input[type="password"]',
    'input[placeholder*="密码"]',
  ];
  for (const sel of preferred) {
    const el = document.querySelector(sel);
    if (el && scorePasswordInput(el) > 0) return el;
  }
  return pickBest(scorePasswordInput);
};

const loginText = (el) => normUiText(`${el.innerText||''} ${el.value||''} ${el.getAttribute('aria-label')||''} ${el.textContent||''}`);

const looksLikeLoginForm = () => !!(pickPasswordInput() && pickAccountInput());

const isLoginLikeUrl = (href) => {
  const h = (href || '').toLowerCase();
  if (/login|signin|sign-in|passport|oauth|sso|\/auth\b/i.test(h)) return true;
  if (loginUrlHint) {
    const hint = loginUrlHint.split('?')[0];
    if (h.startsWith(hint)) return true;
    // Hash SPA：仅把同域下的 #/、#/login 等入口视为登录页
    if (hint.includes('#')) {
      try {
        const a = document.createElement('a');
        a.href = hint;
        const hash = (location.hash || '').toLowerCase();
        if (a.host && location.host === a.host
            && (!hash || hash === '#' || hash === '#/' || /login|signin|auth|passport/.test(hash))) {
          return true;
        }
      } catch (_) {}
    }
  }
  return false;
};

const hasLoggedInSignal = () => {
  const logoutRe = /退出|注销|登出|logout|signout|sign.?out/;
  for (const el of document.querySelectorAll('a, button, [role="button"], span, div')) {
    if (!isVisible(el)) continue;
    if (logoutRe.test(loginText(el))) return true;
  }
  return false;
};

const detectAlreadyLoggedIn = () => {
  if (looksLikeLoginForm()) {
    return { skip: false, reason: '' };
  }
  const href = (location.href || '').toLowerCase();
  if (hasLoggedInSignal()) {
    return { skip: true, reason: '检测到退出/登出入口' };
  }
  if (!isLoginLikeUrl(href)) {
    return { skip: true, reason: '已进入业务页（无登录表单）' };
  }
  // 仍像登录页但一时无表单：交给外层等待，不要误判已登录
  return { skip: false, reason: 'waiting', pending: true };
};

for (let i = 0; i < 40; i++) {
  if (looksLikeLoginForm()) break;
  const early = detectAlreadyLoggedIn();
  if (early.skip) {
    return {
      ok: true,
      alreadyLoggedIn: true,
      skipped: true,
      clicked: false,
      systemName,
      username,
      message: `已处于登录态，跳过填表（${early.reason}）`,
    };
  }
  await sleep(200);
}

const already = detectAlreadyLoggedIn();
if (already.skip) {
  return {
    ok: true,
    alreadyLoggedIn: true,
    skipped: true,
    clicked: false,
    systemName,
    username,
    message: `已处于登录态，跳过填表（${already.reason}）`,
  };
}

// 已等待仍无登录表单：多为会话有效，系统直接进业务页
if (!looksLikeLoginForm()) {
  return {
    ok: true,
    alreadyLoggedIn: true,
    skipped: true,
    clicked: false,
    systemName,
    username,
    message: '已处于登录态，跳过填表（无登录表单，已进入业务页）',
  };
}

const userEl = pickAccountInput();
const pwdEl = pickPasswordInput();
if (!userEl) return { ok: false, error: '未找到账号/手机号输入框' };
if (!pwdEl) return { ok: false, error: '未找到密码输入框' };

fillInput(userEl, username);
fillInput(pwdEl, password);

const loginKw = ['登录', 'login', 'sign in', 'signin', 'submit', '确定'];
const isClickable = (el) => {
  if (!el || el.disabled) return false;
  const tag = el.tagName;
  if (tag === 'BUTTON' || tag === 'A') return true;
  if (tag === 'INPUT' && ['submit','button'].includes((el.type||'').toLowerCase())) return true;
  if (tag === 'DIV' && /\bloginBtn\b/i.test(String(el.className || ''))) return true;
  const role = (el.getAttribute('role') || '').toLowerCase();
  return role === 'button' || el.hasAttribute('onclick');
};

let loginBtn = null;
const preferredBtns = [
  'button.ant-btn-primary[type="submit"]',
  'button[type="submit"].ant-btn-primary',
  'form button[type="submit"]',
  'button.ant-btn-primary',
  '.loginBtn',
];
for (const sel of preferredBtns) {
  const el = document.querySelector(sel);
  if (!el || !isVisible(el) || !isClickable(el)) continue;
  if (sel.includes('loginBtn') || loginKw.some((k) => loginText(el).includes(normUiText(k))) || (el.type || '').toLowerCase() === 'submit') {
    loginBtn = el;
    break;
  }
}
if (!loginBtn) {
  for (const el of document.querySelectorAll('button, input[type="submit"], input[type="button"], a, [role="button"], div.loginBtn, .loginBtn')) {
    if (!isVisible(el) || !isClickable(el)) continue;
    if (loginKw.some((k) => loginText(el).includes(normUiText(k)))) { loginBtn = el; break; }
  }
}
if (!loginBtn) {
  for (const el of document.querySelectorAll('button, input[type="submit"], div.loginBtn, .loginBtn')) {
    if (isVisible(el) && isClickable(el)) { loginBtn = el; break; }
  }
}
if (!loginBtn) return { ok: false, error: '未找到登录按钮' };

try { loginBtn.scrollIntoView({ block: 'center', inline: 'nearest', behavior: 'instant' }); } catch (_) {}
try { loginBtn.focus({ preventScroll: true }); } catch (_) {}
const tr = loginBtn.getBoundingClientRect();
const x = tr.left + tr.width / 2;
const y = tr.top + tr.height / 2;

return {
  ok: true,
  alreadyLoggedIn: false,
  clicked: false,
  x,
  y,
  systemName,
  username,
  message: `已提交登录「${systemName}」`,
};
"#;

/// 安装点击文字探针到 window（整包只在 needInstall 时发一次）。
pub fn compose_ensure_click_text_probe_installed() -> String {
    let ver = CLICK_TEXT_PROBE_API_VERSION;
    format!(
        "(() => {{ {body}\n window.__webclawClickTextProbeApi = {{ version: {ver}, scan: __webclawClickTextProbeScan, apply: __webclawClickTextProbeApply, clear: __webclawClickTextProbeClear }}; return {{ ok: true, installed: true }}; }})()",
        body = click_text_probe_js(),
        ver = ver
    )
}

/// 点击文字解释探针：短调用，不携带整包。
pub fn compose_click_text_probe_page(search: &str) -> String {
    let search_json = serde_json::to_string(search).unwrap_or_else(|_| "\"\"".to_string());
    let ver = CLICK_TEXT_PROBE_API_VERSION;
    format!(
        "(() => {{ const a = window.__webclawClickTextProbeApi; if (!a || a.version !== {ver} || typeof a.scan !== 'function') return {{ ok: false, needInstall: true, items: [], countsByType: {{}} }}; return a.scan({search_json}); }})()"
    )
}

pub fn compose_click_text_probe_overlay(payload: &serde_json::Value) -> String {
    let payload_json = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
    let ver = CLICK_TEXT_PROBE_API_VERSION;
    format!(
        "(() => {{ const a = window.__webclawClickTextProbeApi; if (!a || a.version !== {ver} || typeof a.apply !== 'function') return {{ ok: false, needInstall: true, items: [], countsByType: {{}} }}; const __p = {payload_json}; return a.apply(__p); }})()"
    )
}

/// 点击文字探针：轻量清 overlay / 缓存，不重注入 CLICK_TEXT_PROBE 整包。
pub fn compose_clear_click_text_probe_overlay() -> String {
    r#"(() => {
  const OVERLAY_ID = '__webclaw_click_text_probe_overlay__';
  const clearWin = (win) => {
    if (!win) return;
    try {
      if (win.__webclawClickTextProbeApi && typeof win.__webclawClickTextProbeApi.clear === 'function') {
        try { win.__webclawClickTextProbeApi.clear(); } catch (_) {}
      }
    } catch (_) {}
    try {
      win.document.querySelectorAll('[id="' + OVERLAY_ID + '"]').forEach((n) => {
        try { n.remove(); } catch (_) {}
      });
    } catch (_) {}
    try { win.__webclawClickTextProbeCache = null; } catch (_) {}
    try { delete win.__webclawClickTextProbeApi; } catch (_) {
      try { win.__webclawClickTextProbeApi = null; } catch (_) {}
    }
  };
  const walk = (win) => {
    clearWin(win);
    let frames;
    try { frames = win.document.querySelectorAll('iframe'); } catch (_) { return; }
    for (const f of frames) {
      try {
        const cw = f.contentWindow;
        if (cw) walk(cw);
      } catch (_) {}
    }
  };
  try { walk(window); } catch (_) { clearWin(window); }
  return { ok: true };
})()"#
    .to_string()
}

/// 专用于解释「点击文字 / 点击文字#N」：本 frame 排序见 CLICK_TEXT_RANKING；跨 frame 连续 #N 由 Rust 按得分合成
pub const CLICK_TEXT_PROBE: &str = r#"
const OVERLAY_ID = '__webclaw_click_text_probe_overlay__';

const describeEl = (el) => {
  if (!el || !(el instanceof Element)) return '';
  const tag = el.tagName || '?';
  const id = el.id ? '#' + el.id : '';
  let cls = '';
  try {
    const parts = (el.className || '').toString().trim().split(/\s+/).filter(Boolean).slice(0, 2);
    if (parts.length) cls = '.' + parts.join('.');
  } catch (_) {}
  const text = getVisibleText(el).trim().slice(0, 40);
  return tag + id + cls + (text ? ' 「' + text + '」' : '');
};

const __webclawClickTextProbeScan = (searchRaw) => {
  const pack = __webclawRankClickTextHits(searchRaw);
  if (!pack.ok) {
    return {
      ok: false,
      error: '请输入要解释的点击文字（与「点击xxx」相同）',
      items: [],
      countsByType: {},
      search: null,
      scanMode: null,
      logicHint: '输入文字后扫描；编号与「点击文字#N」跨 frame 连续且得分高优先',
    };
  }
  const { search, qNorm, matcher, scanMode, ranked } = pack;
  const rankedKeys = new Set(ranked.map(clickTextHitKey));
  const scrollX = window.scrollX || document.documentElement.scrollLeft || 0;
  const scrollY = window.scrollY || document.documentElement.scrollTop || 0;
  const items = [];
  ranked.forEach((h, i) => {
    const index = i + 1;
    const indexCmd = index === 1 ? ('点击' + search) : ('点击' + search + '#' + index);
    const pool = h.inViewport ? 'viewport' : 'offscreen';
    const softHint = h.clickable ? '' : '；软命中·本元素连续字·仅坐标点击';
    const note = (scanMode === 'viewport'
      ? (h.inViewport ? '计入序号（视口内）' : '计入序号（视口扫描命中）')
      : (h.inViewport ? '计入序号（视口外兜底池·本条在视口）' : '计入序号（视口外兜底：当前视口无强匹配）'))
      + softHint;
    const tr = h.target.getBoundingClientRect();
    items.push({
      index,
      type: '点击文字',
      desc: describeEl(h.target),
      clickCmd: indexCmd,
      inputCmd: null,
      textCmd: '点击' + search,
      indexCmd,
      textScore: h.textScore,
      score: h.score,
      clickable: !!h.clickable,
      pool,
      note,
      rect: { pageX: tr.left + scrollX, pageY: tr.top + scrollY, width: tr.width, height: tr.height },
    });
  });
  if (scanMode === 'viewport') {
    const allDisplayed = rankHitsForIndex(collectHits(true, qNorm, matcher));
    let excl = 0;
    for (const h of allDisplayed) {
      if (rankedKeys.has(clickTextHitKey(h))) continue;
      if (h.inViewport) continue;
      excl += 1;
      const tr = h.target.getBoundingClientRect();
      items.push({
        index: 0,
        type: '未计入',
        desc: describeEl(h.target),
        clickCmd: '',
        inputCmd: null,
        textCmd: null,
        indexCmd: null,
        textScore: h.textScore,
        score: h.score,
        clickable: !!h.clickable,
        pool: 'excluded',
        note: '未计入序号：视口已有强匹配，点击只扫视口（与页面探针文字不同）'
          + (h.clickable ? '' : '；本条为可见文本软命中'),
        rect: { pageX: tr.left + scrollX, pageY: tr.top + scrollY, width: tr.width, height: tr.height },
      });
      if (excl >= 40) break;
    }
  }
  const hardN = ranked.filter((h) => h.clickable).length;
  const softN = ranked.length - hardN;
  const logicHint = (scanMode === 'viewport'
    ? '本 frame 视口内已有 ≥0.92 匹配 → 只对视口候选编号'
    : '本 frame 视口无强匹配 → 扩大到已展示（含屏外）再编号')
    + '；文字匹配分（textScore）高优先，同分硬可点击优先'
    + (softN ? ('（本页硬' + hardN + '/软' + softN + '）') : '');
  const countsByType = { '点击文字': ranked.length, '未计入': items.length - ranked.length };
  return {
    ok: true,
    url: location.href,
    title: document.title,
    search,
    scanMode,
    logicHint,
    frameLabel: (() => {
      try { if (window.top === window) return null; } catch (_) { return 'iframe'; }
      return 'iframe';
    })(),
    frameUrl: location.href,
    items,
    countsByType,
  };
};

const __webclawClickTextProbePaintCtl = {
  scrollBound: false,
  paintRaf: 0,
  paintTimer: 0,
  lastPaintAt: 0,
  onScrollPaint: null,
  items: null,
  highlight: null,
  showExcluded: false,
};

const __webclawClickTextProbeRemoveOverlay = () => {
  try {
    document.querySelectorAll('[id="' + OVERLAY_ID + '"]').forEach((n) => {
      try { n.remove(); } catch (_) {}
    });
  } catch (_) {
    const old = document.getElementById(OVERLAY_ID);
    if (old) old.remove();
  }
};

const __webclawClickTextProbeEnsureCanvas = () => {
  let canvas = document.getElementById(OVERLAY_ID);
  if (canvas && canvas.tagName === 'CANVAS') return canvas;
  __webclawClickTextProbeRemoveOverlay();
  canvas = document.createElement('canvas');
  canvas.id = OVERLAY_ID;
  canvas.style.cssText =
    'position:fixed;left:0;top:0;width:100vw;height:100vh;pointer-events:none;z-index:2147483646;';
  document.documentElement.appendChild(canvas);
  return canvas;
};

const __webclawClickTextProbePaint = (canvas, items, highlight, showExcluded) => {
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  const cssW = Math.max(1, window.innerWidth || document.documentElement.clientWidth || 1);
  const cssH = Math.max(1, window.innerHeight || document.documentElement.clientHeight || 1);
  const bw = Math.floor(cssW * dpr);
  const bh = Math.floor(cssH * dpr);
  if (canvas.width !== bw || canvas.height !== bh) {
    canvas.width = bw;
    canvas.height = bh;
    canvas.style.width = cssW + 'px';
    canvas.style.height = cssH + 'px';
  }
  const ctx = canvas.getContext('2d');
  if (!ctx) return 0;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, cssW, cssH);
  const scrollX = window.scrollX || document.documentElement.scrollLeft || 0;
  const scrollY = window.scrollY || document.documentElement.scrollTop || 0;
  const pad = 4;
  let drawn = 0;
  ctx.textBaseline = 'middle';
  ctx.textAlign = 'center';
  ctx.font = 'bold 11px sans-serif';
  for (const item of items || []) {
    if (item.type === '未计入' && !showExcluded) continue;
    const color = item.type === '未计入' ? '#94a3b8' : '#e11d48';
    const isHi = highlight && highlight.type === item.type && highlight.index === item.index && item.index > 0;
    const r = item.rect || {};
    const x = (r.pageX || 0) - scrollX;
    const y = (r.pageY || 0) - scrollY;
    const w = Math.max(r.width || 0, 4);
    const h = Math.max(r.height || 0, 4);
    if (x + w < -pad || y + h < -pad || x > cssW + pad || y > cssH + pad) continue;
    ctx.strokeStyle = color;
    ctx.lineWidth = 2;
    if (item.type === '未计入') ctx.setLineDash([4, 3]);
    else ctx.setLineDash([]);
    ctx.fillStyle = isHi ? color + '44' : color + '22';
    ctx.fillRect(x, y, w, h);
    ctx.strokeRect(x, y, w, h);
    ctx.setLineDash([]);
    if (item.index > 0) {
      const label = String(item.index);
      const tw = Math.max(18, ctx.measureText(label).width + 8);
      const th = 18;
      const bx = x - 1;
      const by = y - 1;
      ctx.fillStyle = color;
      ctx.fillRect(bx, by, tw, th);
      ctx.fillStyle = '#fff';
      ctx.fillText(label, bx + tw / 2, by + th / 2);
    }
    drawn++;
  }
  __webclawClickTextProbePaintCtl.lastPaintAt = Date.now();
  return drawn;
};

const __webclawClickTextProbeBindScroll = () => {
  const ctl = __webclawClickTextProbePaintCtl;
  if (ctl.scrollBound) return;
  ctl.scrollBound = true;
  ctl.onScrollPaint = () => {
    if (ctl.paintRaf || ctl.paintTimer) return;
    const wait = Math.max(0, 48 - (Date.now() - (ctl.lastPaintAt || 0)));
    const schedule = (fn) => {
      if (wait <= 0) {
        ctl.paintRaf = requestAnimationFrame(() => {
          ctl.paintRaf = 0;
          fn();
        });
      } else {
        ctl.paintTimer = setTimeout(() => {
          ctl.paintTimer = 0;
          ctl.paintRaf = requestAnimationFrame(() => {
            ctl.paintRaf = 0;
            fn();
          });
        }, wait);
      }
    };
    schedule(() => {
      const el = document.getElementById(OVERLAY_ID);
      if (!el || el.tagName !== 'CANVAS') return;
      if (!ctl.items || !ctl.items.length) return;
      __webclawClickTextProbePaint(el, ctl.items, ctl.highlight, ctl.showExcluded);
    });
  };
  window.addEventListener('scroll', ctl.onScrollPaint, { capture: true, passive: true });
  window.addEventListener('resize', ctl.onScrollPaint, { passive: true });
};

const __webclawClickTextProbeUnbindScroll = () => {
  const ctl = __webclawClickTextProbePaintCtl;
  if (!ctl.scrollBound) return;
  if (ctl.onScrollPaint) {
    try { window.removeEventListener('scroll', ctl.onScrollPaint, true); } catch (_) {}
    try { window.removeEventListener('resize', ctl.onScrollPaint); } catch (_) {}
  }
  if (ctl.paintRaf) {
    try { cancelAnimationFrame(ctl.paintRaf); } catch (_) {}
    ctl.paintRaf = 0;
  }
  if (ctl.paintTimer) {
    try { clearTimeout(ctl.paintTimer); } catch (_) {}
    ctl.paintTimer = 0;
  }
  ctl.scrollBound = false;
  ctl.onScrollPaint = null;
};

const __webclawClickTextProbeClear = () => {
  __webclawClickTextProbeUnbindScroll();
  try {
    document.querySelectorAll('[id="' + OVERLAY_ID + '"]').forEach((n) => {
      try { n.remove(); } catch (_) {}
    });
  } catch (_) {
    const old = document.getElementById(OVERLAY_ID);
    if (old) old.remove();
  }
  try { window.__webclawClickTextProbeCache = null; } catch (_) {}
  __webclawClickTextProbePaintCtl.items = null;
  return { ok: true };
};

const __webclawClickTextProbeDraw = (items, highlight, showExcluded) => {
  const canvas = __webclawClickTextProbeEnsureCanvas();
  __webclawClickTextProbePaintCtl.items = items || [];
  __webclawClickTextProbePaintCtl.highlight = highlight || null;
  __webclawClickTextProbePaintCtl.showExcluded = !!showExcluded;
  __webclawClickTextProbeBindScroll();
  return __webclawClickTextProbePaint(canvas, items, highlight, showExcluded);
};

const __webclawClickTextProbeApply = (payload) => {
  const search = (payload && payload.search) || '';
  const highlight = payload && payload.highlight;
  const showExcluded = !!(payload && payload.showExcluded);
  const reuseItems = !!(payload && payload.reuseItems);
  const indexMap = payload && payload.indexMap;
  const cache = window.__webclawClickTextProbeCache;
  const cacheOk = reuseItems
    && cache
    && cache.search === search
    && !!cache.showExcluded === showExcluded
    && Array.isArray(cache.items)
    && cache.items.length > 0;
  let scan;
  if (cacheOk) {
    scan = {
      ok: true,
      reused: true,
      search: cache.search,
      items: cache.items,
      countsByType: cache.countsByType || {},
      scanMode: cache.scanMode,
      logicHint: cache.logicHint,
    };
  } else {
    scan = __webclawClickTextProbeScan(search);
    if (!scan.ok) {
      __webclawClickTextProbeClear();
      return scan;
    }
    window.__webclawClickTextProbeCache = {
      search,
      showExcluded,
      items: scan.items || [],
      countsByType: scan.countsByType || {},
      scanMode: scan.scanMode,
      logicHint: scan.logicHint,
    };
  }
  // 宿主传入本地→全局 #N（跨 iframe 连续、得分优先）
  if (indexMap && typeof indexMap === 'object' && Array.isArray(scan.items)) {
    const mapped = [];
    for (const item of scan.items) {
      if (item.type !== '点击文字' || !(item.index > 0)) {
        mapped.push(item);
        continue;
      }
      const gRaw = indexMap[String(item.index)] ?? indexMap[item.index];
      const g = Number(gRaw);
      if (!Number.isFinite(g) || g < 1) continue;
      const indexCmd = g === 1 ? ('点击' + search) : ('点击' + search + '#' + g);
      mapped.push(Object.assign({}, item, {
        index: g,
        clickCmd: indexCmd,
        indexCmd,
        textCmd: '点击' + search,
      }));
    }
    scan = Object.assign({}, scan, { items: mapped });
  }
  scan.drawn = __webclawClickTextProbeDraw(scan.items, highlight, showExcluded);
  return scan;
};
"#;

#[cfg(test)]
mod tests {
    use super::is_structural_target;

    #[test]
    fn structural_target_dsl_positive() {
        assert!(is_structural_target("按钮#2"));
        assert!(is_structural_target("按钮#2，"));
        assert!(is_structural_target("图标按钮#1"));
        assert!(is_structural_target("表格#1行#1图标按钮#2"));
        assert!(is_structural_target("表格#4行#1列#4"));
        assert!(is_structural_target("表格#1行#1列#4输入框#1"));
        assert!(is_structural_target("输入框#3"));
    }

    #[test]
    fn structural_target_dsl_negative() {
        assert!(!is_structural_target(""));
        assert!(!is_structural_target("登录"));
        assert!(!is_structural_target("选项#1"));
        assert!(!is_structural_target("第2个按钮"));
        assert!(!is_structural_target("表格#1中的行#1"));
        assert!(!is_structural_target("点击按钮#2"));
        assert!(!is_structural_target("等待按钮#2"));
    }

    #[test]
    fn parse_click_text_args_basic() {
        assert_eq!(
            super::parse_click_text_args("作业过程监管"),
            Some(("作业过程监管".into(), 1))
        );
        assert_eq!(
            super::parse_click_text_args("作业过程监管#2"),
            Some(("作业过程监管".into(), 2))
        );
        assert_eq!(
            super::parse_click_text_args("确定，"),
            Some(("确定".into(), 1))
        );
        assert_eq!(
            super::parse_click_text_args("确定#2，"),
            Some(("确定".into(), 2))
        );
        assert_eq!(super::parse_click_text_args("按钮#2"), None);
        assert_eq!(super::parse_click_text_args("按钮#2，"), None);
        assert_eq!(super::parse_click_text_args("图标:close"), None);
        assert_eq!(super::parse_click_text_args("图标：search#2"), None);
        assert!(super::compose_click_text_catalog("HCM").contains("__webclawRankClickTextHits"));
    }

    #[test]
    fn compose_script_includes_resolver() {
        let s = super::compose_script("return { ok: true };");
        assert!(s.contains("structural-target-v19"));
        assert!(s.contains("stripTrailingStepSeps"));
        assert!(!s.contains("click-text-ranking-v1"));
        assert!(s.contains("parseStructuralTarget"));
        assert!(s.contains("不含视口裁剪") || s.contains("scrollY"));
        assert!(s.contains("sortByDocumentOrder") || s.contains("orderStructuralCandidates"));
        assert!(s.contains("data-webclaw-probe") || s.contains("PROBE_STAMP"));
    }

    #[test]
    fn compose_click_script_includes_ranking() {
        let s = super::compose_click_script("return { ok: true };");
        assert!(s.contains("structural-target-v19"));
        assert!(s.contains("stripTrailingStepSeps"));
        assert!(s.contains("click-text-ranking-v6"));
        assert!(s.contains("isOptionLikeElement"));
        assert!(s.contains("__webclawRankClickTextHits"));
        assert!(s.contains("rankHitsForIndex"));
        assert!(s.contains("coordOnly"));
        assert!(s.contains("findSoftClickTarget"));
        assert!(s.contains("getOwnContinuousText"));
    }

    #[test]
    fn compose_clear_probe_overlay_is_lightweight() {
        let s = super::compose_clear_probe_overlay();
        assert!(s.contains("__webclaw_probe_overlay__"));
        assert!(s.contains("data-webclaw-probe"));
        assert!(s.contains("contentWindow") || s.contains("iframe"));
        assert!(!s.contains("__webclawInstallProbe"));
        assert!(!s.contains("structural-target-v19"));
        assert!(!s.contains("PROBE_VERSION"));
        let c = super::compose_clear_click_text_probe_overlay();
        assert!(c.contains("__webclaw_click_text_probe_overlay__"));
        assert!(c.contains("contentWindow") || c.contains("iframe"));
        assert!(!c.contains("__webclawRankClickTextHits"));
    }

    #[test]
    fn compose_probe_page_is_stub_and_ensure_has_module() {
        let s = super::compose_probe_page(Some("表格#1行#1"), Some("流程"), None);
        assert!(s.contains("needInstall"));
        assert!(s.contains("__webclawProbe"));
        assert!(s.contains(".scan("));
        assert!(s.contains("表格#1行#1"));
        assert!(s.contains("流程"));
        assert!(!s.contains("structural-target-v19"));
        assert!(!s.contains("__webclawInstallProbe"));
        let ens = super::compose_ensure_probe_installed();
        assert!(ens.contains("structural-target-v19"));
        assert!(ens.contains("__webclawInstallProbe"));
        assert!(ens.contains("PROBE_VERSION = 28") || ens.contains("const PROBE_VERSION = 28"));
        assert!(ens.contains("ensureProbeCanvas") || ens.contains("paintProbeItems"));
        assert!(ens.contains("createElement('canvas')") || ens.contains("createElement(\"canvas\")"));
        assert!(ens.contains("reuseItems"));
        let ov = super::compose_probe_overlay(&serde_json::json!({ "reuseItems": true }));
        assert!(ov.contains("needInstall"));
        assert!(ov.contains("reuseItems"));
        assert!(!ov.contains("structural-target-v19"));
    }

    #[test]
    fn compose_probe_page_filtered_limits_types() {
        let s = super::compose_probe_page_filtered(
            None,
            None,
            None,
            Some(&["图标按钮", "按钮", "输入框"]),
        );
        assert!(s.contains("\"图标按钮\""));
        assert!(s.contains("\"按钮\""));
        assert!(s.contains("\"输入框\""));
        assert!(s.contains("needInstall"));
        let ens = super::compose_ensure_probe_installed();
        assert!(ens.contains("structuralClassName") || ens.contains("cls:"));
    }

    #[test]
    fn parse_single_structural_segment_ok() {
        assert_eq!(
            super::parse_single_structural_segment("图标按钮#15"),
            Some(("图标按钮".into(), 15))
        );
        assert_eq!(super::parse_single_structural_segment("表格#1行#1"), None);
    }

    #[test]
    fn input_script_locked_hint_matches_shared_tail() {
        assert!(super::INPUT_FIELD.contains(super::LOCKED_INPUT_HINT_TAIL));
        assert!(super::INPUT_FIELD.contains("input-target-v14"));
        assert!(super::INPUT_FIELD.contains("isCancelRequested"));
        // 只读/禁用：结构化路径按可见性定位后再判定；class 含 disabled 不挡输入
        assert!(super::INPUT_FIELD.contains("isLockedInput"));
        assert!(super::INPUT_FIELD.contains("structuralIsTrulyDisabled"));
        assert!(super::STRUCTURAL_TARGET_RESOLVER.contains("structuralIsTrulyDisabled"));
        // 扇出短探读 ctx.elementWaitTimeout；未注入仍用完整等待
        assert!(super::INPUT_FIELD.contains("ctx.orProbe"));
        assert!(super::INPUT_FIELD.contains("DEFAULT_WAIT_TIMEOUT"));
        assert!(super::INPUT_FIELD.contains("if (n > 0) elementWaitTimeout = n"));
        assert!(!super::INPUT_FIELD.contains("FRAME_FANOUT_WAIT_TIMEOUT"));
        assert!(!super::CLICK_DOM.contains("FRAME_FANOUT_WAIT_TIMEOUT"));
        assert!(super::CLICK_DOM.contains("click-target-v6.16"));
        assert!(super::CLICK_DOM.contains("stripTrailingStepSeps"));
        assert!(super::CLICK_DOM.contains("toTopViewportCoords"));
        assert!(super::CLICK_DOM.contains("topX"));
        assert!(super::CLICK_DOM.contains("isCancelRequested"));
        assert!(super::CLICK_DOM.contains("parseIconClickArgs"));
        assert!(super::CLICK_DOM.contains("findIconButtonHits"));
        assert!(super::STRUCTURAL_TARGET_RESOLVER.contains("formatIconClickCmd"));
        assert!(super::STRUCTURAL_TARGET_RESOLVER.contains("iconNamesEqual"));
        assert!(super::STRUCTURAL_TARGET_RESOLVER.contains("extractBareIconToken"));
        assert!(super::STRUCTURAL_TARGET_RESOLVER.contains("extractClassFallbackHint"));
        assert!(!super::STRUCTURAL_TARGET_RESOLVER.contains("extractLastClassFallback"));
        assert!(super::STRUCTURAL_TARGET_RESOLVER.contains("isLibraryShellClass"));
        assert!(super::STRUCTURAL_TARGET_RESOLVER.contains("buildIconProbeTextCmdsForEls"));
        assert!(super::STRUCTURAL_TARGET_RESOLVER.contains("data-icon"));
        assert!(super::STRUCTURAL_TARGET_RESOLVER.contains("只比最终抽取名"));
        assert!(!super::STRUCTURAL_TARGET_RESOLVER.contains("hintNorm.includes(want)"));
        assert!(super::probe_module_js().contains("overlayOnly"));
        assert!(super::probe_module_js().contains("buildIconProbeTextCmdsForEls"));
        assert!(super::probe_module_js().contains("reuseItems"));
        assert!(super::CLICK_TEXT_PROBE.contains("__webclawClickTextProbeCache") || super::compose_click_text_probe_overlay(&serde_json::json!({})).contains("reuseItems"));
        assert!(super::CLICK_DOM.contains("if (n > 0) elementWaitTimeout = n"));
        assert!(super::CLICK_DOM.contains("__webclawRankClickTextHits"));
        assert!(super::CLICK_TEXT_RANKING.contains("reorderByOcclusion"));
        assert!(super::CLICK_TEXT_RANKING.contains("rankHitsForIndex"));
        assert!(super::CLICK_TEXT_RANKING.contains("coordOnly"));
        assert!(super::CLICK_TEXT_RANKING.contains("findSoftClickTarget"));
        assert!(super::CLICK_TEXT_RANKING.contains("getOwnContinuousText"));
        assert!(super::CLICK_DOM.contains("getClientRects"));
        assert!(super::CLICK_DOM.contains("coordOnly"));
        assert!(!super::CLICK_DOM.contains("isBloatedContainsHit"));
        assert!(super::CLICK_DOM.contains("ctx.orProbe"));
    }

    #[test]
    fn compose_click_text_probe_mirrors_click_rules() {
        let s = super::compose_click_text_probe_page("HCM");
        assert!(s.contains("needInstall"));
        assert!(s.contains("__webclawClickTextProbeApi"));
        assert!(s.contains("HCM"));
        assert!(!s.contains("click-text-ranking-v6"));
        let ens = super::compose_ensure_click_text_probe_installed();
        assert!(ens.contains("__webclawClickTextProbeScan"));
        assert!(ens.contains("click-text-ranking-v6"));
        assert!(ens.contains("isOptionLikeElement"));
        assert!(ens.contains("isComboDropdownPanel"));
        assert!(ens.contains("__webclawRankClickTextHits"));
        assert!(ens.contains("MIN_TEXT_SCORE"));
        assert!(ens.contains("scanMode"));
        assert!(ens.contains("未计入"));
        assert!(ens.contains("reorderByOcclusion"));
        assert!(ens.contains("rankHitsForIndex"));
        assert!(ens.contains("coordOnly"));
        assert!(ens.contains("findSoftClickTarget"));
        assert!(ens.contains("getOwnContinuousText"));
        assert!(ens.contains("__webclaw_click_text_probe_overlay__"));
        assert!(!ens.contains("scanTextMatches")); // 不走页面探针文字路径
    }

    #[test]
    fn parse_input_field_equals() {
        let (text, field) = super::parse_input_step_args("密码=abc123");
        assert_eq!(field, "密码");
        assert_eq!(text, "abc123");
        let (text, field) = super::parse_input_step_args("月份=2026-05");
        assert_eq!(field, "月份");
        assert_eq!(text, "2026-05");
        let (text, field) = super::parse_input_step_args("输入框#2=关键词");
        assert_eq!(field, "输入框#2");
        assert_eq!(text, "关键词");
    }

    #[test]
    fn parse_input_legacy_dao() {
        let (text, field) = super::parse_input_step_args("hello到手机");
        assert_eq!(text, "hello");
        assert_eq!(field, "手机");
    }

    #[test]
    fn parse_input_plain_text_only() {
        let (text, field) = super::parse_input_step_args("2026-05");
        assert_eq!(text, "2026-05");
        assert_eq!(field, "");
    }

    #[test]
    fn format_input_script_args_pipe() {
        assert_eq!(
            super::format_input_script_args("密码=abc"),
            "abc|密码"
        );
    }

    #[test]
    fn parse_pixel_click_args_empty_defaults_origin() {
        assert_eq!(super::parse_pixel_click_args(""), Some((0.0, 0.0)));
        assert_eq!(super::parse_pixel_click_args("   "), Some((0.0, 0.0)));
    }

    #[test]
    fn parse_pixel_click_args_xy() {
        assert_eq!(super::parse_pixel_click_args("120 80"), Some((120.0, 80.0)));
        assert_eq!(super::parse_pixel_click_args("  10  20  "), Some((10.0, 20.0)));
        assert_eq!(super::parse_pixel_click_args("-5 0"), Some((-5.0, 0.0)));
    }

    #[test]
    fn parse_pixel_click_args_rejects_non_coordinate() {
        assert!(super::parse_pixel_click_args("登录").is_none());
        assert!(super::parse_pixel_click_args("100").is_none());
        assert!(super::parse_pixel_click_args("图标:close-circle").is_none());
        assert!(super::parse_pixel_click_args("按钮#2").is_none());
    }
}
