//! 「采集」指令：把当前页可见文字按类别整理成 Markdown，写入 documents/ 供预览。
//! 当前页为飞书多维表格时，表格数据由开放平台写入「表格」分类（与 DOM 扫描同报告结构）。

use crate::cdp::CdpClient;
use chrono::Local;
use serde::Deserialize;
use serde_json::Value;

/// 单字段软上限（防极端脏页撑爆 CDP）；正常页面几乎触不到
const MAX_TEXT_LEN: usize = 500;
const MAX_TABLES: usize = 40;
const MAX_TABLE_ROWS: usize = 300;
/// 宽表（冻结列 + 横向滚动）常见 60~100+ 列
const MAX_TABLE_COLS: usize = 120;

/// 「采集」分类：无参数 = 全部；`采集表格` 等只出对应章节
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectScope {
    pub headings: bool,
    pub buttons: bool,
    pub links: bool,
    pub inputs: bool,
    pub tables: bool,
    pub others: bool,
}

impl CollectScope {
    pub fn all() -> Self {
        Self {
            headings: true,
            buttons: true,
            links: true,
            inputs: true,
            tables: true,
            others: true,
        }
    }

    pub fn is_all(self) -> bool {
        self == Self::all()
    }

    pub fn wants_tables(self) -> bool {
        self.tables
    }

    /// 仅表格分类（飞书多维表等场景）
    pub fn tables_only() -> Self {
        Self {
            headings: false,
            buttons: false,
            links: false,
            inputs: false,
            tables: true,
            others: false,
        }
    }

    /// 文件名 / 概览用短标签；全部时为空
    pub fn file_slug(self) -> Option<&'static str> {
        if self.is_all() {
            return None;
        }
        let mut names = Vec::new();
        if self.tables {
            names.push("表格");
        }
        if self.headings {
            names.push("标题");
        }
        if self.buttons {
            names.push("按钮");
        }
        if self.links {
            names.push("链接");
        }
        if self.inputs {
            names.push("输入框");
        }
        if self.others {
            names.push("其它文字");
        }
        match names.as_slice() {
            ["表格"] => Some("表格"),
            ["标题"] => Some("标题"),
            ["按钮"] => Some("按钮"),
            ["链接"] => Some("链接"),
            ["输入框"] => Some("输入框"),
            ["其它文字"] => Some("其它文字"),
            _ => Some("分类"),
        }
    }

    pub fn display_label(self) -> String {
        if self.is_all() {
            return "全部".into();
        }
        let mut names = Vec::new();
        if self.tables {
            names.push("表格");
        }
        if self.headings {
            names.push("标题");
        }
        if self.buttons {
            names.push("按钮");
        }
        if self.links {
            names.push("链接");
        }
        if self.inputs {
            names.push("输入框");
        }
        if self.others {
            names.push("其它文字");
        }
        names.join("、")
    }
}

/// 解析「采集」参数。空 = 全部；支持：表格、标题、按钮、链接、输入框、其它文字（可并列：采集表格、按钮）
pub fn parse_collect_args(args: &str) -> Result<CollectScope, String> {
    let raw = args.trim();
    if raw.is_empty() {
        return Ok(CollectScope::all());
    }
    let mut scope = CollectScope {
        headings: false,
        buttons: false,
        links: false,
        inputs: false,
        tables: false,
        others: false,
    };
    let mut any = false;
    for part in raw.split(|c: char| {
        c.is_whitespace() || matches!(c, ',' | '，' | '、' | '/' | '+' | '|')
    }) {
        let t = part.trim().trim_start_matches('=').trim();
        if t.is_empty() {
            continue;
        }
        any = true;
        match normalize_collect_kind(t) {
            Some("all") => return Ok(CollectScope::all()),
            Some("tables") => scope.tables = true,
            Some("headings") => scope.headings = true,
            Some("buttons") => scope.buttons = true,
            Some("links") => scope.links = true,
            Some("inputs") => scope.inputs = true,
            Some("others") => scope.others = true,
            _ => {
                return Err(format!(
                    "「采集」分类无法识别：「{t}」。可用：表格、标题、按钮、链接、输入框、其它文字。无参数则采集全部。"
                ));
            }
        }
    }
    if !any {
        return Ok(CollectScope::all());
    }
    Ok(scope)
}

fn normalize_collect_kind(t: &str) -> Option<&'static str> {
    let s = t.trim().to_ascii_lowercase();
    match s.as_str() {
        "全部" | "所有" | "all" => Some("all"),
        "表格" | "表" | "table" | "tables" => Some("tables"),
        "标题" | "heading" | "headings" => Some("headings"),
        "按钮" | "button" | "buttons" => Some("buttons"),
        "链接" | "可点文字" | "link" | "links" => Some("links"),
        "输入框" | "输入" | "input" | "inputs" => Some("inputs"),
        "其它" | "其它文字" | "其他" | "其他文字" | "other" | "others" => Some("others"),
        _ => None,
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CollectItem {
    #[serde(default)]
    text: String,
    #[serde(default)]
    hint: String,
    #[serde(default)]
    cmd: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CollectTableRaw {
    #[serde(default)]
    caption: String,
    #[serde(default)]
    headers: Vec<String>,
    #[serde(default)]
    rows: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FrameCollectRaw {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    frame_label: Option<String>,
    #[serde(default)]
    headings: Vec<CollectItem>,
    #[serde(default)]
    buttons: Vec<CollectItem>,
    #[serde(default)]
    clickables: Vec<CollectItem>,
    #[serde(default)]
    inputs: Vec<CollectItem>,
    #[serde(default)]
    others: Vec<CollectItem>,
    #[serde(default)]
    tables: Vec<CollectTableRaw>,
}

#[derive(Debug, Clone, Default)]
struct CollectBucket {
    items: Vec<CollectItem>,
    seen: std::collections::HashSet<String>,
}

impl CollectBucket {
    fn push(&mut self, mut item: CollectItem, frame: Option<&str>) {
        let text = norm_text(&item.text);
        if text.is_empty() {
            return;
        }
        item.text = truncate(&text, MAX_TEXT_LEN);
        if !item.hint.is_empty() {
            item.hint = truncate(&norm_text(&item.hint), MAX_TEXT_LEN);
        }
        if let Some(f) = frame.filter(|s| !s.is_empty()) {
            if !item.hint.is_empty() {
                item.hint = format!("{} · {}", item.hint, f);
            } else {
                item.hint = f.to_string();
            }
        }
        let key = format!(
            "{}|{}|{}",
            item.text.to_lowercase(),
            item.cmd.to_lowercase(),
            item.hint.to_lowercase()
        );
        if !self.seen.insert(key) {
            return;
        }
        self.items.push(item);
    }
}

fn norm_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn normalize_table(mut t: CollectTableRaw, frame: Option<&str>) -> Option<CollectTable> {
    t.caption = truncate(&norm_text(&t.caption), MAX_TEXT_LEN);
    t.headers = t
        .headers
        .into_iter()
        .map(|c| truncate(&norm_text(&c), MAX_TEXT_LEN))
        .take(MAX_TABLE_COLS)
        .collect();
    t.rows = t
        .rows
        .into_iter()
        .take(MAX_TABLE_ROWS)
        .map(|row| {
            row.into_iter()
                .map(|c| truncate(&norm_text(&c), MAX_TEXT_LEN))
                .take(MAX_TABLE_COLS)
                .collect::<Vec<_>>()
        })
        .filter(|row| row.iter().any(|c| !c.is_empty()))
        .collect();
    if t.headers.iter().all(|c| c.is_empty()) && t.rows.is_empty() {
        return None;
    }
    let mut hint = String::new();
    if let Some(f) = frame.filter(|s| !s.is_empty()) {
        hint = f.to_string();
    }
    Some(CollectTable {
        caption: t.caption,
        headers: t.headers,
        rows: t.rows,
        hint,
    })
}

/// 页内采集脚本（不画 overlay、不打探针标）
pub fn compose_page_collect_js() -> String {
    compose_page_collect_js_with(true)
}

fn compose_page_collect_js_with(want_tables: bool) -> String {
    let body = PAGE_COLLECT_JS;
    let flag = if want_tables { "true" } else { "false" };
    // 横向扫列需要 await；CDP evaluate 已开 awaitPromise
    format!("(async () => {{ const WANT_TABLES = {flag}; {body} }})()")
}

const PAGE_COLLECT_JS: &str = r#"
const MAX_LEN = 500;
const MAX_TABLES = 40;
const MAX_ROWS = 300;
const MAX_COLS = 120;
const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
const clip = (s) => {
  const t = norm(s);
  if (t.length <= MAX_LEN) return t;
  return t.slice(0, MAX_LEN - 1) + '…';
};
const isVisible = (el) => {
  if (!el || !(el instanceof Element)) return false;
  const s = window.getComputedStyle(el);
  if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false;
  const r = el.getBoundingClientRect();
  return r.width >= 2 && r.height >= 2;
};
const isPresent = (el) => {
  // 结构仍在 DOM 中（含宽为 0 的冻结/操作列面板）
  if (!el || !(el instanceof Element)) return false;
  const s = window.getComputedStyle(el);
  if (s.display === 'none' || s.visibility === 'hidden') return false;
  return true;
};
const isVisibleBox = (el) => isVisible(el);
const textOf = (el) => {
  try { return clip(el.innerText || el.textContent || ''); } catch (_) { return ''; }
};
const cellText = (el) => {
  try {
    // 通用：优先 title/label/content 子节点（含 ant-table-column-title），避免图标/复选框噪音
    const prefer = el.querySelector && el.querySelector('.ant-table-column-title, .head-label, .cell-content, .cell-text, .cell-title, [class*="column-title"], [class*="cell-content"], [class*="head-label"]');
    if (prefer) {
      const pt = clip(prefer.innerText || prefer.textContent || '');
      if (pt) return pt;
    }
    let t = '';
    for (const n of el.childNodes || []) {
      if (n.nodeType === 3) t += n.textContent || '';
    }
    t = clip(t);
    if (t) return t;
    // 跳过纯图标 span
    const spans = el.querySelectorAll ? el.querySelectorAll(':scope > span, :scope > div') : [];
    for (const s of spans) {
      const st = clip(s.innerText || s.textContent || '');
      if (st && st.length <= MAX_LEN) return st;
    }
    return clip(el.innerText || el.textContent || '');
  } catch (_) { return ''; }
};
const ariaOf = (el) => clip((el.getAttribute && (el.getAttribute('aria-label') || el.getAttribute('title'))) || '');
const pushUnique = (arr, seen, item) => {
  if (!item || !item.text) return;
  const key = (item.text + '|' + (item.cmd || '') + '|' + (item.hint || '')).toLowerCase();
  if (seen.has(key)) return;
  seen.add(key);
  arr.push(item);
};
const headings = [];
const buttons = [];
const clickables = [];
const inputs = [];
const others = [];
const tables = [];
const seenH = new Set();
const seenB = new Set();
const seenC = new Set();
const seenI = new Set();
const seenO = new Set();
const claimed = new Set();
const claimedTables = new Set();

const markClaimed = (el) => { try { claimed.add(el); } catch (_) {} };
const markClaimedDeep = (el) => {
  try {
    claimed.add(el);
    for (const n of el.querySelectorAll('*')) claimed.add(n);
  } catch (_) {}
};
const isClaimed = (el) => {
  try {
    if (claimed.has(el)) return true;
    for (const c of claimed) {
      if (c.contains && c.contains(el)) return true;
      if (el.contains && el.contains(c)) return true;
    }
  } catch (_) {}
  return false;
};
const isInsideClaimedTable = (el) => {
  try {
    for (const t of claimedTables) {
      if (t.contains && t.contains(el)) return true;
    }
  } catch (_) {}
  return false;
};

const displayOf = (el) => {
  try { return window.getComputedStyle(el).display || ''; } catch (_) { return ''; }
};
const roleOf = (el) => ((el.getAttribute && el.getAttribute('role')) || '').toLowerCase();
const classOf = (el) => {
  try {
    if (!el || !el.className) return '';
    if (typeof el.className === 'string') return el.className.toLowerCase();
    return String(el.className.baseVal || el.className || '').toLowerCase();
  } catch (_) { return ''; }
};
const attr = (el, name) => {
  try { return el.getAttribute ? (el.getAttribute(name) || '') : ''; } catch (_) { return ''; }
};
const hasAnyAttr = (el, names) => {
  for (const n of names) if (attr(el, n) !== '') return true;
  return false;
};
const rowKeyOf = (el) => {
  for (const n of ['row-index', 'rowindex', 'data-row-index', 'data-row-key', 'data-rowid', 'aria-rowindex']) {
    const v = attr(el, n);
    if (v !== '') return v;
  }
  return '';
};
const colKeyOf = (el) => {
  for (const n of ['col-key', 'col-index', 'colindex', 'data-col-key', 'data-column-key', 'data-field', 'aria-colindex']) {
    const v = attr(el, n);
    if (v !== '') return v;
  }
  return '';
};
// 绝对定位网格：行用 top，单元格用 left+width（多套 UI 的冻结列表格通病）
const styleHint = (el) => ((el.getAttribute && el.getAttribute('style')) || '').toLowerCase();
const looksAbsRow = (el) => {
  const s = styleHint(el);
  if (/(^|;)\s*top\s*:/.test(s) && /(^|;)\s*height\s*:/.test(s)) return true;
  try {
    const cs = window.getComputedStyle(el);
    if (cs.position === 'absolute' && cs.top !== 'auto' && parseFloat(cs.height) > 0) return true;
  } catch (_) {}
  return false;
};
const looksAbsCell = (el) => {
  const s = styleHint(el);
  if (/(^|;)\s*left\s*:/.test(s) && /(^|;)\s*width\s*:/.test(s)) return true;
  try {
    const cs = window.getComputedStyle(el);
    if ((cs.position === 'absolute' || cs.position === 'relative') && cs.left !== 'auto' && parseFloat(cs.width) > 0) return true;
  } catch (_) {}
  return false;
};

// 通用 class token：精确匹配 + 前缀库名（ant-table-header / el-table__body-wrapper 等）
const classTokens = (el) => classOf(el).split(/[\s]+/).filter(Boolean);
const hasToken = (el, re) => classTokens(el).some((t) => re.test(t));
const RE_HEADER_WRAP_TOK = /^(table[-_]?header|thead|header[-_]?wrapper|grid[-_]?header)$/;
const RE_HEADER_CELL_TOK = /^(table[-_]?header[-_]?cell|header[-_]?cell|column[-_]?header|grid[-_]?header[-_]?cell)$/;
const RE_BODY_WRAP_TOK = /^(table[-_]?body|tbody|body[-_]?wrapper|grid[-_]?body)$/;
const RE_ROW_TOK = /^(table[-_]?row|data[-_]?row|grid[-_]?row|list[-_]?row|mui[-_]?table[-_]?row)$/;
const RE_CELL_TOK = /^(table[-_]?cell|table[-_]?header[-_]?cell|header[-_]?cell|grid[-_]?cell|data[-_]?cell|td[-_]?content|operate[-_]?cell|mui[-_]?table[-_]?cell)$/;
const RE_TABLE_ROOT_TOK = /^(el-table|ant-table|ant-table-wrapper|mui-table|rt-table|data-grid|list-grid|hc-list-grid|hc-list-grid-new|virtual-scroll|virtual-scroll-container|ag-root|ag-body|vxe-table|ivu-table)$/;
// 以 table-header 结尾，避免误伤 table-header-cell / table-header-column
const endsWithTablePart = (t, part) => new RegExp('(^|[-_])table[-_]?' + part + '$').test(t);

const isHeaderWrap = (el) => {
  if (!(el instanceof Element)) return false;
  if (el.tagName === 'THEAD') return true;
  if (roleOf(el) === 'rowgroup' && classOf(el).includes('header')) return true;
  for (const t of classTokens(el)) {
    if (RE_HEADER_WRAP_TOK.test(t)) return true;
    if (endsWithTablePart(t, 'header') || endsWithTablePart(t, 'thead')) return true;
    if (/header[-_]?wrapper$/.test(t)) return true;
    if (/^el-table__header/.test(t)) return true;
  }
  return false;
};
const isHeaderCell = (el) => {
  if (hasToken(el, RE_HEADER_CELL_TOK)) return true;
  if (el.tagName === 'TH' || roleOf(el) === 'columnheader') return true;
  for (const t of classTokens(el)) {
    if (endsWithTablePart(t, 'header-cell')) return true;
  }
  return false;
};
const isBodyWrap = (el) => {
  if (!(el instanceof Element)) return false;
  if (el.tagName === 'TBODY') return true;
  for (const t of classTokens(el)) {
    if (RE_BODY_WRAP_TOK.test(t)) return true;
    if (endsWithTablePart(t, 'body') || endsWithTablePart(t, 'tbody')) return true;
    if (/body[-_]?wrapper$/.test(t)) return true;
    if (/^el-table__body/.test(t)) return true;
  }
  return false;
};
const isRowLike = (el) => {
  if (!(el instanceof Element)) return false;
  const role = roleOf(el);
  if (role === 'row') return true;
  const d = displayOf(el);
  if (d === 'table-row') return true;
  if (el.tagName === 'TR') return true;
  if (hasToken(el, RE_ROW_TOK)) return true;
  for (const t of classTokens(el)) {
    if (endsWithTablePart(t, 'row') && !/header/.test(t)) return true;
  }
  if (hasAnyAttr(el, ['row-index', 'rowindex', 'data-row-index', 'data-row-key', 'aria-rowindex'])) {
    let cellish = 0;
    for (const c of el.children || []) if (isCellLike(c) || looksAbsCell(c)) cellish++;
    if (cellish >= 1) return true;
  }
  if (looksAbsRow(el)) {
    let cellish = 0;
    for (const c of el.children || []) if (isCellLike(c) || looksAbsCell(c)) cellish++;
    if (cellish >= 2) return true;
  }
  return false;
};
const isCellLike = (el) => {
  if (!(el instanceof Element)) return false;
  const role = roleOf(el);
  if (role === 'cell' || role === 'gridcell' || role === 'columnheader' || role === 'rowheader') return true;
  const d = displayOf(el);
  if (d === 'table-cell') return true;
  if (el.tagName === 'TD' || el.tagName === 'TH') return true;
  if (hasToken(el, RE_CELL_TOK)) return true;
  for (const t of classTokens(el)) {
    if (endsWithTablePart(t, 'cell') || endsWithTablePart(t, 'header-cell')) return true;
  }
  if (hasAnyAttr(el, ['col-key', 'col-index', 'colindex', 'data-col-key', 'data-field', 'aria-colindex']) && looksAbsCell(el)) return true;
  return false;
};

// 直接子级里有多少「header+body」面板（冻结列分栏表的关键结构）
const listDirectPanels = (el) => {
  const panels = [];
  if (!el) return panels;
  for (const ch of el.children || []) {
    let h = false, b = false;
    for (const x of ch.children || []) {
      if (isHeaderWrap(x)) h = true;
      if (isBodyWrap(x)) b = true;
    }
    if (h && b) panels.push(ch);
  }
  return panels;
};
const canMergePanels = (el) => listDirectPanels(el).length >= 2;
// 在根及其后代中找最合适的分栏合并宿主（避免外层 grid/scroll 壳只采到第一块冻结列）
const findMergeHost = (root) => {
  if (canMergePanels(root)) return root;
  let best = null;
  let bestN = 1;
  const walk = (el, depth) => {
    if (!el || depth > 8) return;
    const n = listDirectPanels(el).length;
    if (n > bestN) {
      best = el;
      bestN = n;
    }
    for (const c of el.children || []) walk(c, depth + 1);
  };
  walk(root, 0);
  return best;
};

const isNativeTableRoot = (el) => {
  if (!el) return false;
  if (el.tagName === 'TABLE') return true;
  const role = roleOf(el);
  return role === 'table' || role === 'grid' || role === 'treegrid' || displayOf(el) === 'table';
};
const hasFrameworkTableHost = (el) => {
  if (!(el instanceof Element)) return false;
  if (hasToken(el, RE_TABLE_ROOT_TOK)) return true;
  for (const t of classTokens(el)) {
    if (/^(ant|el|ivu|vxe|arco|semi|n)[-_]?table$/.test(t)) return true;
    if (/^(ant|el)[-_]?table[-_]?wrapper$/.test(t)) return true;
  }
  return false;
};
const hostOwnsSplitTables = (el) => {
  if (!el || !el.querySelector) return false;
  try {
    const hasHead = !!(el.querySelector('thead, [class*="table-thead"], [class*="table-header"] th, [class*="table-header"] tr'));
    const hasBody = !!(el.querySelector('tbody tr, [class*="table-tbody"] tr, [class*="table-body"] tr'));
    return hasHead && hasBody;
  } catch (_) { return false; }
};
const isHalfNativeTable = (el) => {
  if (!el || el.tagName !== 'TABLE') return false;
  const hasHead = !!el.querySelector('thead');
  const hasBody = !!el.querySelector('tbody');
  // ant/el 固定表头：表头表只有 thead，表体表只有 tbody
  return (hasHead && !hasBody) || (!hasHead && hasBody);
};
const isTableRoot = (el) => {
  if (!(el instanceof Element)) return false;
  if (isNativeTableRoot(el)) return true;
  if (hasFrameworkTableHost(el)) return true;
  if (listDirectPanels(el).length >= 1) return true;
  let hasH = false, hasB = false;
  for (const c of el.children || []) {
    if (isHeaderWrap(c)) hasH = true;
    if (isBodyWrap(c)) hasB = true;
  }
  return hasH && hasB;
};

const sortByLeft = (els) => {
  return els.slice().sort((a, b) => {
    try {
      return a.getBoundingClientRect().left - b.getBoundingClientRect().left;
    } catch (_) { return 0; }
  });
};

const rowCells = (row) => {
  const cells = [];
  for (const c of row.children || []) {
    if (!isPresent(c)) continue;
    // 忽略行高拖拽条等非单元格
    const cls = classOf(c);
    if (/(^|[\s_-])(row[-_]?resize|col[-_]?resize|gutter)([\s_-]|$)/.test(cls)) continue;
    if (isCellLike(c) || c.tagName === 'TD' || c.tagName === 'TH' || looksAbsCell(c)) {
      // 零宽单元格跳过（空操作列）
      try {
        const r = c.getBoundingClientRect();
        if (r.width < 1 && !(cellText(c) || textOf(c))) continue;
      } catch (_) {}
      cells.push(c);
    }
  }
  if (cells.length >= 1) return sortByLeft(cells).slice(0, MAX_COLS);
  const found = [];
  for (const c of row.querySelectorAll('td,th,[role="cell"],[role="gridcell"],[role="columnheader"],[role="rowheader"]')) {
    if (!isVisibleBox(c)) continue;
    let nested = false;
    for (const p of found) { if (p.contains(c)) { nested = true; break; } }
    if (nested) continue;
    found.push(c);
    if (found.length >= MAX_COLS) break;
  }
  if (found.length >= 1) return sortByLeft(found);
  const kids = [];
  for (const c of row.children || []) {
    if (!isVisibleBox(c)) continue;
    if (/(^|[\s_-])(row[-_]?resize|col[-_]?resize|gutter)([\s_-]|$)/.test(classOf(c))) continue;
    kids.push(c);
    if (kids.length >= MAX_COLS) break;
  }
  return kids.length >= 2 ? sortByLeft(kids) : found;
};

const headerCellsFromWrap = (wrap) => {
  if (!wrap) return [];
  let cells = [];
  for (const c of wrap.children || []) {
    if (isHeaderCell(c) || isCellLike(c) || looksAbsCell(c)) cells.push(c);
  }
  if (!cells.length) {
    for (const c of wrap.querySelectorAll('*')) {
      if (isHeaderCell(c)) cells.push(c);
    }
  }
  cells = cells.filter((c, i, arr) => !arr.some((p, j) => j !== i && p.contains(c)));
  return sortByLeft(cells).slice(0, MAX_COLS).map((c, i) => {
    const left = (() => { try { return c.getBoundingClientRect().left; } catch (_) { return 0; } })();
    // 优先业务 col-key；否则用排序后下标（ant/el 拆表头表体时 left 可能不一致）
    const key = colKeyOf(c) || ('I' + i);
    return { key, text: cellText(c) || textOf(c) || ariaOf(c) || '', left };
  });
};

const cellInfo = (c, i) => {
  const left = (() => { try { return c.getBoundingClientRect().left; } catch (_) { return 0; } })();
  const key = colKeyOf(c) || ('I' + i);
  return { key, text: cellText(c) || textOf(c) || ariaOf(c) || '', left };
};

const rowsFromBody = (body) => {
  const out = [];
  if (!body) return out;
  let domIndex = 0;
  const pushRow = (rowEl) => {
    if (!rowEl || out.length >= MAX_ROWS) return;
    if (!isRowLike(rowEl) && rowEl.tagName !== 'TR') return;
    const cells = rowCells(rowEl);
    if (!cells.length) return;
    const infos = cells.map((c, i) => cellInfo(c, i));
    if (!infos.some((v) => v.text)) return;
    out.push({
      el: rowEl,
      key: rowKeyOf(rowEl),
      domIndex: domIndex++,
      top: (() => { try { return rowEl.getBoundingClientRect().top; } catch (_) { return domIndex; } })(),
      cells: infos,
      headerish: cells.some((c) => c.tagName === 'TH' || roleOf(c) === 'columnheader' || isHeaderCell(c)),
    });
  };
  for (const c of body.children || []) pushRow(c);
  if (!out.length) {
    for (const r of body.querySelectorAll('[role="row"], tr')) {
      let nested = false;
      for (const e of out) { if (e.el.contains(r)) { nested = true; break; } }
      if (!nested) pushRow(r);
    }
  }
  if (!out.length) {
    for (const el of body.querySelectorAll('*')) {
      if (hasToken(el, RE_ROW_TOK) || looksAbsRow(el)) {
        let nested = false;
        for (const e of out) { if (e.el.contains(el) || el.contains(e.el)) { nested = true; break; } }
        if (!nested) pushRow(el);
      }
    }
  }
  return out;
};

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const findHScrollParent = (el) => {
  let n = el;
  let best = null;
  while (n && n !== document.documentElement) {
    try {
      const sw = n.scrollWidth || 0;
      const cw = n.clientWidth || 0;
      if (sw > cw + 8) {
        const cs = window.getComputedStyle(n);
        if (/(auto|scroll|overlay)/.test(cs.overflowX) || /(auto|scroll|overlay)/.test(cs.overflow)) {
          return n;
        }
        if (!best) best = n;
      }
    } catch (_) {}
    n = n.parentElement;
  }
  return best;
};

// 列虚拟化：表头齐全、行内只挂可视列。横滑窗口并按 col-key 合并。
const sweepPanelByScroll = async (panel, bodyWrap, headerCols, rowAcc) => {
  if (!bodyWrap) return;
  const scrollEl = findHScrollParent(panel) || findHScrollParent(bodyWrap);
  if (!scrollEl) return;
  let styleMax = 0;
  try {
    for (const h of (panel.querySelectorAll && panel.querySelectorAll('.table-header-cell, [class*="header-cell"]')) || []) {
      const s = (h.getAttribute('style') || '');
      const m = /left\s*:\s*([\d.]+)px/i.exec(s);
      if (m) styleMax = Math.max(styleMax, parseFloat(m[1]) || 0);
    }
  } catch (_) {}
  const start = scrollEl.scrollLeft || 0;
  const maxScroll = Math.max(0, (scrollEl.scrollWidth || 0) - (scrollEl.clientWidth || 0), styleMax);
  if (maxScroll < 40) return;
  const step = Math.max(80, Math.floor((scrollEl.clientWidth || 360) * 0.55));
  const ingest = () => {
    const chunk = rowsFromBody(bodyWrap);
    for (const r of chunk) {
      const rk = r.key !== '' ? r.key : ('~' + Math.round(r.top));
      if (!rowAcc.has(rk)) rowAcc.set(rk, { key: r.key, top: r.top, map: new Map(), headerish: r.headerish });
      const entry = rowAcc.get(rk);
      for (const c of r.cells) {
        if (!c.key) continue;
        if (c.text || !entry.map.has(c.key)) entry.map.set(c.key, c.text || '');
      }
    }
  };
  ingest();
  for (let x = 0; x <= maxScroll + step; x += step) {
    try { scrollEl.scrollLeft = Math.min(x, maxScroll); } catch (_) {}
    await sleep(70);
    ingest();
    if (x >= maxScroll) break;
  }
  try { scrollEl.scrollLeft = start; } catch (_) {}
  await sleep(40);
};

const ingestRowsToAcc = (rows, rowAcc) => {
  for (const r of rows) {
    const rk = r.key !== '' ? r.key : ('~' + Math.round(r.top));
    if (!rowAcc.has(rk)) rowAcc.set(rk, { key: r.key, top: r.top, domIndex: r.domIndex, map: new Map(), headerish: r.headerish });
    const entry = rowAcc.get(rk);
    for (const c of r.cells) {
      if (!c.key) continue;
      if (c.text || !entry.map.has(c.key)) entry.map.set(c.key, c.text || '');
    }
  }
};

const compareRowOrder = (ra, rb) => {
  if (ra.domIndex != null && rb.domIndex != null && ra.domIndex !== rb.domIndex) {
    return ra.domIndex - rb.domIndex;
  }
  if (ra.key !== '' && rb.key !== '') {
    const na = Number(ra.key), nb = Number(rb.key);
    if (!Number.isNaN(na) && !Number.isNaN(nb)) return na - nb;
    return String(ra.key).localeCompare(String(rb.key), undefined, { numeric: true });
  }
  return ra.top - rb.top;
};

// 非虚拟化表：保持 rowsFromBody 的 DOM 行序，按列下标对齐表头（不用 col-key 重排）
const alignRowsByHeaderIndex = (rows, headerCols) => {
  return rows.map((r) => ({
    el: r.el,
    key: r.key,
    top: r.top,
    domIndex: r.domIndex,
    headerish: r.headerish,
    cells: headerCols.map((h, i) => {
      const c = r.cells[i];
      const text = c ? (typeof c === 'string' ? c : (c.text || '')) : '';
      return { key: h.key, text, left: 0 };
    }),
  }));
};

const rowsFromAcc = (rowAcc, headerCols) => {
  const keys = Array.from(rowAcc.keys()).sort((a, b) => compareRowOrder(rowAcc.get(a), rowAcc.get(b)));
  const out = [];
  for (const k of keys) {
    if (out.length >= MAX_ROWS) break;
    const entry = rowAcc.get(k);
    const cells = headerCols.map((h, i) => {
      let text = '';
      if (entry.map.has(h.key)) {
        text = entry.map.get(h.key) || '';
      } else {
        // 虚拟列横滑后 body 单元格可能只有 I0/I1，按列序回退
        const ik = 'I' + i;
        if (entry.map.has(ik)) text = entry.map.get(ik) || '';
      }
      return { key: h.key, text, left: 0 };
    });
    out.push({ el: null, key: entry.key, top: entry.top, domIndex: entry.domIndex, cells, headerish: entry.headerish });
  }
  return out;
};

// 单面板：header + body；必要时横滑补齐虚拟列
const extractPanel = async (panel) => {
  let headerWrap = null;
  let bodyWrap = null;
  for (const c of panel.children || []) {
    if (!headerWrap && isHeaderWrap(c)) headerWrap = c;
    if (!bodyWrap && isBodyWrap(c)) bodyWrap = c;
  }
  if (!headerWrap) {
    for (const c of panel.querySelectorAll('*')) { if (isHeaderWrap(c)) { headerWrap = c; break; } }
  }
  if (!bodyWrap) {
    for (const c of panel.querySelectorAll('*')) { if (isBodyWrap(c)) { bodyWrap = c; break; } }
  }
  const headers = headerCellsFromWrap(headerWrap);
  let rows = rowsFromBody(bodyWrap || panel);
  const maxCells = rows.reduce((m, r) => Math.max(m, r.cells.length), 0);
  // 仅列虚拟化（表头列数明显多于当前可见单元格）才走 Map 重排 + 横滑
  if (headers.length > maxCells + 1) {
    const rowAcc = new Map();
    ingestRowsToAcc(rows, rowAcc);
    await sweepPanelByScroll(panel, bodyWrap || panel, headers, rowAcc);
    rows = rowsFromAcc(rowAcc, headers);
  } else if (headers.length) {
    rows = alignRowsByHeaderIndex(rows, headers);
  }
  return { panel, headers, rows };
};

const mergePanels = (panels) => {
  const ordered = panels.slice().filter((p) => {
    try {
      const r = p.panel.getBoundingClientRect();
      // 宽为 0 的右冻结空壳跳过，但仍保留有表头/单元格的面板
      if (r.width < 2 && !(p.headers && p.headers.some((h) => h.text)) && !(p.rows && p.rows.length)) return false;
      return true;
    } catch (_) { return true; }
  }).sort((a, b) => {
    try { return a.panel.getBoundingClientRect().left - b.panel.getBoundingClientRect().left; } catch (_) { return 0; }
  });
  const headers = [];
  for (const p of ordered) {
    for (const h of p.headers || []) {
      headers.push(h.text || '');
      if (headers.length >= MAX_COLS) break;
    }
    if (headers.length >= MAX_COLS) break;
  }
  // 冻结分栏：按行下标横向拼接各面板单元格（保持 DOM 行序，不用 row-key 重排）
  const rowCount = Math.max(...ordered.map((p) => (p.rows || []).length), 0);
  const rows = [];
  for (let ri = 0; ri < rowCount && rows.length < MAX_ROWS; ri++) {
    const cells = [];
    for (const p of ordered) {
      const r = (p.rows || [])[ri];
      if (!r) continue;
      for (const c of r.cells || []) {
        cells.push(c ? (typeof c === 'string' ? c : (c.text || '')) : '');
        if (cells.length >= MAX_COLS) break;
      }
      if (cells.length >= MAX_COLS) break;
    }
    if (cells.some((c) => c)) rows.push(cells);
  }
  return { headers, rows };
};

const extractRowsFromRoot = (root) => {
  const rows = [];
  const pushRow = (rowEl) => {
    if (!rowEl || rows.length >= MAX_ROWS) return;
    const cells = rowCells(rowEl);
    if (cells.length < 1) return;
    const vals = cells.map((c) => cellText(c) || textOf(c) || ariaOf(c) || '');
    if (!vals.some((v) => v)) return;
    rows.push({ el: rowEl, cells: vals, headerish: cells.some((c) => c.tagName === 'TH' || roleOf(c) === 'columnheader' || isHeaderCell(c)) });
  };
  if (root.tagName === 'TABLE') {
    for (const row of root.querySelectorAll('tr')) {
      if (!isVisibleBox(row) && !isVisible(row)) continue;
      pushRow(row);
    }
    return rows;
  }
  for (const c of root.children || []) if (isRowLike(c)) pushRow(c);
  if (rows.length) return rows;
  for (const r of root.querySelectorAll('[role="row"], tr')) {
    let nested = false;
    for (const e of rows) { if (e.el.contains(r)) { nested = true; break; } }
    if (!nested) pushRow(r);
  }
  if (rows.length) return rows;
  for (const el of root.querySelectorAll('*')) {
    if (displayOf(el) === 'table-row' || hasToken(el, RE_ROW_TOK)) {
      let nested = false;
      for (const e of rows) { if (e.el.contains(el) || el.contains(e.el)) { nested = true; break; } }
      if (!nested) pushRow(el);
    }
  }
  return rows;
};

const finalizeTable = (headers, dataRows, root) => {
  let hs = (headers || []).map((h) => clip(typeof h === 'string' ? h : (h.text || '')));
  let body = (dataRows || []).map((r) => r.map((c) => clip(c || '')));
  if (!hs.some((c) => c) && body.length >= 2) {
    const r0 = body[0], r1 = body[1];
    if (r0.length === r1.length && r0.length >= 2 && r0.every((c) => c && c.length <= 24)) {
      hs = r0;
      body = body.slice(1);
    }
  }
  body = body.filter((r) => r.some((c) => c));
  if (!hs.some((c) => c) && body.length === 0) return null;
  const colCount = Math.max(hs.length, ...body.map((r) => r.length), 0);
  if (colCount < 2 && body.length < 2) return null;
  let caption = '';
  try {
    const cap = root && root.querySelector && root.querySelector('caption');
    if (cap) caption = textOf(cap);
  } catch (_) {}
  if (!caption && root) caption = ariaOf(root);
  return { caption, headers: hs.slice(0, MAX_COLS), rows: body };
};

const extractTable = async (root) => {
  const host = findMergeHost(root) || root;
  const panelEls = listDirectPanels(host);
  if (panelEls.length >= 2) {
    const panels = [];
    for (const p of panelEls) panels.push(await extractPanel(p));
    const merged = mergePanels(panels);
    return finalizeTable(merged.headers, merged.rows, host);
  }
  // 含 ant/el：header 壳 + body 壳（表头/表体拆成两张原生 table）
  const panel = await extractPanel(host);
  if (panel.headers.some((h) => h.text) || panel.rows.length) {
    const headers = panel.headers.map((h) => h.text);
    const rows = panel.rows.map((r) => r.cells.map((c) => (typeof c === 'string' ? c : (c.text || ''))));
    const got = finalizeTable(headers, rows, host);
    if (got) return got;
  }
  const rows = extractRowsFromRoot(host);
  if (rows.length < 1) return null;
  let headers = [];
  let body = rows;
  const headCells = [];
  for (const h of host.querySelectorAll('thead th, thead td, [role="columnheader"]')) {
    if (!isPresent(h)) continue;
    headCells.push(cellText(h) || textOf(h) || '');
    if (headCells.length >= MAX_COLS) break;
  }
  if (headCells.some((c) => c)) headers = headCells;
  else if (rows[0] && rows[0].headerish) { headers = rows[0].cells; body = rows.slice(1); }
  return finalizeTable(headers, body.map((r) => r.cells), host);
};

const tableRoots = [];
const rootScore = (el) => {
  if (!el) return -1;
  // 框架表宿主（表头/表体拆成两张原生 table）必须压过内层 table
  if (hasFrameworkTableHost(el) && hostOwnsSplitTables(el)) return 140;
  if (hasFrameworkTableHost(el)) return 90;
  if (isNativeTableRoot(el)) {
    if (isHalfNativeTable(el)) return 25;
    return 100;
  }
  const mergeN = listDirectPanels(el).length;
  if (mergeN >= 2) return 80 + mergeN;
  let hasH = false, hasB = false;
  for (const c of el.children || []) {
    if (isHeaderWrap(c)) hasH = true;
    if (isBodyWrap(c)) hasB = true;
  }
  if (hasH && hasB) return 70;
  if (findMergeHost(el)) return 40;
  if (hostOwnsSplitTables(el)) return 60;
  return 0;
};
const considerRoot = (el) => {
  if (!el || tableRoots.includes(el)) return;
  for (let i = tableRoots.length - 1; i >= 0; i--) {
    const t = tableRoots[i];
    if (t.contains(el)) {
      // 已有祖先：仅当自己分栏能力更强时替换祖先
      if (rootScore(el) > rootScore(t)) {
        tableRoots.splice(i, 1);
      } else {
        return;
      }
    } else if (el.contains(t)) {
      // 自己是祖先：仅当自己分栏能力 >= 子节点时吞并子节点
      if (rootScore(el) >= rootScore(t)) {
        tableRoots.splice(i, 1);
      } else {
        return;
      }
    }
  }
  tableRoots.push(el);
};

if (WANT_TABLES) {
for (const el of document.querySelectorAll('table, [role="table"], [role="grid"], [role="treegrid"]')) {
  // 半表（仅 thead/tbody）也要纳入，再由更高分宿主吞并
  if (isPresent(el) && (isVisible(el) || el.querySelector('tr'))) considerRoot(el);
}
for (const el of document.querySelectorAll('div, section, main, article')) {
  if (isTableRoot(el) && isPresent(el)) considerRoot(el);
}
for (const el of document.querySelectorAll('div, section, main')) {
  if (displayOf(el) === 'table') considerRoot(el);
}
// 绝对定位分栏表：优先收录「直接子级含多个 header+body 面板」的容器
for (const el of document.querySelectorAll('div, section, main, article')) {
  if (!isPresent(el)) continue;
  if (canMergePanels(el)) considerRoot(el);
  else {
    let h = false, b = false;
    for (const x of el.children || []) {
      if (isHeaderWrap(x)) h = true;
      if (isBodyWrap(x)) b = true;
    }
    if (h && b) considerRoot(el);
  }
}

for (const root of tableRoots) {
  if (tables.length >= MAX_TABLES) break;
  const t = await extractTable(root);
  if (!t) continue;
  tables.push(t);
  claimedTables.add(root);
  markClaimedDeep(root);
}
}

for (const el of document.querySelectorAll('h1,h2,h3,h4,h5,h6,[role="heading"]')) {
  if (!isVisible(el)) continue;
  if (isInsideClaimedTable(el)) continue;
  const text = textOf(el);
  if (!text) continue;
  pushUnique(headings, seenH, { text, hint: '', cmd: '' });
  markClaimed(el);
}

for (const el of document.querySelectorAll('button, [role="button"], input[type="submit"], input[type="button"], input[type="reset"], summary')) {
  if (!isVisible(el)) continue;
  if (isInsideClaimedTable(el)) continue;
  let text = textOf(el) || ariaOf(el);
  if (!text && el.tagName === 'INPUT') text = clip(el.value || '');
  if (!text || text.length > 80) continue;
  pushUnique(buttons, seenB, { text, hint: '', cmd: '' });
  markClaimed(el);
}

for (const el of document.querySelectorAll('a, [role="link"]')) {
  if (!isVisible(el)) continue;
  if (isInsideClaimedTable(el)) continue;
  const text = textOf(el) || ariaOf(el);
  if (!text || text.length > 80) continue;
  if (isClaimed(el)) continue;
  pushUnique(clickables, seenC, { text, hint: '', cmd: '' });
  markClaimed(el);
}

const inputSel = 'input:not([type=hidden]):not([type=checkbox]):not([type=radio]):not([type=submit]):not([type=button]):not([type=reset]):not([type=file]), textarea, [role="textbox"], [role="searchbox"], [role="combobox"], [contenteditable="true"]';
let inputIndex = 0;
for (const el of document.querySelectorAll(inputSel)) {
  if (!isVisible(el)) continue;
  if (isInsideClaimedTable(el)) continue;
  inputIndex += 1;
  const ph = clip(el.placeholder || '');
  const aria = ariaOf(el);
  let label = '';
  try {
    if (el.id) {
      const lb = document.querySelector('label[for="' + CSS.escape(el.id) + '"]');
      if (lb) label = clip(lb.innerText || lb.textContent || '');
    }
  } catch (_) {}
  if (!label) {
    try {
      const wrap = el.closest('label');
      if (wrap) label = clip(wrap.innerText || wrap.textContent || '');
    } catch (_) {}
  }
  const name = clip(el.name || '');
  const primary = label || aria || ph || name || ('输入框#' + inputIndex);
  let hint = '';
  if (label && ph && label !== ph) hint = ph;
  else if (aria && ph && aria !== ph) hint = ph;
  pushUnique(inputs, seenI, { text: primary, hint, cmd: '' });
  markClaimed(el);
}

// 其它短文案（pointer），排除已归类节点
for (const el of document.querySelectorAll('div, span, li, td, th, p, label')) {
  if (!isVisible(el)) continue;
  if (isClaimed(el) || isInsideClaimedTable(el)) continue;
  let cursor = '';
  try { cursor = window.getComputedStyle(el).cursor; } catch (_) {}
  if (cursor !== 'pointer') continue;
  const text = textOf(el) || ariaOf(el);
  if (!text || text.length > 80) continue;
  const r = el.getBoundingClientRect();
  if (r.width < 8 || r.height < 8) continue;
  pushUnique(clickables, seenC, { text, hint: '', cmd: '' });
  markClaimed(el);
}

for (const el of document.querySelectorAll('p, li, td, th, label, span, div')) {
  if (!isVisible(el)) continue;
  if (isClaimed(el) || isInsideClaimedTable(el)) continue;
  const text = textOf(el);
  if (!text || text.length < 2) continue;
  // 只要叶子倾向：有可见子块文字则跳过，减少整页容器噪音
  let childText = false;
  try {
    for (const c of el.children || []) {
      if (isVisible(c) && norm(c.innerText || '').length >= 2) { childText = true; break; }
    }
  } catch (_) {}
  if (childText) continue;
  pushUnique(others, seenO, { text, hint: '', cmd: '' });
}

let frameLabel = null;
try {
  if (window.top !== window) frameLabel = 'iframe';
} catch (_) {
  frameLabel = 'iframe';
}

return {
  ok: true,
  url: location.href || '',
  title: document.title || '',
  frameLabel,
  headings,
  buttons,
  clickables,
  inputs,
  others,
  tables,
};
"#;

async fn eval_collect_on_client(client: &CdpClient, want_tables: bool) -> Vec<FrameCollectRaw> {
    let expr = compose_page_collect_js_with(want_tables);
    let mut frames = Vec::new();
    let mut ctx_ids = client.refresh_frame_contexts().await;
    if ctx_ids.is_empty() {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        ctx_ids = client.refresh_frame_contexts().await;
    }

    let parse = |v: Value| -> Option<FrameCollectRaw> {
        serde_json::from_value(v)
            .ok()
            .filter(|r: &FrameCollectRaw| r.ok || r.error.is_some())
    };

    if ctx_ids.is_empty() {
        if let Ok(v) = client.evaluate(&expr).await {
            if let Some(raw) = parse(v) {
                frames.push(raw);
            }
        }
        return frames;
    }

    for ctx_id in ctx_ids {
        if let Ok(v) = client.evaluate_in_context(&expr, Some(ctx_id)).await {
            if let Some(raw) = parse(v) {
                frames.push(raw);
            }
        }
    }
    if frames.is_empty() {
        if let Ok(v) = client.evaluate(&expr).await {
            if let Some(raw) = parse(v) {
                frames.push(raw);
            }
        }
    }
    frames
}

/// OOPIF 按父页 FrameTree URL 顺序排列（与探针一致）
async fn list_oopifs_stable(parent: &CdpClient, parent_id: &str) -> Vec<crate::chrome::TargetInfo> {
    let mut iframes = crate::chrome::discover_oopif_targets_for_parent(
        parent.port,
        parent_id,
        Some(parent),
        crate::chrome::DiscoverOopifMode::Wait,
    )
    .await;
    if iframes.len() <= 1 {
        return iframes;
    }
    let tree_urls = parent.frame_tree_urls_dfs().await;
    iframes.sort_by_key(|t| {
        tree_urls
            .iter()
            .position(|u| u == &t.url)
            .unwrap_or(usize::MAX)
    });
    iframes
}

/// 主文档 + 同源 iframe，再扫跨域 OOPIF 子 target
async fn eval_collect_page_and_oopifs(
    parent: &CdpClient,
    want_tables: bool,
) -> Vec<FrameCollectRaw> {
    let mut frames = eval_collect_on_client(parent, want_tables).await;

    let iframes = list_oopifs_stable(parent, &parent.target_id).await;
    for (i, iframe) in iframes.iter().enumerate() {
        let Ok(child) = CdpClient::connect_to_any_target_on(parent.port, &iframe.id).await else {
            continue;
        };
        // target 出现 ≠ 可采：等 SPA 正文/控件起来
        let _ = child
            .wait_spa_content_ready(std::time::Duration::from_secs(3))
            .await;
        let mut child_frames = eval_collect_on_client(&child, want_tables).await;
        for raw in &mut child_frames {
            if raw.url.as_deref().unwrap_or("").is_empty() {
                raw.url = Some(iframe.url.clone());
            }
            // OOPIF 在独立 target 上求值时可能被当成「主文档」；强制打标以免污染 URL/标题
            raw.frame_label = Some(format!("oopif#{}", i + 1));
        }
        frames.extend(child_frames);
    }
    frames
}

fn merge_frames(frames: Vec<FrameCollectRaw>) -> Result<(String, String, CollectMerged), String> {
    if frames.is_empty() {
        return Err("采集失败：页面无可用上下文".into());
    }
    let mut url = String::new();
    let mut title = String::new();
    let mut headings = CollectBucket::default();
    let mut buttons = CollectBucket::default();
    let mut clickables = CollectBucket::default();
    let mut inputs = CollectBucket::default();
    let mut others = CollectBucket::default();
    let mut tables: Vec<CollectTable> = Vec::new();
    let mut iframe_n = 0u32;

    for raw in frames {
        if !raw.ok {
            continue;
        }
        let is_iframe = raw
            .frame_label
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let frame_hint = if is_iframe {
            iframe_n += 1;
            Some(format!("iframe#{iframe_n}"))
        } else {
            None
        };
        if !is_iframe {
            if url.is_empty() {
                url = raw.url.unwrap_or_default();
            }
            if title.is_empty() {
                title = raw.title.unwrap_or_default();
            }
        }
        let fh = frame_hint.as_deref();
        for it in raw.headings {
            headings.push(it, fh);
        }
        for it in raw.buttons {
            buttons.push(it, fh);
        }
        for it in raw.clickables {
            clickables.push(it, fh);
        }
        for it in raw.inputs {
            inputs.push(it, fh);
        }
        for it in raw.others {
            others.push(it, fh);
        }
        for t in raw.tables {
            if tables.len() >= MAX_TABLES {
                break;
            }
            if let Some(nt) = normalize_table(t, fh) {
                tables.push(nt);
            }
        }
    }

    Ok((
        url,
        title,
        CollectMerged {
            headings: headings.items,
            buttons: buttons.items,
            clickables: clickables.items,
            inputs: inputs.items,
            others: others.items,
            tables,
        },
    ))
}

#[derive(Debug, Clone, Default)]
struct CollectTable {
    caption: String,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    hint: String,
}

#[derive(Debug, Clone, Default)]
struct CollectMerged {
    headings: Vec<CollectItem>,
    buttons: Vec<CollectItem>,
    clickables: Vec<CollectItem>,
    inputs: Vec<CollectItem>,
    others: Vec<CollectItem>,
    tables: Vec<CollectTable>,
}

fn escape_cell(s: &str) -> String {
    s.replace('|', "\\|")
        .replace('\n', " ")
        .replace('\r', "")
}

fn write_text_table(md: &mut String, title: &str, items: &[CollectItem]) {
    md.push_str(&format!("\n## {title}\n\n"));
    if items.is_empty() {
        md.push_str("（无）\n");
        return;
    }
    let has_note = items.iter().any(|it| !it.hint.is_empty());
    if has_note {
        md.push_str("| # | 文本 | 备注 |\n");
        md.push_str("| --- | --- | --- |\n");
        for (i, it) in items.iter().enumerate() {
            md.push_str(&format!(
                "| {} | {} | {} |\n",
                i + 1,
                escape_cell(&it.text),
                escape_cell(&it.hint)
            ));
        }
    } else {
        md.push_str("| # | 文本 |\n");
        md.push_str("| --- | --- |\n");
        for (i, it) in items.iter().enumerate() {
            md.push_str(&format!("| {} | {} |\n", i + 1, escape_cell(&it.text)));
        }
    }
}

fn pad_row(row: &[String], cols: usize) -> Vec<String> {
    let mut out: Vec<String> = row.iter().cloned().take(cols).collect();
    while out.len() < cols {
        out.push(String::new());
    }
    out
}

fn write_data_tables(md: &mut String, tables: &[CollectTable]) {
    md.push_str("\n## 表格\n\n");
    if tables.is_empty() {
        md.push_str("（无）\n");
        return;
    }
    for (i, t) in tables.iter().enumerate() {
        let mut title = format!("表格 {}", i + 1);
        if !t.caption.is_empty() {
            title = format!("表格 {} · {}", i + 1, t.caption);
        }
        if !t.hint.is_empty() {
            title = format!("{title}（{}）", t.hint);
        }
        md.push_str(&format!("### {title}\n\n"));

        let cols = t
            .headers
            .len()
            .max(t.rows.iter().map(|r| r.len()).max().unwrap_or(0))
            .max(1);
        let headers = if t.headers.iter().any(|c| !c.is_empty()) {
            pad_row(&t.headers, cols)
        } else {
            (1..=cols).map(|n| format!("列{n}")).collect()
        };
        md.push('|');
        for h in &headers {
            md.push(' ');
            md.push_str(&escape_cell(h));
            md.push_str(" |");
        }
        md.push('\n');
        md.push('|');
        for _ in 0..cols {
            md.push_str(" --- |");
        }
        md.push('\n');
        for row in &t.rows {
            let cells = pad_row(row, cols);
            md.push('|');
            for c in &cells {
                md.push(' ');
                md.push_str(&escape_cell(c));
                md.push_str(" |");
            }
            md.push('\n');
        }
        md.push('\n');
    }
}

fn apply_scope(mut data: CollectMerged, scope: CollectScope) -> CollectMerged {
    if !scope.headings {
        data.headings.clear();
    }
    if !scope.buttons {
        data.buttons.clear();
    }
    if !scope.links {
        data.clickables.clear();
    }
    if !scope.inputs {
        data.inputs.clear();
    }
    if !scope.tables {
        data.tables.clear();
    }
    if !scope.others {
        data.others.clear();
    }
    data
}

fn count_collect_items(data: &CollectMerged) -> usize {
    data.headings.len()
        + data.buttons.len()
        + data.clickables.len()
        + data.inputs.len()
        + data.others.len()
        + data.tables.iter().map(|t| t.rows.len()).sum::<usize>()
}

/// 将采集结果格式化为 Markdown（供 documents/ 预览）
pub fn format_collect_markdown(url: &str, title: &str, data: &CollectMerged) -> String {
    format_collect_markdown_scoped(url, title, data, CollectScope::all())
}

fn format_collect_markdown_scoped(
    url: &str,
    title: &str,
    data: &CollectMerged,
    scope: CollectScope,
) -> String {
    let now = Local::now().format("%Y-%m-%d %H:%M:%S");
    let mut md = String::new();
    md.push_str("# 页面采集\n\n");
    md.push_str("## 概览\n\n");
    md.push_str("| 项 | 内容 |\n");
    md.push_str("| --- | --- |\n");
    md.push_str(&format!("| 时间 | {} |\n", escape_cell(&now.to_string())));
    let title = title.trim();
    let url = url.trim();
    if !title.is_empty() {
        md.push_str(&format!("| 标题 | {} |\n", escape_cell(title)));
    }
    if !url.is_empty() {
        md.push_str(&format!("| 地址 | {} |\n", escape_cell(url)));
    }
    md.push_str(&format!("| 范围 | {} |\n", escape_cell(&scope.display_label())));
    if scope.tables {
        md.push_str(&format!("| 表格数 | {} |\n", data.tables.len()));
    }

    if scope.headings {
        write_text_table(&mut md, "标题", &data.headings);
    }
    if scope.buttons {
        write_text_table(&mut md, "按钮", &data.buttons);
    }
    if scope.links {
        write_text_table(&mut md, "链接", &data.clickables);
    }
    if scope.inputs {
        write_text_table(&mut md, "输入框", &data.inputs);
    }
    if scope.tables {
        write_data_tables(&mut md, &data.tables);
    }
    if scope.others {
        write_text_table(&mut md, "其它文字", &data.others);
    }

    let total = count_collect_items(data);
    md.push_str(&format!("\n共 {total} 条（含表格行）。\n"));
    md
}

/// 扫描当前附着页（同源 iframe + 跨域 OOPIF），返回 Markdown 正文与摘要计数
pub async fn collect_page_markdown(client: &mut CdpClient) -> Result<(String, usize), String> {
    collect_page_markdown_scoped(client, CollectScope::all()).await
}

pub async fn collect_page_markdown_scoped(
    client: &mut CdpClient,
    scope: CollectScope,
) -> Result<(String, usize), String> {
    let _ = client.ensure_attached().await;
    let frames = eval_collect_page_and_oopifs(client, scope.wants_tables()).await;
    let (url, title, merged) = merge_frames(frames)?;
    let merged = apply_scope(merged, scope);
    let total = count_collect_items(&merged);
    let md = format_collect_markdown_scoped(&url, &title, &merged, scope);
    Ok((md, total))
}

/// 将已展平的表数据写入「页面采集」Markdown 的表格分类（与 DOM 采集报告同结构）。
pub fn collect_markdown_from_plain_table(
    url: &str,
    title: &str,
    caption: &str,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
) -> (String, usize) {
    let scope = CollectScope::tables_only();
    let data = CollectMerged {
        tables: vec![CollectTable {
            caption: caption.trim().to_string(),
            headers,
            rows,
            hint: String::new(),
        }],
        ..Default::default()
    };
    let total = count_collect_items(&data);
    let md = format_collect_markdown_scoped(url, title, &data, scope);
    (md, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_uses_tables_without_guidance() {
        let data = CollectMerged {
            headings: vec![CollectItem {
                text: "加班台账".into(),
                ..Default::default()
            }],
            buttons: vec![CollectItem {
                text: "导出".into(),
                ..Default::default()
            }],
            clickables: vec![CollectItem {
                text: "时间管理".into(),
                ..Default::default()
            }],
            inputs: vec![CollectItem {
                text: "月份".into(),
                hint: "请选择".into(),
                ..Default::default()
            }],
            others: vec![CollectItem {
                text: "共 20 条".into(),
                ..Default::default()
            }],
            tables: vec![CollectTable {
                caption: "明细".into(),
                headers: vec!["姓名".into(), "时长".into()],
                rows: vec![
                    vec!["张三".into(), "2".into()],
                    vec!["李四".into(), "3".into()],
                ],
                hint: String::new(),
            }],
        };
        let md = format_collect_markdown("https://example.com/x", "加班台账", &data);
        assert!(md.contains("# 页面采集"));
        assert!(md.contains("## 概览"));
        assert!(md.contains("| 项 | 内容 |"));
        assert!(md.contains("## 标题"));
        assert!(md.contains("## 按钮"));
        assert!(md.contains("## 链接"));
        assert!(md.contains("## 输入框"));
        assert!(md.contains("## 表格"));
        assert!(md.contains("### 表格 1 · 明细"));
        assert!(md.contains("| 姓名 | 时长 |"));
        assert!(md.contains("| 张三 | 2 |"));
        assert!(md.contains("## 其它文字"));
        assert!(md.contains("| # | 文本 |"));
        assert!(md.contains("| 1 | 导出 |"));
        assert!(md.contains("| 1 | 时间管理 |"));
        assert!(md.contains("| 1 | 月份 | 请选择 |"));
        assert!(md.contains("https://example.com/x"));
        assert!(!md.contains("可写"));
        assert!(!md.contains("点击导出"));
        assert!(!md.contains("输入月份="));
        assert!(!md.contains("可直接参考"));
        assert!(!md.contains("可点文字"));
    }

    #[test]
    fn parse_collect_args_empty_is_all() {
        let s = parse_collect_args("").unwrap();
        assert!(s.is_all());
        assert!(s.wants_tables());
        assert_eq!(s.file_slug(), None);
    }

    #[test]
    fn parse_collect_args_table_only() {
        let s = parse_collect_args("表格").unwrap();
        assert!(!s.is_all());
        assert!(s.tables);
        assert!(!s.buttons);
        assert_eq!(s.file_slug(), Some("表格"));
        assert_eq!(s.display_label(), "表格");
        let s2 = parse_collect_args("表").unwrap();
        assert!(s2.tables && !s2.headings);
    }

    #[test]
    fn parse_collect_args_multi_and_unknown() {
        let s = parse_collect_args("表格、按钮").unwrap();
        assert!(s.tables && s.buttons && !s.links);
        assert!(parse_collect_args("乱七八糟").is_err());
    }

    #[test]
    fn markdown_scope_tables_omits_other_sections() {
        let data = CollectMerged {
            headings: vec![CollectItem {
                text: "加班台账".into(),
                ..Default::default()
            }],
            buttons: vec![CollectItem {
                text: "导出".into(),
                ..Default::default()
            }],
            tables: vec![CollectTable {
                caption: "明细".into(),
                headers: vec!["姓名".into(), "时长".into()],
                rows: vec![vec!["张三".into(), "2".into()]],
                hint: String::new(),
            }],
            ..Default::default()
        };
        let scope = parse_collect_args("表格").unwrap();
        let filtered = apply_scope(data, scope);
        let md = format_collect_markdown_scoped("https://example.com/x", "加班台账", &filtered, scope);
        assert!(md.contains("| 范围 | 表格 |"));
        assert!(md.contains("## 表格"));
        assert!(md.contains("| 张三 | 2 |"));
        assert!(!md.contains("## 按钮"));
        assert!(!md.contains("## 标题"));
        assert!(!md.contains("| 1 | 导出 |"));
    }

    #[test]
    fn compose_js_collects_div_abs_tables() {
        let s = compose_page_collect_js();
        assert!(s.contains("tables"));
        assert!(s.contains("findMergeHost"));
        assert!(s.contains("listDirectPanels"));
        assert!(s.contains("mergePanels"));
        assert!(s.contains("looksAbsCell"));
        assert!(s.contains("row-index"));
        assert!(s.contains("col-key"));
        assert!(s.contains("sweepPanelByScroll"));
        assert!(s.contains("findHScrollParent"));
        assert!(s.contains("async () =>"));
        assert!(s.contains("MAX_COLS = 120"));
        assert!(!s.contains("const MAX = 60"));
        assert!(!s.contains("__webclawInstallProbe"));
    }

    #[test]
    fn compose_js_recognizes_ant_el_split_tables() {
        let s = compose_page_collect_js();
        assert!(s.contains("endsWithTablePart"));
        assert!(s.contains("hasFrameworkTableHost"));
        assert!(s.contains("hostOwnsSplitTables"));
        assert!(s.contains("isHalfNativeTable"));
        assert!(s.contains("ant-table-column-title"));
        assert!(s.contains("ant-table-wrapper"));
        assert!(s.contains("el-table__header"));
        assert!(s.contains("el-table__body"));
        assert!(s.contains("return 140"));
    }

    #[test]
    fn bucket_has_no_per_category_cap() {
        let mut b = CollectBucket::default();
        for i in 0..120 {
            b.push(
                CollectItem {
                    text: format!("item-{i}"),
                    ..Default::default()
                },
                None,
            );
        }
        assert_eq!(b.items.len(), 120);
    }
}
