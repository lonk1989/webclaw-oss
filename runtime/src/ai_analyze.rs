//! 「AI」指令：基于用户提问，分析本次运行产物中的文本/表格/Word 文件。

use calamine::{open_workbook_auto, Data, Reader};
use chrono::Local;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use zip::ZipArchive;

use crate::ark::{self, TokenUsage};
use crate::run_artifacts::RunSession;

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PER_FILE_CHARS: usize = 40_000;
const MAX_TOTAL_CHARS: usize = 120_000;
const MAX_EXCEL_ROWS: usize = 200;

const TEXT_EXT: &[&str] = &["md", "log", "txt", "csv"];
const EXCEL_EXT: &[&str] = &["xlsx", "xls", "xlsm"];
const WORD_EXT: &[&str] = &["docx"];

const MEDIA_EXT: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "ico", "tif", "tiff", "heic", "heif", "mp3",
    "mp4", "wav", "avi", "mov", "mkv", "flv", "wmv", "m4a", "aac", "ogg", "webm",
];

const SYSTEM_PROMPT_ANALYZE: &str = r#"你是浏览器自动化任务的执行结果分析助手。用户会提供本次运行产生的文本、日志、Markdown、表格或 Word 文档内容，请根据用户问题进行分析、总结或回答。

要求：
- 用中文回答，结构清晰（可用 markdown 标题、列表、表格）
- 只基于提供的文件内容作答，不要编造未出现的数据
- 若内容不足以回答，明确说明缺少什么信息
- 本次未提供图片或音视频，不要假设或描述它们
- Excel/CSV 已转为文本表格，可直接引用具体行/列
"#;

#[derive(Debug, Clone)]
struct SourceFile {
    rel_path: String,
    kind: &'static str,
}

#[derive(Debug)]
struct ExtractedFile {
    rel_path: String,
    kind: &'static str,
    text: String,
    truncated: bool,
}

fn ext_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
}

fn is_media_ext(ext: &str) -> bool {
    MEDIA_EXT.contains(&ext)
}

fn classify_ext(ext: &str) -> Option<&'static str> {
    if TEXT_EXT.contains(&ext) {
        Some("text")
    } else if EXCEL_EXT.contains(&ext) {
        Some("excel")
    } else if WORD_EXT.contains(&ext) {
        Some("word")
    } else {
        None
    }
}

fn should_skip_file(_rel: &str, name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower == "readme.md" {
        return true;
    }
    lower.starts_with("ai分析") || lower.starts_with("ai-")
}

fn collect_analyzable_files(run_dir: &Path) -> Vec<SourceFile> {
    let subs = ["documents", "downloads", "files", "executions"];
    let mut out = Vec::new();
    for sub in subs {
        let dir = run_dir.join(sub);
        if !dir.is_dir() {
            continue;
        }
        let mut entries = Vec::new();
        collect_files_recursive(&dir, run_dir, &mut entries);
        entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        out.extend(entries);
    }
    out
}

fn collect_files_recursive(dir: &Path, run_dir: &Path, out: &mut Vec<SourceFile>) {
    let read_dir = match fs::read_dir(dir) {
        Ok(d) => d,
        Err(_) => return,
    };
    for ent in read_dir.flatten() {
        let path = ent.path();
        if path.is_dir() {
            collect_files_recursive(&path, run_dir, out);
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(ext) = ext_lower(&path) else {
            continue;
        };
        if is_media_ext(&ext) {
            continue;
        }
        let Some(kind) = classify_ext(&ext) else {
            continue;
        };
        let rel = path
            .strip_prefix(run_dir)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| name.to_string());
        if should_skip_file(&rel, name) {
            continue;
        }
        out.push(SourceFile {
            rel_path: rel,
            kind,
        });
    }
}

fn truncate_chars(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        return (s.to_string(), false);
    }
    let cut: String = s.chars().take(max).collect();
    (format!("{cut}\n…（已截断，原文共 {} 字）", s.chars().count()), true)
}

fn read_text_file(path: &Path) -> Result<String, String> {
    let meta = fs::metadata(path).map_err(|e| format!("读取失败 {}: {e}", path.display()))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "文件过大（{} MB），请缩小后重试",
            meta.len() / 1024 / 1024
        ));
    }
    fs::read_to_string(path).map_err(|e| format!("读取失败 {}: {e}", path.display()))
}

fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(s) => s.replace('\r', "").replace('\n', " "),
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

fn excel_to_text(path: &Path) -> Result<String, String> {
    let mut workbook = open_workbook_auto(path)
        .map_err(|e| format!("读取表格失败 {}: {e}", path.display()))?;
    let names = workbook.sheet_names().to_vec();
    if names.is_empty() {
        return Err("表格无工作表".into());
    }
    let mut parts = Vec::new();
    for (si, sheet_name) in names.iter().enumerate().take(3) {
        let range = workbook
            .worksheet_range(sheet_name)
            .map_err(|e| format!("读取工作表 {sheet_name} 失败: {e}"))?;
        let mut rows_out = Vec::new();
        for (ri, row) in range.rows().enumerate() {
            if ri >= MAX_EXCEL_ROWS {
                rows_out.push(format!(
                    "…（已截断，仅展示前 {MAX_EXCEL_ROWS} 行；工作表「{sheet_name}」）"
                ));
                break;
            }
            let cells: Vec<String> = row.iter().map(cell_to_string).collect();
            if cells.iter().any(|c| !c.is_empty()) {
                rows_out.push(cells.join("\t"));
            }
        }
        if rows_out.is_empty() {
            continue;
        }
        let header = if names.len() > 1 {
            format!("### 工作表: {sheet_name}\n")
        } else {
            String::new()
        };
        parts.push(format!("{header}{}", rows_out.join("\n")));
        if si + 1 >= 3 && names.len() > 3 {
            parts.push(format!("…（另有 {} 个工作表未展开）", names.len() - 3));
            break;
        }
    }
    if parts.is_empty() {
        return Err("表格为空".into());
    }
    Ok(parts.join("\n\n"))
}

fn strip_xml_text(xml: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in xml.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace('\r', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn docx_to_text(path: &Path) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|e| format!("打开 Word 失败: {e}"))?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("解析 docx 失败: {e}"))?;
    let mut doc = archive
        .by_name("word/document.xml")
        .map_err(|_| "不是有效的 .docx 文件".to_string())?;
    let mut xml = String::new();
    doc.read_to_string(&mut xml)
        .map_err(|e| format!("读取 docx 内容失败: {e}"))?;
    let text = strip_xml_text(&xml);
    if text.trim().is_empty() {
        return Err("Word 文档无文本内容".into());
    }
    Ok(text)
}

fn extract_file(run_dir: &Path, src: &SourceFile) -> Result<ExtractedFile, String> {
    let path = run_dir.join(&src.rel_path);
    if !path.is_file() {
        return Err(format!("文件不存在: {}", src.rel_path));
    }
    let raw = match src.kind {
        "text" => read_text_file(&path)?,
        "excel" => excel_to_text(&path)?,
        "word" => docx_to_text(&path)?,
        _ => return Err(format!("不支持的类型: {}", src.rel_path)),
    };
    let (text, truncated) = truncate_chars(&raw, MAX_PER_FILE_CHARS);
    Ok(ExtractedFile {
        rel_path: src.rel_path.clone(),
        kind: src.kind,
        text,
        truncated,
    })
}

fn build_context(run_dir: &Path, files: &[SourceFile]) -> Result<(String, Vec<String>), String> {
    let mut parts = Vec::new();
    let mut used_names = Vec::new();
    let mut total = 0usize;
    let mut skipped = Vec::new();

    for src in files {
        match extract_file(run_dir, src) {
            Ok(ex) => {
                let block = format!(
                    "### 文件: {}（{}）{}\n{}",
                    ex.rel_path,
                    ex.kind,
                    if ex.truncated { " · 已截断" } else { "" },
                    ex.text
                );
                if total + block.chars().count() > MAX_TOTAL_CHARS {
                    skipped.push(format!("{}（总上下文已满）", ex.rel_path));
                    continue;
                }
                total += block.chars().count();
                used_names.push(ex.rel_path.clone());
                parts.push(block);
            }
            Err(e) => skipped.push(format!("{}（{e}）", src.rel_path)),
        }
    }

    if parts.is_empty() {
        let hint = if files.is_empty() {
            "本次运行目录中未找到可分析的 .md/.log/.txt/.csv/.xlsx/.xls/.docx 文件。".to_string()
        } else {
            format!(
                "未能读取任何文件。{}",
                skipped.join("；")
            )
        };
        return Err(hint);
    }

    let mut ctx = parts.join("\n\n");
    if !skipped.is_empty() {
        ctx.push_str("\n\n### 未纳入的文件\n");
        ctx.push_str(&skipped.join("\n"));
    }
    Ok((ctx, used_names))
}

fn format_report(
    question: &str,
    task_name: &str,
    run_dir: &Path,
    answer: &str,
    sources: &[String],
    usage: Option<&TokenUsage>,
) -> String {
    let now = Local::now().format("%Y-%m-%d %H:%M:%S");
    let mut md = String::new();
    md.push_str("# AI 分析\n\n");
    md.push_str("## 问题\n\n");
    md.push_str(question.trim());
    md.push_str("\n\n## 分析\n\n");
    md.push_str(answer.trim());
    md.push('\n');
    md.push_str("\n## 参考文件\n\n");
    if sources.is_empty() {
        md.push_str("（无）\n");
    } else {
        for s in sources {
            md.push_str(&format!("- {s}\n"));
        }
    }
    md.push_str("\n## 概览\n\n");
    md.push_str("| 项 | 内容 |\n| --- | --- |\n");
    md.push_str(&format!("| 时间 | {now} |\n"));
    md.push_str(&format!("| 任务 | {} |\n", task_name.replace('|', "\\|")));
    md.push_str(&format!(
        "| 目录 | {} |\n",
        run_dir.display().to_string().replace('|', "\\|")
    ));
    if let Some(u) = usage {
        md.push_str(&format!(
            "| Token | 输入 {} / 输出 {} / 合计 {} |\n",
            u.prompt_tokens, u.completion_tokens, u.total_tokens
        ));
    }
    md
}

fn default_question() -> &'static str {
    "请总结本次运行产物中的关键信息、异常与值得关注的结论。"
}

fn check_cancel(cancel: Option<&AtomicBool>) -> Result<(), String> {
    if cancel.is_some_and(|f| f.load(std::sync::atomic::Ordering::SeqCst)) {
        return Err("已取消".into());
    }
    Ok(())
}

pub async fn analyze_run_artifacts(
    session: &RunSession,
    args: &str,
    cancel: Option<&AtomicBool>,
) -> Result<(String, String, String), String> {
    check_cancel(cancel)?;

    let question = {
        let q = args.trim();
        if q.is_empty() {
            default_question()
        } else {
            q
        }
    };

    let files = collect_analyzable_files(&session.run_dir);
    let (context, sources) = build_context(&session.run_dir, &files)?;

    let user_msg = format!(
        "## 用户问题\n{question}\n\n## 运行信息\n- 任务: {}\n- 目录: {}\n\n## 文件内容\n{context}",
        session.task_name,
        session.run_dir.display()
    );

    let result =
        ark::chat_completion_with_messages(SYSTEM_PROMPT_ANALYZE, &user_msg, 0.3, 4096, None).await?;
    check_cancel(cancel)?;
    let answer = result.content.trim().to_string();
    let report = format_report(
        question,
        &session.task_name,
        &session.run_dir,
        &answer,
        &sources,
        result.usage.as_ref(),
    );

    let usage_note = result
        .usage
        .as_ref()
        .map(|u| format!(" | Token {}/{}", u.prompt_tokens, u.completion_tokens))
        .unwrap_or_default();

    Ok((
        report,
        format!(
            "AI 分析完成（{} 个文件）{}",
            sources.len(),
            usage_note
        ),
        answer,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_extensions() {
        assert_eq!(classify_ext("md"), Some("text"));
        assert_eq!(classify_ext("xlsx"), Some("excel"));
        assert_eq!(classify_ext("docx"), Some("word"));
        assert!(classify_ext("png").is_none());
        assert!(is_media_ext("mp4"));
    }

    #[test]
    fn strip_xml_basic() {
        let t = strip_xml_text("<w:p><w:r><w:t>你好</w:t></w:r></w:p>");
        assert_eq!(t, "你好");
    }

    #[test]
    fn truncate_chars_works() {
        let (s, truncated) = truncate_chars("abcdef", 3);
        assert!(truncated);
        assert_eq!(s, "abc\n…（已截断，原文共 6 字）");
    }
}
