use calamine::{open_workbook_auto, Reader};
use serde::Serialize;
use std::fs;
use std::path::Path;

#[derive(Clone, Serialize)]
pub struct Preview {
    pub ok: bool,
    pub rows: Option<Vec<Vec<String>>>,
    pub text: Option<String>,
    pub error: Option<String>,
    pub engine: Option<String>,
}

pub fn preview_path(path: &Path) -> Preview {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "xlsx" | "xlsm" | "xls" => preview_excel(path),
        "txt" | "csv" | "md" | "log" | "rhai" => match fs::read_to_string(path) {
            Ok(s) => Preview {
                ok: true,
                rows: None,
                text: Some(s.chars().take(4000).collect()),
                error: None,
                engine: Some("text".into()),
            },
            Err(e) => Preview {
                ok: false,
                rows: None,
                text: None,
                error: Some(e.to_string()),
                engine: None,
            },
        },
        _ => Preview {
            ok: false,
            rows: None,
            text: None,
            error: Some(format!("不支持预览 .{ext}")),
            engine: None,
        },
    }
}

fn preview_excel(path: &Path) -> Preview {
    match open_workbook_auto(path) {
        Ok(mut wb) => {
            let Some(Ok(range)) = wb.worksheet_range_at(0) else {
                return Preview {
                    ok: false,
                    rows: None,
                    text: None,
                    error: Some("表格无工作表".into()),
                    engine: None,
                };
            };
            let mut rows = Vec::new();
            for (i, row) in range.rows().enumerate() {
                if i >= 6 {
                    break;
                }
                let cells: Vec<String> = row
                    .iter()
                    .take(18)
                    .map(|c| c.to_string().chars().take(80).collect())
                    .collect();
                rows.push(cells);
            }
            Preview {
                ok: true,
                rows: Some(rows),
                text: None,
                error: None,
                engine: Some("calamine".into()),
            }
        }
        Err(e) => Preview {
            ok: false,
            rows: None,
            text: None,
            error: Some(e.to_string()),
            engine: None,
        },
    }
}
