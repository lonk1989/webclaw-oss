pub mod excel_compat;
pub mod http_json;
pub mod local_ocr;
pub mod model_cdn;
pub mod sanitation_classify;
pub mod scene_match;
pub mod wxgf_voip;
pub mod table;
pub mod wechat_dat;
pub mod workbook;
pub mod zip_fs;

use table::ScriptTable;
use workbook::ScriptWorkbook;
use rhai::{Dynamic, Engine, EvalAltResult, Scope};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct RhaiRunResult {
    pub message: String,
    pub logs: Vec<String>,
    pub artifacts: Vec<RhaiArtifact>,
}

#[derive(Clone)]
pub struct RhaiArtifact {
    pub name: String,
    pub path: PathBuf,
}

/// 实时 print 回调（例如推送到主窗口对话日志）
pub type RhaiPrintSink = Arc<dyn Fn(&str) + Send + Sync>;

/// 脚本 `track_output` 落盘后回调（用于运行中刷新产物列表）
pub type RhaiArtifactSink = Arc<dyn Fn(&Path) + Send + Sync>;

const CANCEL_TOKEN: &str = "已取消";

pub fn run_script(
    source: &str,
    run_dir: &Path,
    templates_dir: &Path,
    args: &str,
    on_print: Option<RhaiPrintSink>,
) -> Result<RhaiRunResult, String> {
    run_script_cancel(source, run_dir, templates_dir, args, on_print, None, None)
}

/// 与 [`run_script`] 相同，额外接收停止标记：主窗口点「停止」后引擎求值与 `sleep_secs` 会尽快退出。
pub fn run_script_cancel(
    source: &str,
    run_dir: &Path,
    templates_dir: &Path,
    args: &str,
    on_print: Option<RhaiPrintSink>,
    cancel: Option<Arc<AtomicBool>>,
    on_artifact: Option<RhaiArtifactSink>,
) -> Result<RhaiRunResult, String> {
    if cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst)) {
        return Err(CANCEL_TOKEN.into());
    }

    let logs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let artifacts: Arc<Mutex<Vec<RhaiArtifact>>> = Arc::new(Mutex::new(Vec::new()));
    let base = run_dir.to_path_buf();
    let templates = templates_dir.to_path_buf();

    let mut engine = Engine::new();
    // fill-attendance 含多层 while/if 嵌套，debug 下默认函数体深度 16 会触发 ExprTooDeep
    engine.set_max_expr_depths(128, 64);
    table::register(&mut engine);
    workbook::register(&mut engine);

    {
        let logs = logs.clone();
        let on_print = on_print.clone();
        engine.on_print(move |s| {
            let line = s.to_string();
            logs.lock().unwrap().push(line.clone());
            if let Some(ref sink) = on_print {
                sink(&line);
            }
        });
    }

    // 每条 Rhai 语句之间检查停止标记；脚本无法 try/catch 吞掉 ErrorTerminated
    if let Some(flag) = cancel.clone() {
        engine.on_progress(move |_| {
            if flag.load(Ordering::SeqCst) {
                Some(Dynamic::from(CANCEL_TOKEN))
            } else {
                None
            }
        });
    }

    register_fs_fns(
        &mut engine,
        base.clone(),
        templates.clone(),
        artifacts.clone(),
        on_artifact,
    );
    register_table_fns(&mut engine, base.clone());
    register_workbook_fns(&mut engine, base.clone());
    register_http_json_fns(&mut engine, cancel.clone());
    register_args_fns(&mut engine, args);
    register_feishu_push_fns(&mut engine, base.clone());
    register_wechat_dat_fns(&mut engine, base.clone());
    register_local_ocr_fns(&mut engine, base.clone());

    let mut scope = Scope::new();
    // 保留原始整段参数；分词请用 arg / args / require_arg
    scope.push("ARGS", args.trim().to_string());

    let _ = engine
        .eval_with_scope::<Dynamic>(&mut scope, source)
        .map_err(|e| map_rhai_eval_err(e.as_ref(), cancel.as_deref()))?;

    if cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst)) {
        return Err(CANCEL_TOKEN.into());
    }

    let log_lines = logs.lock().unwrap().clone();
    let arts = artifacts.lock().unwrap().clone();
    let message = if log_lines.is_empty() {
        "Rhai 脚本执行完成".to_string()
    } else {
        log_lines
            .last()
            .cloned()
            .unwrap_or_else(|| "Rhai 脚本执行完成".to_string())
    };

    Ok(RhaiRunResult {
        message,
        logs: log_lines,
        artifacts: arts,
    })
}

fn map_rhai_eval_err(err: &EvalAltResult, cancel: Option<&AtomicBool>) -> String {
    if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
        return CANCEL_TOKEN.into();
    }
    match err {
        EvalAltResult::ErrorTerminated(token, _) => {
            let s = token.to_string();
            if s.contains(CANCEL_TOKEN) || s == "()" || s.is_empty() {
                CANCEL_TOKEN.into()
            } else {
                s
            }
        }
        other => {
            let s = other.to_string();
            if s.contains(CANCEL_TOKEN) {
                CANCEL_TOKEN.into()
            } else {
                s
            }
        }
    }
}

/// 按空白（空格 / Tab）拆分 `ARGS`，与内置脚本手写循环规则一致。
fn split_script_args(raw: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    for ch in raw.trim().chars() {
        if ch == ' ' || ch == '\t' {
            if !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

fn arg_at(parts: &[String], index: i64) -> String {
    if index < 0 {
        return String::new();
    }
    parts
        .get(index as usize)
        .cloned()
        .unwrap_or_default()
}

/// 参数 API：保留全局 `ARGS` 原样；封装分词访问。
/// - `arg(0)` / `arg(1)` … — 0 基，缺省返回 `""`
/// - `arg(0, "默认")` — 缺省返回默认值
/// - `args()` — 全部 token 数组
/// - `arg_count()` — token 个数
/// - `require_arg(0, "用法：脚本某某 公司 月份")` — 缺省则 throw
fn register_args_fns(engine: &mut Engine, raw_args: &str) {
    let parts = Arc::new(split_script_args(raw_args));

    let p0 = parts.clone();
    engine.register_fn("arg", move |index: i64| -> String { arg_at(&p0, index) });
    let p0f = parts.clone();
    engine.register_fn("arg", move |index: f64| -> String {
        arg_at(&p0f, index as i64)
    });

    let p1 = parts.clone();
    engine.register_fn("arg", move |index: i64, default: &str| -> String {
        let v = arg_at(&p1, index);
        if v.is_empty() {
            default.to_string()
        } else {
            v
        }
    });
    let p1f = parts.clone();
    engine.register_fn("arg", move |index: f64, default: &str| -> String {
        let v = arg_at(&p1f, index as i64);
        if v.is_empty() {
            default.to_string()
        } else {
            v
        }
    });

    let p_all = parts.clone();
    engine.register_fn("args", move || -> rhai::Array {
        p_all
            .iter()
            .map(|s| Dynamic::from(s.clone()))
            .collect()
    });

    let p_n = parts.clone();
    engine.register_fn("arg_count", move || -> i64 { p_n.len() as i64 });

    let p_req = parts.clone();
    engine.register_fn(
        "require_arg",
        move |index: i64, usage: &str| -> Result<String, Box<EvalAltResult>> {
            let v = arg_at(&p_req, index);
            if v.is_empty() {
                let hint = usage.trim();
                let msg = if hint.is_empty() {
                    format!("缺少参数 arg({index})")
                } else {
                    format!("缺少参数 arg({index})。{hint}")
                };
                return Err(err_runtime(msg));
            }
            Ok(v)
        },
    );
    let p_req_f = parts;
    engine.register_fn(
        "require_arg",
        move |index: f64, usage: &str| -> Result<String, Box<EvalAltResult>> {
            let index = index as i64;
            let v = arg_at(&p_req_f, index);
            if v.is_empty() {
                let hint = usage.trim();
                let msg = if hint.is_empty() {
                    format!("缺少参数 arg({index})")
                } else {
                    format!("缺少参数 arg({index})。{hint}")
                };
                return Err(err_runtime(msg));
            }
            Ok(v)
        },
    );
}

fn register_feishu_push_fns(engine: &mut Engine, base: PathBuf) {
    engine.register_fn(
        "feishu_push_message",
        |text: &str| -> Result<(), Box<EvalAltResult>> {
            crate::feishu_push::push_text_blocking(text).map_err(err_runtime)?;
            Ok(())
        },
    );

    let base_att = base.clone();
    engine.register_fn(
        "feishu_push_attachment",
        move |query: &str| -> Result<(), Box<EvalAltResult>> {
            let queries = crate::feishu_push::split_attachment_queries(query);
            if queries.is_empty() {
                let path = crate::commands::runtime::resolve_upload_file(&base_att, None)
                    .map_err(err_runtime)?;
                crate::feishu_push::push_file_path_blocking(&path).map_err(err_runtime)?;
                return Ok(());
            }
            for q in queries {
                let path = crate::commands::runtime::resolve_upload_file(&base_att, Some(&q))
                    .map_err(err_runtime)?;
                crate::feishu_push::push_file_path_blocking(&path).map_err(err_runtime)?;
            }
            Ok(())
        },
    );

    // 无参重载：单附件自动
    let base_att0 = base.clone();
    engine.register_fn("feishu_push_attachment", move || -> Result<(), Box<EvalAltResult>> {
        let path = crate::commands::runtime::resolve_upload_file(&base_att0, None)
            .map_err(err_runtime)?;
        crate::feishu_push::push_file_path_blocking(&path).map_err(err_runtime)?;
        Ok(())
    });

    let base_ai = base.clone();
    engine.register_fn(
        "feishu_push_ai",
        move |question: &str| -> Result<(), Box<EvalAltResult>> {
            feishu_push_ai_blocking(&base_ai, question).map_err(err_runtime)
        },
    );
    let base_ai0 = base;
    engine.register_fn("feishu_push_ai", move || -> Result<(), Box<EvalAltResult>> {
        feishu_push_ai_blocking(&base_ai0, "").map_err(err_runtime)
    });
}

fn feishu_push_ai_blocking(run_dir: &Path, question: &str) -> Result<(), String> {
    let session = crate::run_artifacts::RunSession::open_existing(run_dir)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("创建运行时失败: {e}"))?;
    let (report, _summary, answer) = rt.block_on(async {
        crate::ai_analyze::analyze_run_artifacts(&session, question, None).await
    })?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let file_name = format!("AI分析-{stamp}.md");
    let _ = session.save_text_document(&file_name, &report, None);
    let answer = answer.trim();
    if answer.is_empty() {
        return Err("AI 未生成可推送的回答".into());
    }
    let push_body = if answer.chars().count() > 3800 {
        let truncated: String = answer.chars().take(3800).collect();
        format!("{truncated}\n\n…（内容过长已截断，完整报告见 documents/{file_name}）")
    } else {
        answer.to_string()
    };
    crate::feishu_push::push_text_blocking(&push_body)?;
    Ok(())
}

fn register_fs_fns(
    engine: &mut Engine,
    base: PathBuf,
    templates_dir: PathBuf,
    artifacts: Arc<Mutex<Vec<RhaiArtifact>>>,
    on_artifact: Option<RhaiArtifactSink>,
) {
    let _base = base.clone();
    engine.register_fn("basename", move |path: &str| -> String {
        Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path)
            .to_string()
    });

    engine.register_fn("monthly_department_name", |path: &str| -> String {
        let name = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path);
        regex::Regex::new(r"^考勤月报_(.+?)_\d{4}-\d{2}-\d{2}_")
            .ok()
            .and_then(|re| re.captures(name))
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default()
    });
    engine.register_fn("department_from_monthly", |path: &str| -> String {
        let name = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path);
        regex::Regex::new(r"^考勤月报_(.+?)_\d{4}-\d{2}-\d{2}_")
            .ok()
            .and_then(|re| re.captures(name))
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default()
    });
    engine.register_fn("month_from_monthly", |path: &str| -> String {
        let name = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path);
        regex::Regex::new(r"^考勤月报_.+?_(\d{4}-\d{2})-\d{2}_")
            .ok()
            .and_then(|re| re.captures(name))
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default()
    });

    let base_copy = base.clone();
    engine.register_fn("copy_file", move |src: &str, dst: &str| -> Result<(), Box<EvalAltResult>> {
        let from = excel_compat::resolve_excel_path(&resolve_path(&base_copy, src));
        let to = resolve_path(&base_copy, dst);
        if !from.is_file() {
            return Err(err_runtime(format!(
                "复制文件失败：源不存在 {}",
                resolve_path(&base_copy, src).display()
            )));
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| err_runtime(format!("创建目录失败: {e}")))?;
        }
        std::fs::copy(&from, &to).map_err(|e| {
            err_runtime(format!("复制文件失败 {} → {}: {e}", from.display(), to.display()))
        })?;
        Ok(())
    });

    /// 尽力复制；目标被占用（如 Excel 打开）时返回 false，不抛错。
    let base_try_copy = base.clone();
    engine.register_fn(
        "try_copy_file",
        move |src: &str, dst: &str| -> Result<bool, Box<EvalAltResult>> {
            let from = excel_compat::resolve_excel_path(&resolve_path(&base_try_copy, src));
            let to = resolve_path(&base_try_copy, dst);
            if !from.is_file() {
                return Err(err_runtime(format!(
                    "try_copy_file：源不存在 {}",
                    resolve_path(&base_try_copy, src).display()
                )));
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| err_runtime(format!("创建目录失败: {e}")))?;
            }
            match std::fs::copy(&from, &to) {
                Ok(_) => Ok(true),
                Err(e) => {
                    let kind = e.kind();
                    let locked = kind == std::io::ErrorKind::PermissionDenied
                        || kind == std::io::ErrorKind::WouldBlock
                        || format!("{e}").contains("另一个程序正在使用")
                        || format!("{e}").to_ascii_lowercase().contains("sharing")
                        || format!("{e}").contains("used by another process");
                    if locked {
                        Ok(false)
                    } else {
                        // 其它错误也当作「暂不可写」，避免实时循环因偶发 IO 中断
                        Ok(false)
                    }
                }
            }
        },
    );

    let base_find2 = base.clone();
    engine.register_fn(
        "find_file",
        move |subdir: &str, pattern: &str| -> Result<String, Box<EvalAltResult>> {
            find_file_impl(&base_find2, subdir, pattern, None)
        },
    );
    let base_find_optional = base.clone();
    engine.register_fn(
        "find_file_optional",
        move |subdir: &str, pattern: &str| -> Result<String, Box<EvalAltResult>> {
            find_file_optional_impl(&base_find_optional, subdir, pattern, None)
        },
    );
    let base_find3 = base.clone();
    engine.register_fn(
        "find_file",
        move |subdir: &str, pattern: &str, exclude: &str| -> Result<String, Box<EvalAltResult>> {
            let exclude = if exclude.is_empty() { None } else { Some(exclude) };
            find_file_impl(&base_find3, subdir, pattern, exclude)
        },
    );

    let tpl_find2 = templates_dir.clone();
    engine.register_fn(
        "find_template",
        move |pattern: &str| -> Result<String, Box<EvalAltResult>> {
            find_file_impl(&tpl_find2, ".", pattern, None)
        },
    );
    let tpl_find_optional = templates_dir.clone();
    engine.register_fn(
        "find_template_optional",
        move |pattern: &str, exclude: &str| -> Result<String, Box<EvalAltResult>> {
            let exclude = if exclude.is_empty() { None } else { Some(exclude) };
            find_file_optional_impl(&tpl_find_optional, ".", pattern, exclude)
        },
    );
    let tpl_find3 = templates_dir.clone();
    engine.register_fn(
        "find_template",
        move |pattern: &str, exclude: &str| -> Result<String, Box<EvalAltResult>> {
            let exclude = if exclude.is_empty() { None } else { Some(exclude) };
            find_file_impl(&tpl_find3, ".", pattern, exclude)
        },
    );

    let tpl_copy_from = templates_dir.clone();
    let tpl_copy_to = base.clone();
    engine.register_fn(
        "copy_template",
        move |template_name: &str, dest_in_run: &str| -> Result<(), Box<EvalAltResult>> {
            let from = excel_compat::resolve_excel_path(&tpl_copy_from.join(template_name));
            let to = resolve_path(&tpl_copy_to, dest_in_run);
            if !from.is_file() {
                return Err(err_runtime(format!("模板不存在: {}", tpl_copy_from.join(template_name).display())));
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| err_runtime(format!("创建目录失败: {e}")))?;
            }
            std::fs::copy(&from, &to).map_err(|e| {
                err_runtime(format!(
                    "复制模板失败 {} → {}: {e}",
                    from.display(),
                    to.display()
                ))
            })?;
            Ok(())
        },
    );

    let base_track = base.clone();
    engine.register_fn("track_output", move |path: &str| {
        let full = resolve_path(&base_track, path);
        if full.is_file() {
            let name = full
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("output")
                .to_string();
            artifacts.lock().unwrap().push(RhaiArtifact {
                name,
                path: full.clone(),
            });
            if let Some(ref sink) = on_artifact {
                sink(&full);
            }
        }
    });

    let base_zip = base.clone();
    engine.register_fn(
        "extract_zip",
        move |zip_path: &str, dest_dir: &str| -> Result<rhai::Array, Box<EvalAltResult>> {
            let zip_full = resolve_path(&base_zip, zip_path);
            let dest_full = resolve_path(&base_zip, dest_dir);
            let files = zip_fs::extract_zip(&zip_full, &dest_full)?;
            let dest_norm = dest_dir.trim_end_matches(['/', '\\']);
            let mut out = rhai::Array::new();
            for name in files {
                out.push(Dynamic::from(format!("{dest_norm}/{name}").replace('\\', "/")));
            }
            Ok(out)
        },
    );

    let base_write = base.clone();
    engine.register_fn(
        "write_text_file",
        move |path: &str, content: &str| -> Result<(), Box<EvalAltResult>> {
            let full = resolve_path(&base_write, path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| err_runtime(format!("创建目录失败: {e}")))?;
            }
            std::fs::write(&full, content)
                .map_err(|e| err_runtime(format!("写入文件失败: {e}")))?;
            Ok(())
        },
    );

    let base_read_text = base.clone();
    engine.register_fn(
        "read_text_file",
        move |path: &str| -> Result<String, Box<EvalAltResult>> {
            let full = resolve_path(&base_read_text, path);
            std::fs::read_to_string(&full)
                .map_err(|e| err_runtime(format!("读取文本失败 {}: {e}", full.display())))
        },
    );

    let base_exists = base.clone();
    engine.register_fn("path_exists", move |path: &str| -> bool {
        resolve_path(&base_exists, path).exists()
    });

    let base_size = base.clone();
    engine.register_fn(
        "file_size",
        move |path: &str| -> Result<i64, Box<EvalAltResult>> {
            let full = resolve_path(&base_size, path);
            let meta = std::fs::metadata(&full).map_err(|e| {
                err_runtime(format!("读取文件大小失败 {}: {e}", full.display()))
            })?;
            if !meta.is_file() {
                return Err(err_runtime(format!("不是文件: {}", full.display())));
            }
            Ok(meta.len() as i64)
        },
    );

    let base_mtime = base.clone();
    engine.register_fn("file_mtime_ymd", move |path: &str| -> String {
        let full = resolve_path(&base_mtime, path);
        let Ok(meta) = std::fs::metadata(&full) else {
            return String::new();
        };
        let Ok(modified) = meta.modified() else {
            return String::new();
        };
        let datetime: chrono::DateTime<chrono::Local> = modified.into();
        datetime.format("%Y-%m-%d").to_string()
    });

    let base_mtime_epoch = base.clone();
    engine.register_fn("file_mtime_epoch", move |path: &str| -> i64 {
        let full = resolve_path(&base_mtime_epoch, path);
        let Ok(meta) = std::fs::metadata(&full) else {
            return 0;
        };
        let Ok(modified) = meta.modified() else {
            return 0;
        };
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    });

    let base_mtime_text = base.clone();
    engine.register_fn("file_mtime_text", move |path: &str| -> String {
        let full = resolve_path(&base_mtime_text, path);
        let Ok(meta) = std::fs::metadata(&full) else {
            return String::new();
        };
        let Ok(modified) = meta.modified() else {
            return String::new();
        };
        let datetime: chrono::DateTime<chrono::Local> = modified.into();
        datetime.format("%Y-%m-%d %H:%M:%S").to_string()
    });

    engine.register_fn("parse_local_epoch", |text: &str| -> i64 {
        parse_local_epoch(text)
    });
    engine.register_fn("parse_int", |text: &str| -> i64 {
        text.trim().parse::<i64>().unwrap_or(0)
    });

    let base_dirs = base.clone();
    engine.register_fn(
        "list_child_dirs",
        move |path: &str| -> Result<rhai::Array, Box<EvalAltResult>> {
            let full = resolve_path(&base_dirs, path);
            if !full.is_dir() {
                return Err(err_runtime(format!("目录不存在: {}", full.display())));
            }
            let mut paths = Vec::new();
            for entry in std::fs::read_dir(&full)
                .map_err(|e| err_runtime(format!("读取目录失败: {e}")))?
            {
                let entry = entry.map_err(|e| err_runtime(format!("读取目录项失败: {e}")))?;
                if entry
                    .file_type()
                    .map(|t| t.is_dir())
                    .unwrap_or(false)
                {
                    paths.push(entry.path().to_string_lossy().replace('\\', "/"));
                }
            }
            paths.sort();
            let mut out = rhai::Array::with_capacity(paths.len());
            for p in paths {
                out.push(rhai::Dynamic::from(p));
            }
            Ok(out)
        },
    );

    // 递归列出匹配文件名正则的文件（绝对路径字符串数组，按路径排序）
    let base_walk = base;
    engine.register_fn(
        "walk_files",
        move |dir: &str, pattern: &str| -> Result<rhai::Array, Box<EvalAltResult>> {
            walk_files_impl(&base_walk, dir, pattern)
        },
    );

    engine.register_fn("wechat_img_cache_dir", || -> String {
        crate::paths::wechat_img_cache_dir()
            .to_string_lossy()
            .replace('\\', "/")
    });
}

fn register_local_ocr_fns(engine: &mut Engine, base: PathBuf) {
    let base_ocr = base.clone();
    engine.register_fn(
        "ocr_image",
        move |path: &str| -> Result<String, Box<EvalAltResult>> {
            let full = resolve_path(&base_ocr, path);
            local_ocr::ocr_image_file(&full).map_err(err_runtime)
        },
    );
    engine.register_fn("ocr_parse_meta", |text: &str| -> rhai::Map {
        local_ocr::parse_ocr_meta(text)
    });
    engine.register_fn(
        "format_process_duration",
        |discover: &str, fix: &str| -> String {
            local_ocr::format_process_duration(discover, fix)
        },
    );
    let base_cls = base.clone();
    engine.register_fn(
        "classify_sanitation_image",
        move |path: &str| -> Result<rhai::Map, Box<EvalAltResult>> {
            let full = resolve_path(&base_cls, path);
            sanitation_classify::classify_sanitation_image(&full)
        },
    );
    let base_cls2 = base.clone();
    engine.register_fn(
        "classify_sanitation_image",
        move |path: &str, ocr_text: &str| -> Result<rhai::Map, Box<EvalAltResult>> {
            let full = resolve_path(&base_cls2, path);
            sanitation_classify::classify_sanitation_with_fallback(&full, ocr_text)
        },
    );
    engine.register_fn(
        "pair_scene_images",
        |before: rhai::Array, after: rhai::Array| -> Result<rhai::Map, Box<EvalAltResult>> {
            scene_match::pair_scene_images(before, after)
        },
    );
    let base_score = base.clone();
    engine.register_fn(
        "score_scene_pair",
        move |before: rhai::Map, after: rhai::Map| -> Result<rhai::Map, Box<EvalAltResult>> {
            let mut b = before;
            let mut a = after;
            if let Some(p) = b.get("path").cloned().and_then(|v| v.into_string().ok()) {
                let full = resolve_path(&base_score, &p);
                b.insert(
                    "path".into(),
                    rhai::Dynamic::from(full.to_string_lossy().replace('\\', "/")),
                );
            }
            if let Some(p) = a.get("path").cloned().and_then(|v| v.into_string().ok()) {
                let full = resolve_path(&base_score, &p);
                a.insert(
                    "path".into(),
                    rhai::Dynamic::from(full.to_string_lossy().replace('\\', "/")),
                );
            }
            scene_match::score_scene_pair(b, a)
        },
    );
}

fn register_wechat_dat_fns(engine: &mut Engine, base: PathBuf) {
    engine.register_fn("wechat_resolve_image_key", wechat_dat::rhai_resolve_image_key);

    let base_dec = base.clone();
    engine.register_fn(
        "wechat_dat_decrypt",
        move |src: &str, dest: &str, keys: rhai::Map| -> Result<String, Box<EvalAltResult>> {
            let src_full = resolve_path(&base_dec, src);
            let dest_full = resolve_path(&base_dec, dest);
            let k = wechat_dat::keys_from_map(&keys).map_err(err_runtime)?;
            let out = wechat_dat::decrypt_dat_to_file(&src_full, &dest_full, &k)
                .map_err(err_runtime)?;
            Ok(out.to_string_lossy().replace('\\', "/"))
        },
    );

    // 两参数：自动从微信根目录解析密钥（src 须在 xwechat_files/<账号>/ 下）
    let base_auto = base;
    engine.register_fn(
        "wechat_dat_decrypt",
        move |src: &str, dest: &str| -> Result<String, Box<EvalAltResult>> {
            let src_full = resolve_path(&base_auto, src);
            let dest_full = resolve_path(&base_auto, dest);
            let wx_root = wechat_dat::infer_wx_root(&src_full).map_err(err_runtime)?;
            let keys = wechat_dat::resolve_keys_for_root(&wx_root).map_err(err_runtime)?;
            let out = wechat_dat::decrypt_dat_to_file(&src_full, &dest_full, &keys)
                .map_err(err_runtime)?;
            Ok(out.to_string_lossy().replace('\\', "/"))
        },
    );
}

fn register_http_json_fns(engine: &mut Engine, cancel: Option<Arc<AtomicBool>>) {
    // 通用
    engine.register_fn("http_request", http_json::http_request);
    engine.register_fn("http_exchange", http_json::http_exchange);
    engine.register_fn("http_headers", http_json::http_headers);
    // GET
    engine.register_fn("http_get", http_json::http_get);
    engine.register_fn("http_get", http_json::http_get_simple);
    // POST
    engine.register_fn("http_post", http_json::http_post);
    engine.register_fn("http_post", http_json::http_post_simple);
    // PUT / PATCH / DELETE
    engine.register_fn("http_put", http_json::http_put);
    engine.register_fn("http_put", http_json::http_put_simple);
    engine.register_fn("http_patch", http_json::http_patch);
    engine.register_fn("http_patch", http_json::http_patch_simple);
    engine.register_fn("http_delete", http_json::http_delete);
    engine.register_fn("http_delete", http_json::http_delete_headers);
    engine.register_fn("http_delete", http_json::http_delete_simple);
    // JSON / 日期 / 休眠
    engine.register_fn("json_parse", http_json::json_parse);
    engine.register_fn("json_stringify", http_json::json_stringify);
    engine.register_fn("today_ymd", http_json::today_ymd);
    engine.register_fn("ymd_cmp", http_json::ymd_cmp);
    engine.register_fn("now_hm", http_json::now_hm);
    engine.register_fn("now_hour", http_json::now_hour);
    engine.register_fn("now_minute", http_json::now_minute);
    {
        let cancel_i = cancel.clone();
        engine.register_fn("sleep_secs", move |secs: i64| -> Result<(), Box<EvalAltResult>> {
            http_json::sleep_secs_cancellable(secs, cancel_i.as_deref())
        });
        let cancel_f = cancel;
        engine.register_fn("sleep_secs", move |secs: f64| -> Result<(), Box<EvalAltResult>> {
            http_json::sleep_secs_cancellable(secs as i64, cancel_f.as_deref())
        });
    }
    engine.register_fn("rand_int", http_json::rand_int);
    engine.register_fn("rand_int", |lo: f64, hi: f64| -> Result<i64, Box<EvalAltResult>> {
        http_json::rand_int(lo as i64, hi as i64)
    });
    engine.register_fn("ymd_add_days", http_json::ymd_add_days);
    engine.register_fn("ymd_add_days", |ymd: &str, days: f64| -> Result<String, Box<EvalAltResult>> {
        http_json::ymd_add_days(ymd, days as i64)
    });
    engine.register_fn("ymd_weekday", http_json::ymd_weekday);
    engine.register_fn("ymd_weekday_cn", http_json::ymd_weekday_cn);
}

fn register_table_fns(engine: &mut Engine, base: PathBuf) {
    let base_read = base.clone();
    engine.register_fn("read_table", move |path: &str, _: Dynamic| -> Result<ScriptTable, Box<EvalAltResult>> {
        let full = resolve_path(&base_read, path);
        table::read_table(&full)
    });

    engine.register_fn("to_str", table::to_str);
    engine.register_fn("to_num", table::to_num);
    // Rhai 内置 s.trim() 是原地修改且返回 ()；提供返回新串的辅助，避免 AI 写成 let x = s.trim()
    engine.register_fn("str_trim", |s: &str| -> String { s.trim().to_string() });
    engine.register_fn("str_ends_with", |s: &str, suffix: &str| -> bool { s.ends_with(suffix) });
    engine.register_fn("str_to_lower", |s: &str| -> String { s.to_lowercase() });
    engine.register_fn("str_contains", |s: &str, sub: &str| -> bool { s.contains(sub) });

    engine.register_fn("parse_attendance_record_name", |filename: &str| -> Dynamic {
        let name = Path::new(filename)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(filename);
        let mut m = rhai::Map::new();
        if let Some(re) = regex::Regex::new(r"^(.+?)(\d{4}年\d{1,2}月)考勤记录表\.xlsx?$").ok() {
            if let Some(c) = re.captures(name) {
                m.insert("company".into(), Dynamic::from(c[1].to_string()));
                m.insert("month".into(), Dynamic::from(c[2].to_string()));
                m.insert("ok".into(), Dynamic::from(true));
                return Dynamic::from(m);
            }
        }
        m.insert("company".into(), Dynamic::from(""));
        m.insert("month".into(), Dynamic::from(""));
        m.insert("ok".into(), Dynamic::from(false));
        Dynamic::from(m)
    });
    engine.register_fn("parse_ot_summary_name", |filename: &str| -> Dynamic {
        let name = Path::new(filename)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(filename);
        let mut m = rhai::Map::new();
        if let Some(re) = regex::Regex::new(r"^(\d{4}年\d{1,2}月)(.+?)计划外加班工时汇总表\.xlsx?$").ok() {
            if let Some(c) = re.captures(name) {
                m.insert("month".into(), Dynamic::from(c[1].to_string()));
                m.insert("company".into(), Dynamic::from(c[2].to_string()));
                m.insert("ok".into(), Dynamic::from(true));
                return Dynamic::from(m);
            }
        }
        m.insert("company".into(), Dynamic::from(""));
        m.insert("month".into(), Dynamic::from(""));
        m.insert("ok".into(), Dynamic::from(false));
        Dynamic::from(m)
    });
    engine.register_fn("abs_num", |v: f64| -> f64 { v.abs() });
    engine.register_fn("abs_num", |v: i64| -> f64 { (v as f64).abs() });
    engine.register_fn("round2", |v: f64| -> f64 { (v * 100.0).round() / 100.0 });
}

fn register_workbook_fns(engine: &mut Engine, base: PathBuf) {
    let base_open = base.clone();
    engine.register_fn("open_workbook", move |path: &str| -> Result<ScriptWorkbook, Box<EvalAltResult>> {
        let full = resolve_path(&base_open, path);
        workbook::open_workbook(&full)
    });
    engine.register_fn("new_workbook", workbook::new_workbook);

    engine.register_fn(
        "wb_set_title",
        |wb: ScriptWorkbook, sheet: &str, title: &str| -> Result<(), Box<EvalAltResult>> {
            workbook::wb_set_title(&wb, sheet, title)
        },
    );
    engine.register_fn(
        "ws_set_cell",
        |wb: ScriptWorkbook, sheet: &str, row: i64, col: i64, value: Dynamic| -> Result<(), Box<EvalAltResult>> {
            workbook::ws_set_cell(&wb, sheet, row, col, value)
        },
    );
    let base_img = base.clone();
    engine.register_fn(
        "ws_add_image",
        move |wb: ScriptWorkbook,
              sheet: &str,
              row: i64,
              col: i64,
              path: &str|
              -> Result<(), Box<EvalAltResult>> {
            let full = resolve_path(&base_img, path);
            workbook::ws_add_image(&wb, sheet, row, col, &full, 220)
        },
    );
    let base_img2 = base.clone();
    engine.register_fn(
        "ws_add_image",
        move |wb: ScriptWorkbook,
              sheet: &str,
              row: i64,
              col: i64,
              path: &str,
              max_width_px: i64|
              -> Result<(), Box<EvalAltResult>> {
            let full = resolve_path(&base_img2, path);
            workbook::ws_add_image(&wb, sheet, row, col, &full, max_width_px)
        },
    );
    engine.register_fn(
        "ws_set_row_height",
        |wb: ScriptWorkbook, sheet: &str, row: i64, height: f64| -> Result<(), Box<EvalAltResult>> {
            workbook::ws_set_row_height(&wb, sheet, row, height)
        },
    );
    engine.register_fn(
        "ws_set_row_height",
        |wb: ScriptWorkbook, sheet: &str, row: i64, height: i64| -> Result<(), Box<EvalAltResult>> {
            workbook::ws_set_row_height(&wb, sheet, row, height as f64)
        },
    );
    engine.register_fn(
        "ws_get_cell",
        |wb: ScriptWorkbook, sheet: &str, row: i64, col: i64| -> Result<Dynamic, Box<EvalAltResult>> {
            workbook::ws_get_cell(&wb, sheet, row, col)
        },
    );
    engine.register_fn(
        "ws_insert_rows",
        |wb: ScriptWorkbook, sheet: &str, row: i64, count: i64| -> Result<(), Box<EvalAltResult>> {
            workbook::ws_insert_rows(&wb, sheet, row, count)
        },
    );
    engine.register_fn(
        "ws_remove_columns",
        |wb: ScriptWorkbook, sheet: &str, col: i64, count: i64| -> Result<(), Box<EvalAltResult>> {
            workbook::ws_remove_columns(&wb, sheet, col, count)
        },
    );
    engine.register_fn(
        "ws_copy_row_style",
        |wb: ScriptWorkbook, sheet: &str, src: i64, dst: i64, max_col: i64| -> Result<(), Box<EvalAltResult>> {
            workbook::ws_copy_row_style(&wb, sheet, src, dst, max_col)
        },
    );
    engine.register_fn(
        "ws_set_formula",
        |wb: ScriptWorkbook, sheet: &str, row: i64, col: i64, formula: &str| -> Result<(), Box<EvalAltResult>> {
            workbook::ws_set_formula(&wb, sheet, row, col, formula)
        },
    );
    engine.register_fn(
        "ws_set_date",
        |wb: ScriptWorkbook,
         sheet: &str,
         row: i64,
         col: i64,
         year: i64,
         month: i64,
         day: i64|
         -> Result<(), Box<EvalAltResult>> {
            workbook::ws_set_date(&wb, sheet, row, col, year, month, day)
        },
    );
    engine.register_fn(
        "ws_set_date",
        |wb: ScriptWorkbook,
         sheet: &str,
         row: i64,
         col: i64,
         year: f64,
         month: f64,
         day: f64|
         -> Result<(), Box<EvalAltResult>> {
            workbook::ws_set_date(
                &wb,
                sheet,
                row,
                col,
                year as i64,
                month as i64,
                day as i64,
            )
        },
    );
    // Rhai 常把 to_num 结果当 f64、字面量 1 / 循环变量当 i64，补齐混合签名
    engine.register_fn(
        "ws_set_date",
        |wb: ScriptWorkbook,
         sheet: &str,
         row: i64,
         col: i64,
         year: f64,
         month: f64,
         day: i64|
         -> Result<(), Box<EvalAltResult>> {
            workbook::ws_set_date(&wb, sheet, row, col, year as i64, month as i64, day)
        },
    );
    engine.register_fn(
        "ws_set_date",
        |wb: ScriptWorkbook,
         sheet: &str,
         row: i64,
         col: i64,
         year: i64,
         month: i64,
         day: f64|
         -> Result<(), Box<EvalAltResult>> {
            workbook::ws_set_date(&wb, sheet, row, col, year, month, day as i64)
        },
    );
    engine.register_fn("excel_date_serial", workbook::excel_date_serial);
    engine.register_fn(
        "excel_date_serial",
        |year: f64, month: f64, day: f64| -> Result<f64, Box<EvalAltResult>> {
            workbook::excel_date_serial(year as i64, month as i64, day as i64)
        },
    );
    engine.register_fn("excel_serial_ymd", workbook::excel_serial_ymd);
    engine.register_fn("days_in_month", workbook::days_in_month);
    engine.register_fn("days_in_month", |year: f64, month: f64| -> Result<i64, Box<EvalAltResult>> {
        workbook::days_in_month(year as i64, month as i64)
    });
    let base_save = base;
    engine.register_fn(
        "wb_save",
        move |wb: ScriptWorkbook, path: &str| -> Result<(), Box<EvalAltResult>> {
            let full = resolve_path(&base_save, path);
            workbook::wb_save(&wb, &full)
        },
    );
    engine.register_fn("wb_sheet_names", |wb: ScriptWorkbook| -> rhai::Array {
        workbook::wb_sheet_names(&wb)
            .into_iter()
            .map(Dynamic::from)
            .collect()
    });
    engine.register_fn(
        "wb_new_sheet",
        |wb: ScriptWorkbook, sheet: &str| -> Result<(), Box<EvalAltResult>> {
            workbook::wb_new_sheet(&wb, sheet)
        },
    );
    engine.register_fn(
        "wb_copy_sheet",
        |wb: ScriptWorkbook, src: &str, dst: &str| -> Result<(), Box<EvalAltResult>> {
            workbook::wb_copy_sheet(&wb, src, dst)
        },
    );
    engine.register_fn(
        "ws_clear_images",
        |wb: ScriptWorkbook, sheet: &str| -> Result<(), Box<EvalAltResult>> {
            workbook::ws_clear_images(&wb, sheet)
        },
    );
    engine.register_fn(
        "ws_clear_range",
        |wb: ScriptWorkbook,
         sheet: &str,
         start_row: i64,
         end_row: i64,
         start_col: i64,
         end_col: i64|
         -> Result<(), Box<EvalAltResult>> {
            workbook::ws_clear_range(&wb, sheet, start_row, end_row, start_col, end_col)
        },
    );
}

fn find_file_impl(
    base: &Path,
    subdir: &str,
    pattern: &str,
    exclude: Option<&str>,
) -> Result<String, Box<EvalAltResult>> {
    find_file_match_impl(base, subdir, pattern, exclude)?.ok_or_else(|| {
        err_runtime(format!("在 {subdir} 中未找到匹配 {pattern} 的文件"))
    })
}

fn find_file_optional_impl(
    base: &Path,
    subdir: &str,
    pattern: &str,
    exclude: Option<&str>,
) -> Result<String, Box<EvalAltResult>> {
    match find_file_match_impl(base, subdir, pattern, exclude) {
        Ok(Some(p)) => Ok(p),
        Ok(None) => Ok(String::new()),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("目录不存在") {
                Ok(String::new())
            } else {
                Err(e)
            }
        }
    }
}

fn find_file_match_impl(
    base: &Path,
    subdir: &str,
    pattern: &str,
    exclude: Option<&str>,
) -> Result<Option<String>, Box<EvalAltResult>> {
    use excel_compat::name_matches_excel_pattern;
    // downloads / files 互通：手工附件在 files/，浏览器下载在 downloads/，脚本两侧都能找到
    let search_dirs: Vec<&str> = match subdir {
        "downloads" => vec!["downloads", "files"],
        "files" => vec!["files", "downloads"],
        _ => vec![subdir],
    };

    let re = regex::Regex::new(pattern).map_err(|e| err_runtime(format!("正则无效: {e}")))?;
    let mut matches = Vec::new();
    let mut any_dir = false;
    for sub in &search_dirs {
        let dir = if *sub == "." {
            base.to_path_buf()
        } else {
            resolve_path(base, sub)
        };
        if !dir.is_dir() {
            continue;
        }
        any_dir = true;
        for entry in std::fs::read_dir(&dir).map_err(|e| err_runtime(format!("读取目录失败: {e}")))? {
            let entry = entry.map_err(|e| err_runtime(format!("读取目录项失败: {e}")))?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if let Some(ex) = exclude {
                if name.contains(ex) {
                    continue;
                }
            }
            if name_matches_excel_pattern(&re, name) {
                matches.push((path, entry.metadata().ok().and_then(|m| m.modified().ok())));
            }
        }
    }
    if !any_dir {
        let primary = if subdir == "." {
            base.to_path_buf()
        } else {
            resolve_path(base, subdir)
        };
        return Err(err_runtime(format!("目录不存在: {}", primary.display())));
    }
    matches.sort_by_key(|(_, mtime)| mtime.unwrap_or(std::time::UNIX_EPOCH));
    Ok(matches
        .last()
        .map(|(p, _)| {
            if subdir == "." {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string()
            } else {
                path_to_rel(base, p)
            }
        }))
}

fn resolve_path(base: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    // 按 / 与 \ 分段 join，避免 Windows 上留下 `files/a.xlsx` 与 `files\a.xlsx` 两种字符串
    let mut out = base.to_path_buf();
    for part in path.split(['/', '\\']).filter(|s| !s.is_empty() && *s != ".") {
        out.push(part);
    }
    out
}

fn walk_files_impl(
    base: &Path,
    dir: &str,
    pattern: &str,
) -> Result<rhai::Array, Box<EvalAltResult>> {
    let root = resolve_path(base, dir);
    if !root.is_dir() {
        return Err(err_runtime(format!("目录不存在: {}", root.display())));
    }
    let re = regex::Regex::new(pattern).map_err(|e| err_runtime(format!("正则无效: {e}")))?;
    let mut paths = Vec::new();
    walk_files_collect(&root, &re, &mut paths)?;
    paths.sort();
    let mut out = rhai::Array::with_capacity(paths.len());
    for p in paths {
        out.push(Dynamic::from(p));
    }
    Ok(out)
}

fn walk_files_collect(
    dir: &Path,
    re: &regex::Regex,
    out: &mut Vec<String>,
) -> Result<(), Box<EvalAltResult>> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| err_runtime(format!("读取目录失败 {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| err_runtime(format!("读取目录项失败: {e}")))?;
        let path = entry.path();
        let ft = entry
            .file_type()
            .map_err(|e| err_runtime(format!("读取文件类型失败: {e}")))?;
        if ft.is_dir() {
            walk_files_collect(&path, re, out)?;
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if re.is_match(name) {
            // 统一正斜杠，便于脚本里 contains / 拼接；resolve_path 两侧都认
            out.push(path.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

fn path_to_rel(base: &Path, full: &Path) -> String {
    full.strip_prefix(base)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| full.to_string_lossy().into_owned())
}

fn err_runtime(msg: String) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(msg.into(), rhai::Position::NONE))
}

/// 本地时间 → Unix 秒。接受 `2026-09-21 15:40`、`2026-09-21 15:40:00`、`2026.09.21 15:40`。失败返回 -1。
fn parse_local_epoch(text: &str) -> i64 {
    let t = text.trim().replace(['.', '/'], "-");
    let fmts = [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ];
    for f in fmts {
        let Ok(naive) = chrono::NaiveDateTime::parse_from_str(&t, f) else {
            continue;
        };
        if let Some(dt) = naive.and_local_timezone(chrono::Local).single() {
            return dt.timestamp();
        }
    }
    -1
}

pub const FILL_ATTENDANCE_SCRIPT: &str = include_str!("../../data/rhai/fill-attendance.rhai");
pub const COMPARE_ATTENDANCE_SCRIPT: &str = include_str!("../../data/rhai/compare-attendance.rhai");
pub const TENNIS_VENUE_SCRIPT: &str = include_str!("../../data/rhai/tennis-venue-slots.rhai");
pub const WATCH_WECHAT_IMAGES_SCRIPT: &str =
    include_str!("../../data/rhai/watch-wechat-images.rhai");
pub const OCR_ROAD_CLEAN_CLASSIFY_SCRIPT: &str =
    include_str!("../../data/rhai/ocr-road-clean-classify.rhai");
pub const SCENE_PAIR_SCORE_SCRIPT: &str = include_str!("../../data/rhai/scene-pair-score.rhai");

#[cfg(test)]
mod builtin_script_tests {
    use super::*;

    #[test]
    fn attendance_builtin_scripts_have_valid_rhai_syntax() {
        let mut engine = Engine::new();
        engine.set_max_expr_depths(128, 64);
        engine
            .compile(FILL_ATTENDANCE_SCRIPT)
            .expect("fill-attendance.rhai should compile");
        engine
            .compile(COMPARE_ATTENDANCE_SCRIPT)
            .expect("compare-attendance.rhai should compile");
        engine
            .compile(TENNIS_VENUE_SCRIPT)
            .expect("tennis-venue-slots.rhai should compile");
        engine
            .compile(WATCH_WECHAT_IMAGES_SCRIPT)
            .expect("watch-wechat-images.rhai should compile");
        engine
            .compile(OCR_ROAD_CLEAN_CLASSIFY_SCRIPT)
            .expect("ocr-road-clean-classify.rhai should compile");
        engine
            .compile(SCENE_PAIR_SCORE_SCRIPT)
            .expect("scene-pair-score.rhai should compile");
        engine
            .compile(include_str!("../../data/rhai/find-wechat-group.rhai"))
            .expect("find-wechat-group.rhai should compile");
    }

    #[test]
    fn find_file_searches_downloads_and_files() {
        use std::fs;
        let run_dir = std::env::temp_dir().join(format!(
            "webclaw-find-file-dual-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&run_dir);
        fs::create_dir_all(run_dir.join("files")).unwrap();
        fs::write(run_dir.join("files").join("考勤月报_测试.xlsx"), b"x").unwrap();

        let found = find_file_match_impl(
            &run_dir,
            "downloads",
            r"^考勤月报_.*\.xlsx$",
            None,
        )
        .unwrap()
        .expect("should find in files/ when searching downloads");
        assert!(found.contains("考勤月报_测试.xlsx"));
        assert!(found.starts_with("files/"));

        let _ = fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn walk_files_recursive_and_file_size() {
        use std::fs;
        let run_dir = std::env::temp_dir().join(format!(
            "webclaw-walk-files-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&run_dir);
        let nested = run_dir.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("pic.JPG"), b"12345").unwrap();
        fs::write(nested.join("skip.txt"), b"no").unwrap();

        let files = walk_files_impl(&run_dir, run_dir.to_str().unwrap(), r"(?i)\.jpg$").unwrap();
        assert_eq!(files.len(), 1);
        let p = files[0].clone().into_string().unwrap();
        assert!(p.replace('\\', "/").ends_with("a/b/pic.JPG") || p.ends_with("pic.JPG"));

        let abs = nested.join("pic.JPG");
        let sz = {
            let meta = fs::metadata(&abs).unwrap();
            meta.len() as i64
        };
        assert_eq!(sz, 5);

        let _ = fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn watch_wechat_images_once_builds_baseline() {
        use std::fs;
        let run_dir = std::env::temp_dir().join(format!(
            "webclaw-wechat-watch-run-{}",
            std::process::id()
        ));
        let wx_root = std::env::temp_dir().join(format!(
            "webclaw-wechat-watch-data-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&run_dir);
        let _ = fs::remove_dir_all(&wx_root);
        fs::create_dir_all(run_dir.join("files")).unwrap();
        // 伪造最小 V2 布局目录；无真实 .dat 时基线为 0，仍应 once 正常结束
        let img_dir = wx_root
            .join("msg")
            .join("attach")
            .join("abc")
            .join("2026-09")
            .join("Img");
        fs::create_dir_all(&img_dir).unwrap();
        fs::create_dir_all(wx_root.join("cache")).unwrap();

        // 预置密钥，避免依赖本机微信 kvcomm
        let key_json = r#"{"aes_key":"a657a64124075eb3","xor_key":128,"account":"Lonk1989","code":574066560}"#;
        fs::write(run_dir.join("files").join("wechat-image-key.json"), key_json).unwrap();

        let wx = wx_root.to_string_lossy().replace('\\', "/");
        let args = format!("{wx} once");
        let templates = std::env::temp_dir().join("webclaw-wechat-watch-tpl");
        let _ = fs::create_dir_all(&templates);
        let result = run_script(WATCH_WECHAT_IMAGES_SCRIPT, &run_dir, &templates, &args, None)
            .expect("watch script once should run");
        let logs = result.logs.join("\n");
        println!("{logs}");
        assert!(
            logs.contains("基线") || logs.contains("once 模式结束"),
            "unexpected logs:\n{logs}"
        );
        let seen = run_dir.join("files").join("wechat-image-seen.json");
        assert!(seen.is_file(), "seen state missing");

        let _ = fs::remove_dir_all(&run_dir);
        let _ = fs::remove_dir_all(&wx_root);
    }

    #[test]
    fn find_file_xlsx_pattern_matches_xls() {
        use std::fs;
        let run_dir = std::env::temp_dir().join(format!(
            "webclaw-find-xls-alias-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&run_dir);
        fs::create_dir_all(run_dir.join("downloads")).unwrap();
        fs::write(run_dir.join("downloads").join("考勤月报_测试.xls"), b"x").unwrap();

        let found = find_file_match_impl(
            &run_dir,
            "downloads",
            r"^考勤月报_.*\.xlsx$",
            None,
        )
        .unwrap()
        .expect("xlsx pattern should find .xls");
        assert!(found.ends_with("考勤月报_测试.xls"), "found={found}");

        let _ = fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn example_student_score_scripts_run() {
        use std::fs;
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let templates = manifest.join("data/templates");
        assert!(
            templates.join("学生各科分数.xlsx").is_file(),
            "missing student scores template"
        );
        let run_dir = std::env::temp_dir().join(format!(
            "webclaw-ex-scores-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&run_dir);
        fs::create_dir_all(run_dir.join("files")).unwrap();

        let copy = include_str!("../../../examples/rhai/02-copy-scores.rhai");
        let preview = include_str!("../../../examples/rhai/03-preview-scores.rhai");
        let calc = include_str!("../../../examples/rhai/04-calc-total.rhai");
        let pass = include_str!("../../../examples/rhai/05-filter-pass.rhai");

        run_script(copy, &run_dir, &templates, "", None).expect("copy");
        assert!(run_dir.join("files").join("学生各科分数.xlsx").is_file());

        let prev = run_script(preview, &run_dir, &templates, "东湖班", None).expect("preview");
        let logs = prev.logs.join("\n");
        assert!(logs.contains("青禾"), "{logs}");

        let calc_res = run_script(calc, &run_dir, &templates, "", None).expect("calc");
        assert!(
            run_dir.join("files").join("学生成绩-已核算.xlsx").is_file(),
            "calc output missing; logs={}",
            calc_res.logs.join("\n")
        );

        let pass_res = run_script(pass, &run_dir, &templates, "60 东湖班", None).expect("pass");
        let pass_logs = pass_res.logs.join("\n");
        assert!(
            run_dir.join("files").join("及格名单.txt").is_file(),
            "{pass_logs}"
        );

        let _ = fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn example_http_get_script_runs() {
        use std::fs;
        let run_dir = std::env::temp_dir().join(format!(
            "webclaw-ex-http-get-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&run_dir);
        fs::create_dir_all(run_dir.join("files")).unwrap();
        let templates = std::env::temp_dir().join("webclaw-empty-templates");
        let _ = fs::create_dir_all(&templates);

        let src = include_str!("../../../examples/rhai/06-http-get.rhai");
        let res = match run_script(src, &run_dir, &templates, "", None) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("example http get skipped: {e}");
                let _ = fs::remove_dir_all(&run_dir);
                return;
            }
        };
        let logs = res.logs.join("\n");
        assert!(
            run_dir.join("files").join("http-get-demo.json").is_file(),
            "{logs}"
        );
        assert!(logs.contains("title=") || logs.contains("✓"), "{logs}");
        let _ = fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn tennis_venue_script_fetches_and_exports() {
        use std::fs;
        let run_dir = std::env::temp_dir().join(format!(
            "webclaw-tennis-venue-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&run_dir);
        fs::create_dir_all(run_dir.join("files")).unwrap();
        let templates = std::env::temp_dir().join("webclaw-empty-templates");
        let _ = fs::create_dir_all(&templates);

        let res = run_script(
            TENNIS_VENUE_SCRIPT,
            &run_dir,
            &templates,
            "once",
            None,
        );
        let res = match res {
            Ok(r) => r,
            Err(e) => {
                // 访客令牌过期、静默时段或外网不可达时跳过，避免拖垮本地单测
                eprintln!("tennis venue script skipped: {e}");
                let _ = fs::remove_dir_all(&run_dir);
                return;
            }
        };
        let logs = res.logs.join("\n");
        assert!(
            logs.contains("监控") || logs.contains("可约") || logs.contains("静默"),
            "{logs}"
        );
        let _ = fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn split_script_args_whitespace() {
        assert_eq!(
            split_script_args("山西阳泉 2026-06"),
            vec!["山西阳泉".to_string(), "2026-06".to_string()]
        );
        assert_eq!(split_script_args("  a\tb  c  "), vec!["a", "b", "c"]);
        assert!(split_script_args("").is_empty());
        assert!(split_script_args("   ").is_empty());
    }

    #[test]
    fn arg_helpers_expose_tokens() {
        use std::fs;
        let run_dir = std::env::temp_dir().join(format!(
            "webclaw-arg-api-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&run_dir);
        fs::create_dir_all(&run_dir).unwrap();
        let templates = run_dir.join("templates");
        fs::create_dir_all(&templates).unwrap();

        let src = r#"
print("raw=" + ARGS);
print("n=" + arg_count());
print("0=" + arg(0));
print("1=" + arg(1));
print("2=" + arg(2, "缺省"));
let all = args();
print("len=" + all.len());
print("req0=" + require_arg(0, "用法：脚本测试 公司 月份"));
"#;
        let result = run_script(src, &run_dir, &templates, "山西阳泉 2026-06", None)
            .expect("arg api script");
        let logs = result.logs.join("\n");
        assert!(logs.contains("raw=山西阳泉 2026-06"), "{logs}");
        assert!(logs.contains("n=2"), "{logs}");
        assert!(logs.contains("0=山西阳泉"), "{logs}");
        assert!(logs.contains("1=2026-06"), "{logs}");
        assert!(logs.contains("2=缺省"), "{logs}");
        assert!(logs.contains("len=2"), "{logs}");
        assert!(logs.contains("req0=山西阳泉"), "{logs}");

        let missing = run_script(
            r#"require_arg(1, "用法：脚本测试 公司");"#,
            &run_dir,
            &templates,
            "仅公司",
            None,
        );
        let err = match missing {
            Ok(_) => panic!("expected require_arg to fail"),
            Err(e) => e,
        };
        assert!(err.contains("缺少参数"), "{err}");
        assert!(err.contains("用法：脚本测试 公司"), "{err}");

        let _ = fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn str_trim_returns_new_string_unlike_builtin_trim() {
        use std::fs;
        let run_dir = std::env::temp_dir().join(format!(
            "webclaw-str-trim-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&run_dir);
        fs::create_dir_all(&run_dir).unwrap();
        let templates = run_dir.join("templates");
        fs::create_dir_all(&templates).unwrap();

        let ok = run_script(
            r#"
let a = str_trim("  9月  ");
print("a=" + a);
print("len=" + a.len());
print("end=" + str_ends_with(a, "月"));
let b = "  8月  ";
b.trim();
print("b=" + b);
"#,
            &run_dir,
            &templates,
            "",
            None,
        )
        .expect("str_trim script");
        let logs = ok.logs.join("\n");
        assert!(logs.contains("a=9月"), "{logs}");
        assert!(logs.contains("len=2"), "{logs}");
        assert!(logs.contains("end=true"), "{logs}");
        assert!(logs.contains("b=8月"), "{logs}");

        let bad = run_script(
            r#"
let x = "  x  ".trim();
print(x.len());
"#,
            &run_dir,
            &templates,
            "",
            None,
        );
        let err = match bad {
            Ok(_) => panic!("expected builtin trim assign to fail on .len()"),
            Err(e) => e,
        };
        assert!(
            err.contains("len") || err.contains("()"),
            "unexpected err: {err}"
        );

        let _ = fs::remove_dir_all(&run_dir);
    }

    /// 用 data/excel 中的现网样本手动验证合并考勤耗时：
    /// cargo test benchmark_production_attendance_scripts --lib -- --ignored --nocapture
    #[test]
    #[ignore]
    fn benchmark_production_attendance_scripts() {
        use std::fs;
        use std::time::Instant;

        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let source_dir = manifest.join("data/excel");
        let templates_dir = manifest.join("data/templates");
        let run_dir = std::env::temp_dir().join(format!(
            "webclaw-attendance-bench-{}",
            std::process::id()
        ));
        let downloads = run_dir.join("downloads");
        fs::create_dir_all(&downloads).unwrap();
        fs::create_dir_all(run_dir.join("files")).unwrap();
        for entry in fs::read_dir(&source_dir).unwrap() {
            let entry = entry.unwrap();
            if entry.path().is_file() {
                fs::copy(entry.path(), downloads.join(entry.file_name())).unwrap();
            }
        }

        let started = Instant::now();
        let merged = run_script(
            FILL_ATTENDANCE_SCRIPT,
            &run_dir,
            &templates_dir,
            "山西阳泉 2026-06",
            None,
        )
        .expect("merge attendance should run");
        let merge_elapsed = started.elapsed();
        println!(
            "merge elapsed={merge_elapsed:?}\n{}",
            merged.logs.join("\n")
        );

        let _ = fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn compare_attendance_against_sample_zip() {
        use std::fs;

        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let zip_src = manifest.join("data/zip/加班工时申请-陈倩-2026-07-02_614220260807111314.zip");
        if !zip_src.is_file() {
            eprintln!("skip: sample zip missing");
            return;
        }
        let templates_dir = manifest.join("data/templates");
        let run_dir = std::env::temp_dir().join(format!(
            "webclaw-compare-att-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&run_dir);
        fs::create_dir_all(run_dir.join("downloads")).unwrap();
        fs::create_dir_all(run_dir.join("files")).unwrap();
        fs::copy(
            &zip_src,
            run_dir
                .join("downloads")
                .join("加班工时申请-陈倩-2026-07-02_614220260807111314.zip"),
        )
        .unwrap();

        let result = run_script(COMPARE_ATTENDANCE_SCRIPT, &run_dir, &templates_dir, "陈倩", None)
            .expect("compare attendance should run");
        let logs = result.logs.join("\n");
        println!("{logs}");
        assert!(
            logs.contains("全部通过") || logs.contains("通过 2"),
            "expected both companies to pass, logs:\n{logs}"
        );
        let opinion = run_dir.join("files").join("考勤数据对比审核意见.txt");
        assert!(opinion.is_file(), "opinion file missing");
        let text = fs::read_to_string(&opinion).unwrap();
        assert!(text.contains("通过"), "opinion:\n{text}");

        let _ = fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn run_script_cancel_interrupts_sleep() {
        use std::fs;
        use std::time::Instant;
        let run_dir = std::env::temp_dir().join(format!(
            "webclaw-rhai-cancel-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&run_dir);
        fs::create_dir_all(&run_dir).unwrap();
        let templates = std::env::temp_dir().join("webclaw-empty-templates");
        let _ = fs::create_dir_all(&templates);

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_bg = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            cancel_bg.store(true, Ordering::SeqCst);
        });

        let t0 = Instant::now();
        let err = match run_script_cancel(
            "sleep_secs(30);\nprint(\"should-not-reach\");\n",
            &run_dir,
            &templates,
            "",
            None,
            Some(cancel),
            None,
        ) {
            Ok(_) => panic!("should cancel"),
            Err(e) => e,
        };
        assert!(err.contains("已取消"), "unexpected err={err}");
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(5),
            "cancel too slow: {:?}",
            t0.elapsed()
        );
        let _ = fs::remove_dir_all(&run_dir);
    }

    #[test]
    fn run_script_cancel_interrupts_busy_loop() {
        use std::fs;
        use std::time::Instant;
        let run_dir = std::env::temp_dir().join(format!(
            "webclaw-rhai-cancel-loop-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&run_dir);
        fs::create_dir_all(&run_dir).unwrap();
        let templates = std::env::temp_dir().join("webclaw-empty-templates");
        let _ = fs::create_dir_all(&templates);

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_bg = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(80));
            cancel_bg.store(true, Ordering::SeqCst);
        });

        let t0 = Instant::now();
        let err = match run_script_cancel(
            "let i = 0;\nwhile true { i += 1; }\n",
            &run_dir,
            &templates,
            "",
            None,
            Some(cancel),
            None,
        ) {
            Ok(_) => panic!("should cancel"),
            Err(e) => e,
        };
        assert!(err.contains("已取消"), "unexpected err={err}");
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(3),
            "cancel too slow: {:?}",
            t0.elapsed()
        );
        let _ = fs::remove_dir_all(&run_dir);
    }
}
