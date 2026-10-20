//! 本地 OCR：内嵌 RapidOCR（Rust + ONNX Runtime），模型按需下载到用户机器缓存。

use parking_lot::Mutex;
use rapidocr_core::{
    config::PipelineConfig,
    model::{
        model_set_by_name, ModelAssetSpec, ModelCache, ModelDownloadMode, ModelSetSpec,
        DEFAULT_MODEL_SET_NAME,
    },
    RapidOcr,
};
use rhai::{Dynamic, EvalAltResult, Map};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// 默认模型集（RapidOCR 官方 ONNX，与 Python RapidOCR 同源托管）。
pub const OCR_MODEL_SET: &str = DEFAULT_MODEL_SET_NAME;

static ENGINE: OnceLock<Mutex<Option<RapidOcr>>> = OnceLock::new();
static ENSURING: AtomicBool = AtomicBool::new(false);

fn err(msg: impl Into<String>) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(
        msg.into().into(),
        rhai::Position::NONE,
    ))
}

fn engine_slot() -> &'static Mutex<Option<RapidOcr>> {
    ENGINE.get_or_init(|| Mutex::new(None))
}

fn model_set() -> Result<&'static ModelSetSpec, String> {
    model_set_by_name(OCR_MODEL_SET)
        .ok_or_else(|| format!("未知 OCR 模型集: {OCR_MODEL_SET}"))
}

fn pipeline() -> PipelineConfig {
    PipelineConfig::full()
}

/// `%LOCALAPPDATA%/webclaw/ocr-models`（机器级，与登录用户无关）
pub fn ocr_models_dir() -> PathBuf {
    crate::paths::ocr_models_dir()
}

pub fn model_cache() -> ModelCache {
    ModelCache::new(ocr_models_dir())
}

const ATTACHMENT_DIR_NAMES: &[&str] = &["screenshots", "documents", "downloads", "files"];

fn dir_size_bytes(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    let mut total = 0u64;
    let Ok(walker) = walkdir_shallow(path) else {
        return 0;
    };
    for p in walker {
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

fn walkdir_shallow(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    Ok(out)
}

fn is_attachment_dir_name(name: &str) -> bool {
    ATTACHMENT_DIR_NAMES
        .iter()
        .any(|n| name.eq_ignore_ascii_case(n))
}

/// 运行记录条数：`runs/<id>` + `runs/sessions/<id>`（不含 sessions 容器本身）
fn count_run_records(runs_dir: &Path) -> u64 {
    if !runs_dir.is_dir() {
        return 0;
    }
    let mut n = 0u64;
    let Ok(entries) = std::fs::read_dir(runs_dir) else {
        return 0;
    };
    for ent in entries.flatten() {
        let path = ent.path();
        if !path.is_dir() {
            continue;
        }
        let name = ent.file_name().to_string_lossy().to_string();
        if name.eq_ignore_ascii_case("sessions") {
            if let Ok(sessions) = std::fs::read_dir(&path) {
                for s in sessions.flatten() {
                    if s.path().is_dir() {
                        n = n.saturating_add(1);
                    }
                }
            }
        } else {
            n = n.saturating_add(1);
        }
    }
    n
}

/// 运行记录占用：runs 下除附件目录外的文件体积
fn runs_meta_size_bytes(runs_dir: &Path) -> u64 {
    if !runs_dir.is_dir() {
        return 0;
    }
    let mut total = 0u64;
    let mut stack = vec![runs_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for ent in entries.flatten() {
            let p = ent.path();
            if p.is_dir() {
                let name = ent.file_name().to_string_lossy().to_string();
                if is_attachment_dir_name(&name) {
                    continue;
                }
                stack.push(p);
            } else if let Ok(meta) = ent.metadata() {
                if meta.is_file() {
                    total = total.saturating_add(meta.len());
                }
            }
        }
    }
    total
}

/// 附件：runs 下 screenshots/documents/downloads/files 内的文件
fn scan_attachments(runs_dir: &Path) -> (u64, u64) {
    if !runs_dir.is_dir() {
        return (0, 0);
    }
    let mut count = 0u64;
    let mut bytes = 0u64;
    let mut stack = vec![runs_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for ent in entries.flatten() {
            let p = ent.path();
            if p.is_dir() {
                let name = ent.file_name().to_string_lossy().to_string();
                if is_attachment_dir_name(&name) {
                    let Ok(files) = walkdir_shallow(&p) else {
                        continue;
                    };
                    for f in files {
                        if let Ok(meta) = std::fs::metadata(&f) {
                            if meta.is_file() {
                                count = count.saturating_add(1);
                                bytes = bytes.saturating_add(meta.len());
                            }
                        }
                    }
                } else {
                    stack.push(p);
                }
            }
        }
    }
    (count, bytes)
}

fn browser_cache_size_bytes_at(base: &Path) -> u64 {
    ["chrome-debug", "chrome-profiles", "manual-cache"]
        .iter()
        .map(|name| dir_size_bytes(&base.join(name)))
        .fold(0u64, u64::saturating_add)
}

fn browser_cache_size_bytes() -> u64 {
    browser_cache_size_bytes_at(&crate::paths::app_data_dir())
}

/// 用户目录下可再生运行缓存合计（runs + 浏览器临时数据）
fn user_runtime_cache_bytes(user_dir: &Path) -> u64 {
    let runs_dir = user_dir.join("runs");
    let run_bytes = runs_meta_size_bytes(&runs_dir);
    let (_, attach_bytes) = scan_attachments(&runs_dir);
    let browser_bytes = browser_cache_size_bytes_at(user_dir);
    run_bytes
        .saturating_add(attach_bytes)
        .saturating_add(browser_bytes)
}

fn clear_runtime_dirs_in(user_dir: &Path) {
    if !user_dir.is_dir() {
        return;
    }
    for name in ["runs", "chrome-debug", "chrome-profiles", "manual-cache"] {
        let p = user_dir.join(name);
        if p.is_dir() {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

/// 其他登录用户的运行缓存：账号数 + 合计体积
fn other_users_runtime_cache() -> (u64, u64) {
    let users_root = crate::paths::app_root_dir().join("users");
    if !users_root.is_dir() {
        return (0, 0);
    }
    let current = crate::paths::current_user_key();
    let mut count = 0u64;
    let mut bytes = 0u64;
    let Ok(entries) = std::fs::read_dir(&users_root) else {
        return (0, 0);
    };
    for ent in entries.flatten() {
        let path = ent.path();
        if !path.is_dir() {
            continue;
        }
        let name = ent.file_name().to_string_lossy().to_string();
        if name == current {
            continue;
        }
        let size = user_runtime_cache_bytes(&path);
        count = count.saturating_add(1);
        bytes = bytes.saturating_add(size);
    }
    (count, bytes)
}

fn asset_status(cache: &ModelCache, asset: ModelAssetSpec) -> OcrAssetStatus {
    let path = cache.asset_path(asset);
    let exists = path.is_file();
    let size = if exists {
        std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };
    OcrAssetStatus {
        name: asset.name.to_string(),
        filename: asset.filename.to_string(),
        path: path.to_string_lossy().into_owned(),
        ready: exists,
        size_bytes: size,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrAssetStatus {
    pub name: String,
    pub filename: String,
    pub path: String,
    pub ready: bool,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalCacheStatus {
    /// 系统缓存总占用（OCR + 图像分类模型，机器级）
    pub system_cache_size_bytes: u64,
    /// 当前用户运行记录条数
    pub run_record_count: u64,
    /// 当前用户运行记录占用（不含附件目录）
    pub run_record_size_bytes: u64,
    /// 当前用户浏览器临时数据占用
    pub browser_cache_size_bytes: u64,
    /// 当前用户附件文件个数
    pub attachment_count: u64,
    /// 当前用户附件占用
    pub attachment_size_bytes: u64,
    /// 当前用户应用缓存合计（运行记录 + 浏览器 + 附件）
    pub app_cache_size_bytes: u64,
    /// 其他登录用户目录数
    pub other_users_count: u64,
    /// 其他用户运行缓存合计
    pub other_users_cache_size_bytes: u64,
    pub app_data_dir: String,
    pub app_root_dir: String,
    /// 系统模型缓存根目录（含 ocr-models / classify-models）
    pub system_cache_dir: String,
    /// 图像分类模型目录与占用
    pub classify_dir: String,
    pub classify_size_bytes: u64,
    // —— 以下字段保留供内部/兼容 ——
    pub ocr_model_set: String,
    pub ocr_dir: String,
    pub ocr_ready: bool,
    pub ocr_size_bytes: u64,
    pub ocr_missing: Vec<String>,
    pub ocr_assets: Vec<OcrAssetStatus>,
    pub app_data_size_bytes: u64,
    pub ensuring: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrCacheProgress {
    pub stage: String,
    pub message: String,
    pub ready: bool,
    pub error: Option<String>,
}

pub fn local_cache_status() -> Result<LocalCacheStatus, String> {
    let set = model_set()?;
    let cache = model_cache();
    let missing = cache.missing_assets_for_pipeline(set, pipeline());
    let assets = set
        .assets()
        .into_iter()
        .map(|a| asset_status(&cache, a))
        .collect::<Vec<_>>();
    let ocr_dir = ocr_models_dir();
    let ocr_size = dir_size_bytes(&ocr_dir);
    let classify_dir = crate::paths::classify_models_dir();
    let classify_size = dir_size_bytes(&classify_dir);
    let system_cache_dir = crate::paths::app_root_dir();
    let runs_dir = crate::paths::runs_dir();
    let run_record_count = count_run_records(&runs_dir);
    let run_record_size_bytes = runs_meta_size_bytes(&runs_dir);
    let browser_bytes = browser_cache_size_bytes();
    let (attachment_count, attachment_size_bytes) = scan_attachments(&runs_dir);
    let app_cache_size_bytes = run_record_size_bytes
        .saturating_add(browser_bytes)
        .saturating_add(attachment_size_bytes);
    let (other_users_count, other_users_cache_size_bytes) = other_users_runtime_cache();
    Ok(LocalCacheStatus {
        system_cache_size_bytes: ocr_size.saturating_add(classify_size),
        run_record_count,
        run_record_size_bytes,
        browser_cache_size_bytes: browser_bytes,
        attachment_count,
        attachment_size_bytes,
        app_cache_size_bytes,
        other_users_count,
        other_users_cache_size_bytes,
        app_data_dir: crate::paths::app_data_dir().to_string_lossy().into_owned(),
        app_root_dir: system_cache_dir.to_string_lossy().into_owned(),
        system_cache_dir: system_cache_dir.to_string_lossy().into_owned(),
        classify_dir: classify_dir.to_string_lossy().into_owned(),
        classify_size_bytes: classify_size,
        ocr_model_set: OCR_MODEL_SET.to_string(),
        ocr_dir: ocr_dir.to_string_lossy().into_owned(),
        ocr_ready: missing.is_empty(),
        ocr_size_bytes: ocr_size,
        ocr_missing: missing.iter().map(|a| a.filename.to_string()).collect(),
        ocr_assets: assets,
        app_data_size_bytes: dir_size_bytes(&crate::paths::app_data_dir()),
        ensuring: ENSURING.load(Ordering::SeqCst),
    })
}

/// 下载/校验 OCR 模型；已就绪时快速返回。
/// 优先从国内 CDN 拉取，失败再回落 RapidOCR 默认源（ModelScope）。
pub fn ensure_ocr_models() -> Result<(), String> {
    let set = model_set()?;
    let cache = model_cache();
    let _ = std::fs::create_dir_all(cache.root());
    prefetch_ocr_from_cdn(&cache, set);
    cache
        .ensure_model_set_for_pipeline(set, pipeline(), ModelDownloadMode::Missing)
        .map_err(|e| format!("下载 OCR 模型失败: {e}"))?;
    Ok(())
}

fn prefetch_ocr_from_cdn(cache: &ModelCache, set: &ModelSetSpec) {
    let missing = cache.missing_assets_for_pipeline(set, pipeline());
    for asset in missing {
        let dest = cache.asset_path(asset);
        let relative = format!("ocr-models/{}", asset.filename);
        if let Err(e) = crate::rhai_runtime::model_cdn::download_cdn_relative(&relative, &dest) {
            let _ = std::fs::remove_file(&dest);
            eprintln!("[model-cdn] OCR 预取失败 {}: {e}", asset.filename);
            continue;
        }
        if let Some(expected) = asset.sha256 {
            if let Err(e) =
                crate::rhai_runtime::model_cdn::verify_sha256_or_remove(&dest, expected)
            {
                eprintln!("[model-cdn] OCR 校验失败 {}: {e}", asset.filename);
            }
        }
    }
}

/// 清除 OCR 模型缓存并丢弃已加载引擎。
pub fn clear_ocr_models() -> Result<(), String> {
    {
        let mut slot = engine_slot().lock();
        *slot = None;
    }
    let dir = ocr_models_dir();
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("清除 OCR 缓存失败: {e}"))?;
    }
    let _ = std::fs::create_dir_all(&dir);
    Ok(())
}

/// 清除当前用户业务数据目录中的可再生运行缓存（保留配方/设置等配置）。
pub fn clear_user_app_data() -> Result<(), String> {
    clear_runtime_dirs_in(&crate::paths::app_data_dir());
    Ok(())
}

/// 清除本机其他登录用户的可再生运行缓存（不影响其配方/设置）。
pub fn clear_other_users_app_data() -> Result<(), String> {
    let users_root = crate::paths::app_root_dir().join("users");
    if !users_root.is_dir() {
        return Ok(());
    }
    let current = crate::paths::current_user_key();
    let Ok(entries) = std::fs::read_dir(&users_root) else {
        return Ok(());
    };
    for ent in entries.flatten() {
        let path = ent.path();
        if !path.is_dir() {
            continue;
        }
        let name = ent.file_name().to_string_lossy().to_string();
        if name == current {
            continue;
        }
        clear_runtime_dirs_in(&path);
    }
    Ok(())
}

fn load_engine() -> Result<(), String> {
    ensure_ocr_models()?;
    let set = model_set()?;
    let cache = model_cache();
    let cfg = cache.config_for(set).with_pipeline(pipeline());
    let ocr = RapidOcr::from_config(cfg).map_err(|e| format!("初始化 RapidOCR 失败: {e}"))?;
    *engine_slot().lock() = Some(ocr);
    Ok(())
}

fn with_engine<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce(&mut RapidOcr) -> Result<T, String>,
{
    let mut slot = engine_slot().lock();
    if slot.is_none() {
        drop(slot);
        load_engine()?;
        slot = engine_slot().lock();
    }
    let engine = slot
        .as_mut()
        .ok_or_else(|| "OCR 引擎未就绪".to_string())?;
    f(engine)
}

pub fn ocr_image_file(path: &Path) -> Result<String, String> {
    if !path.is_file() {
        return Err(format!("图片不存在: {}", path.display()));
    }
    with_engine(|ocr| {
        let started = Instant::now();
        let output = ocr
            .run_path(path)
            .map_err(|e| format!("OCR 识别失败: {e}"))?;
        let text = output
            .lines
            .into_iter()
            .map(|l| l.text.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let _ = started;
        Ok(text)
    })
}

/// 从 OCR 文本解析水印常见字段：日期、时间、地点、拍摄人。
pub fn parse_ocr_meta(text: &str) -> Map {
    let mut m = Map::new();
    let raw = text.replace('\r', "\n");
    let flat = raw.replace('\n', " ");

    let mut date = String::new();
    let mut time = String::new();
    // 优先：日期+时间连写；其次：时间：前缀；最后：裸 hh:mm（并纠偏镜像/脏数字）
    if let Some(cap) = regex_first(
        r"(20\d{2})[.\-/年](\d{1,2})[.\-/月](\d{1,2})日?\s*(\d{1,2}):(\d{2,3})",
        &flat,
    ) {
        date = format!(
            "{:04}-{:02}-{:02}",
            cap[0].parse::<u32>().unwrap_or(0),
            cap[1].parse::<u32>().unwrap_or(0),
            cap[2].parse::<u32>().unwrap_or(0)
        );
        time = sanitize_hm(&cap[3], &cap[4]).unwrap_or_default();
    } else {
        if let Some(cap) = regex_first(r"(20\d{2})[.\-/年](\d{1,2})[.\-/月](\d{1,2})", &flat) {
            date = format!(
                "{:04}-{:02}-{:02}",
                cap[0].parse::<u32>().unwrap_or(0),
                cap[1].parse::<u32>().unwrap_or(0),
                cap[2].parse::<u32>().unwrap_or(0)
            );
        }
        if let Some(cap) = regex_first(r"时间[:：\s]*(\d{1,2}):(\d{2,3})", &flat) {
            time = sanitize_hm(&cap[0], &cap[1]).unwrap_or_default();
        } else if let Some(cap) = regex_first(r"(?:真实时间|打卡)?\s*(\d{1,2}):(\d{2,3})", &flat) {
            time = sanitize_hm(&cap[0], &cap[1]).unwrap_or_default();
        }
    }
    // 仍无效时，扫全部候选取第一个合法 hh:mm
    if time.is_empty() {
        time = find_best_time(&flat).unwrap_or_default();
    }

    let mut location = String::new();
    if let Some(cap) = regex_first(
        r"(?:地点|位置)[:：\s]*([^\n|]+)",
        &raw,
    ) {
        location = clean_location(&cap[0]);
    } else if let Some(cap) = regex_first(
        r"阳泉市?[^\n·]*[·•]([^\n|]+)",
        &flat,
    ) {
        location = clean_location(&cap[0]);
    }

    let mut photographer = String::new();
    if let Some(cap) = regex_first(
        r"拍摄人[:：\s]*(班组织\s*[-－—_]?\s*[^\n|，,；;]+)",
        &raw,
    ) {
        photographer = normalize_admin(&cap[0]);
    } else if let Some(cap) = regex_first(r"拍摄人[:：\s]*([^\n|，,；;]+)", &raw) {
        photographer = normalize_admin(&cap[0]);
    } else if let Some(cap) = regex_first(r"(班组织\s*[-－—_]?\s*[^\n|，,；;\s]+)", &flat) {
        photographer = normalize_admin(&cap[0]);
    }

    let problem_type = classify_problem_type(text);

    let datetime = if !date.is_empty() && !time.is_empty() {
        format!("{date} {time}")
    } else if !date.is_empty() {
        date.clone()
    } else {
        time.clone()
    };

    m.insert("date".into(), Dynamic::from(date));
    m.insert("time".into(), Dynamic::from(time));
    m.insert("datetime".into(), Dynamic::from(datetime));
    m.insert("location".into(), Dynamic::from(location));
    m.insert("photographer".into(), Dynamic::from(photographer));
    m.insert("problem_type".into(), Dynamic::from(problem_type));
    m.insert("raw".into(), Dynamic::from(text.to_string()));
    m
}

/// 问题点位类型（优先匹配更具体的关键词）。
pub fn classify_problem_type(text: &str) -> String {
    let t = text.replace('\r', "\n").replace('\n', "");
    let lower = t.to_lowercase();
    // 公厕 / 便池
    if contains_any(&t, &["公厕", "便池", "厕位", "蹲位", "厕所不净", "卫生间"]) {
        return "公厕便池不净".into();
    }
    // 垃圾桶未密闭
    if contains_any(&t, &["垃圾桶未密闭", "箱体未密闭", "桶盖未"])
        || (contains_any(&t, &["未密闭", "未盖", "没盖", "敞口", "敞开", "未关闭"])
            && contains_any(&t, &["桶", "垃圾箱", "果皮箱"]))
    {
        return "垃圾桶未密闭".into();
    }
    // 垃圾桶满溢
    if contains_any(&t, &["满溢", "溢出", "爆桶", "桶满", "装满", "超满"])
        || (contains_any(&t, &["垃圾桶", "果皮箱", "垃圾箱"]) && contains_any(&t, &["满", "溢"]))
    {
        return "垃圾桶满溢".into();
    }
    // 树坑
    if contains_any(&t, &["树坑", "树穴", "树池"]) {
        return "树坑散落垃圾".into();
    }
    // 垃圾点
    if contains_any(&t, &["垃圾点", "收集点", "投放点", "中转点"]) {
        return "垃圾点散落垃圾".into();
    }
    // 路面（默认）
    if contains_any(&t, &["路面", "道路", "人行道", "机动车道", "非机动车", "散落", "落地"])
        || lower.contains("road")
    {
        return "路面散落垃圾".into();
    }
    "路面散落垃圾".into()
}

fn contains_any(text: &str, keys: &[&str]) -> bool {
    keys.iter().any(|k| text.contains(k))
}

/// 管理员：只保留人名（去掉「班组织-」前缀）。
fn normalize_admin(s: &str) -> String {
    let s = s
        .trim()
        .trim_matches(|c: char| c == '·' || c == '•' || c == ':' || c == '：')
        .replace('－', "-")
        .replace('—', "-")
        .replace('_', "-");
    let s = s.split_whitespace().collect::<Vec<_>>().join("");
    if s.is_empty() {
        return String::new();
    }
    // 过滤水印噪声
    if contains_any(&s, &["水印", "时间", "地点", "阳泉中环", "CLEAN", "今日"]) {
        return String::new();
    }
    let name = if let Some(rest) = s.strip_prefix("班组织") {
        rest.trim_start_matches('-').trim()
    } else if s.contains('-') {
        s.rsplit('-').next().unwrap_or(&s).trim()
    } else {
        s.as_str()
    };
    if name.is_empty() || name.chars().count() > 12 {
        return String::new();
    }
    name.to_string()
}

/// 规范化/纠偏 OCR 时分。常见问题：水印镜像成 80:60、分钟多一位 09:061。
fn sanitize_hm(h_raw: &str, m_raw: &str) -> Option<String> {
    let h_digits: String = h_raw.chars().filter(|c| c.is_ascii_digit()).collect();
    let m_digits: String = m_raw.chars().filter(|c| c.is_ascii_digit()).collect();
    if h_digits.is_empty() || m_digits.is_empty() {
        return None;
    }

    // 分钟 3 位时取前两位（16:391 → 16:39）
    let m_use = if m_digits.len() >= 3 {
        m_digits[..2].to_string()
    } else {
        m_digits
    };
    let h_use = if h_digits.len() > 2 {
        h_digits[h_digits.len() - 2..].to_string()
    } else {
        h_digits
    };

    if let Some(t) = hm_ok(&h_use, &m_use) {
        return Some(t);
    }

    let all = format!("{h_use}{m_use}");
    if all.len() < 3 {
        return None;
    }
    let rev: String = all.chars().rev().collect();
    // 水印镜像：09:08 → 80:60；反转后再 6↔9：8060 → 0608 → 0908
    let swapped: String = rev
        .chars()
        .map(|c| match c {
            '6' => '9',
            '9' => '6',
            o => o,
        })
        .collect();
    if let Some(t) = split_hm_digits(&swapped) {
        return Some(t);
    }
    split_hm_digits(&rev)
}

fn hm_ok(h: &str, m: &str) -> Option<String> {
    let hh = h.parse::<u32>().ok()?;
    let mm = m.parse::<u32>().ok()?;
    if hh < 24 && mm < 60 {
        // 水印通常只有时分，秒固定补 00 → HH:mm:ss
        Some(format!("{hh:02}:{mm:02}:00"))
    } else {
        None
    }
}

fn split_hm_digits(key: &str) -> Option<String> {
    if key.len() == 3 {
        return hm_ok(&key[..1], &key[1..]);
    }
    if key.len() >= 4 {
        let split = key.len() - 2;
        return hm_ok(&key[..split], &key[split..]);
    }
    None
}

fn find_best_time(flat: &str) -> Option<String> {
    let re = regex::Regex::new(r"(\d{1,2}):(\d{2,3})").ok()?;
    for cap in re.captures_iter(flat) {
        let h = cap.get(1)?.as_str();
        let m = cap.get(2)?.as_str();
        if let Some(t) = sanitize_hm(h, m) {
            return Some(t);
        }
    }
    None
}

/// 处理时长 HH:mm:ss（发现 → 整改）。
pub fn format_process_duration(discover: &str, fix: &str) -> String {
    let (Some(a), Some(b)) = (parse_dt(discover), parse_dt(fix)) else {
        return String::new();
    };
    let secs = b.signed_duration_since(a).num_seconds();
    if secs < 0 {
        return String::new();
    }
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

fn clean_location(s: &str) -> String {
    s.split(|c| matches!(c, '|' | '/' | '\\'))
        .next()
        .unwrap_or(s)
        .trim()
        .trim_start_matches(|c: char| {
            c.is_whitespace() || c == '·' || c == '•' || c == ':' || c == '：'
        })
        .trim_end_matches(|c: char| c.is_whitespace() || c == '·' || c == '•')
        .replace("阳泉市城区", "")
        .replace("阳泉市", "")
        .replace("城区", "")
        .trim_matches(|c: char| c == '·' || c == '•' || c.is_whitespace())
        .to_string()
}

fn regex_first(pat: &str, text: &str) -> Option<Vec<String>> {
    let re = regex::Regex::new(pat).ok()?;
    let cap = re.captures(text)?;
    let mut out = Vec::new();
    for i in 1..cap.len() {
        out.push(cap.get(i)?.as_str().trim().to_string());
    }
    Some(out)
}

pub(crate) fn parse_dt(s: &str) -> Option<chrono::NaiveDateTime> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let fmts = [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y.%m.%d %H:%M:%S",
        "%Y.%m.%d %H:%M",
        "%Y/%m/%d %H:%M:%S",
        "%Y/%m/%d %H:%M",
    ];
    for f in fmts {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(t, f) {
            return Some(dt);
        }
    }
    // 仅 HH:mm:ss / HH:mm → 同一虚拟日，便于算时长
    if let Ok(tod) = chrono::NaiveTime::parse_from_str(t, "%H:%M:%S") {
        return chrono::NaiveDate::from_ymd_opt(2000, 1, 1).map(|d| d.and_time(tod));
    }
    if let Ok(tod) = chrono::NaiveTime::parse_from_str(t, "%H:%M") {
        return chrono::NaiveDate::from_ymd_opt(2000, 1, 1).map(|d| d.and_time(tod));
    }
    None
}

pub fn rhai_ocr_image(path: &str) -> Result<String, Box<EvalAltResult>> {
    ocr_image_file(Path::new(path)).map_err(err)
}

/// 后台首次下载；可多次调用，并发时只跑一路。
pub fn spawn_ensure_ocr_models<F>(on_progress: F)
where
    F: Fn(OcrCacheProgress) + Send + Sync + 'static,
{
    if ENSURING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        on_progress(OcrCacheProgress {
            stage: "busy".into(),
            message: "OCR 模型正在下载中…".into(),
            ready: false,
            error: None,
        });
        return;
    }

    std::thread::Builder::new()
        .name("webclaw-ocr-ensure".into())
        .spawn(move || {
            on_progress(OcrCacheProgress {
                stage: "start".into(),
                message: "正在检查 / 下载 RapidOCR 模型…".into(),
                ready: false,
                error: None,
            });
            let result = ensure_ocr_models();
            ENSURING.store(false, Ordering::SeqCst);
            match result {
                Ok(()) => on_progress(OcrCacheProgress {
                    stage: "done".into(),
                    message: "OCR 模型已就绪".into(),
                    ready: true,
                    error: None,
                }),
                Err(e) => on_progress(OcrCacheProgress {
                    stage: "error".into(),
                    message: e.clone(),
                    ready: false,
                    error: Some(e),
                }),
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_set_resolves() {
        assert!(model_set().is_ok());
    }

    #[test]
    fn duration_is_hh_mm_ss() {
        assert_eq!(
            format_process_duration("2026-09-14 16:39:00", "2026-09-14 16:47:00"),
            "00:08:00"
        );
        assert_eq!(format_process_duration("08:54:00", "08:56:00"), "00:02:00");
    }

    #[test]
    fn admin_from_after_watermark() {
        let m = parse_ocr_meta(
            "阳泉中环洁\n时间：2026.09.14 08:56\n地点：阳泉市城区·义北小区\n拍摄人：班组织-石素萍",
        );
        assert_eq!(
            m.get("photographer").unwrap().clone().into_string().unwrap(),
            "石素萍"
        );
        assert_eq!(
            m.get("time").unwrap().clone().into_string().unwrap(),
            "08:56:00"
        );
    }

    #[test]
    fn time_sanitizes_mirror_and_extra_digit() {
        // 水印镜像：09:08 → 80:60（翻转 + 9↔6）
        assert_eq!(sanitize_hm("80", "60").as_deref(), Some("09:08:00"));
        // 正常时间不变
        assert_eq!(sanitize_hm("09", "08").as_deref(), Some("09:08:00"));
        assert_eq!(sanitize_hm("16", "39").as_deref(), Some("16:39:00"));
        // 分钟多一位
        assert_eq!(sanitize_hm("16", "391").as_deref(), Some("16:39:00"));
        let m = parse_ocr_meta("真实时间\n80:60\n2026-09-14\n星期一晴18℃");
        assert_eq!(
            m.get("time").unwrap().clone().into_string().unwrap(),
            "09:08:00"
        );
    }

    #[test]
    fn problem_type_keywords() {
        assert_eq!(classify_problem_type("树坑内有散落垃圾"), "树坑散落垃圾");
        assert_eq!(classify_problem_type("垃圾桶满溢严重"), "垃圾桶满溢");
        assert_eq!(classify_problem_type("公厕便池不净"), "公厕便池不净");
        assert_eq!(classify_problem_type("路边有烟头"), "路面散落垃圾");
    }
}
