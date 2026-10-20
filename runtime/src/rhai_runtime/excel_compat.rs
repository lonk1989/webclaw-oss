//! xls / xlsx 互通：查找、路径解析、保存扩展名归一。
//! 业务脚本仍可写 `\.xlsx$` / `open_workbook(...xlsx)`，运行时自动兼容同名 `.xls`。

use std::path::{Path, PathBuf};

const EXCEL_EXTS: &[&str] = &["xlsx", "xls", "xlsm"];

fn stem_and_ext(name: &str) -> Option<(&str, &str)> {
    let dot = name.rfind('.')?;
    if dot == 0 || dot + 1 >= name.len() {
        return None;
    }
    let ext = &name[dot + 1..];
    if !EXCEL_EXTS.iter().any(|e| ext.eq_ignore_ascii_case(e)) {
        return None;
    }
    Some((&name[..dot], ext))
}

/// 同一逻辑名的其它 Excel 扩展名别名（含原名）。
pub fn excel_name_aliases(name: &str) -> Vec<String> {
    let mut out = vec![name.to_string()];
    let Some((stem, ext)) = stem_and_ext(name) else {
        return out;
    };
    for alt in EXCEL_EXTS {
        if !ext.eq_ignore_ascii_case(alt) {
            out.push(format!("{stem}.{alt}"));
        }
    }
    out
}

/// 正则按「字面文件名」匹配；若不中，用 xls↔xlsx 别名再试（使 `\.xlsx$` 能命中 `.xls`）。
pub fn name_matches_excel_pattern(re: &regex::Regex, name: &str) -> bool {
    if re.is_match(name) {
        return true;
    }
    excel_name_aliases(name)
        .into_iter()
        .skip(1)
        .any(|alias| re.is_match(&alias))
}

/// 路径不存在时，尝试同名的其它 Excel 扩展名。
pub fn resolve_excel_path(path: &Path) -> PathBuf {
    if path.is_file() {
        return path.to_path_buf();
    }
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return path.to_path_buf();
    };
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    for alias in excel_name_aliases(name).into_iter().skip(1) {
        let cand = parent.join(&alias);
        if cand.is_file() {
            return cand;
        }
    }
    path.to_path_buf()
}

/// 写入侧统一为 xlsx（umya 仅支持写 OOXML）；`.xls` / `.xlsm` 目标改写为 `.xlsx`。
pub fn normalize_excel_write_path(path: &Path) -> PathBuf {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return path.to_path_buf();
    };
    let Some((stem, ext)) = stem_and_ext(name) else {
        return path.to_path_buf();
    };
    if ext.eq_ignore_ascii_case("xlsx") {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{stem}.xlsx"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_swap_xls_xlsx() {
        let a = excel_name_aliases("考勤月报_测试.xlsx");
        assert!(a.iter().any(|n| n == "考勤月报_测试.xls"));
        let b = excel_name_aliases("2026-06月报.xls");
        assert!(b.iter().any(|n| n == "2026-06月报.xlsx"));
    }

    #[test]
    fn pattern_xlsx_matches_xls_file() {
        let re = regex::Regex::new(r"^考勤月报_.*\.xlsx$").unwrap();
        assert!(name_matches_excel_pattern(&re, "考勤月报_甲.xlsx"));
        assert!(name_matches_excel_pattern(&re, "考勤月报_甲.xls"));
        assert!(!name_matches_excel_pattern(&re, "异常数据_甲.xls"));
    }

    #[test]
    fn write_path_xls_becomes_xlsx() {
        let p = PathBuf::from(r"C:\runs\files\out.xls");
        assert_eq!(
            normalize_excel_write_path(&p),
            PathBuf::from(r"C:\runs\files\out.xlsx")
        );
    }
}
