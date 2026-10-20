//! 脚本工作台：交付后的语法与常见坑校验。

use regex::Regex;
use rhai::Engine;
use serde::Serialize;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateResult {
    pub ok: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn validate_script(script: &str) -> ValidateResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let src = script.trim();
    if src.is_empty() {
        return ValidateResult {
            ok: false,
            errors: vec!["脚本为空".into()],
            warnings,
        };
    }

    let mut engine = Engine::new();
    engine.set_max_expr_depths(128, 64);
    if let Err(e) = engine.compile(src) {
        errors.push(format!("语法错误: {e}"));
    }

    lint_common_pitfalls(src, &mut errors, &mut warnings);

    ValidateResult {
        ok: errors.is_empty(),
        errors,
        warnings,
    }
}

fn lint_common_pitfalls(src: &str, errors: &mut Vec<String>, warnings: &mut Vec<String>) {
    // for-in：本环境 map 不可迭代，且提示词要求用 while
    if let Ok(re) = Regex::new(r"(?m)^\s*for\s+\w+\s+in\s+") {
        for m in re.find_iter(src) {
            let line = line_no(src, m.start());
            errors.push(format!(
                "第 {line} 行：不要使用 for-in。map（#{{}}）不可迭代；请用数组存键 + while 遍历"
            ));
        }
    }

    // let x = s.trim() —— trim 返回 ()
    if let Ok(re) = Regex::new(r"(?m)let\s+\w+\s*=\s*[^;\n]*\.trim\s*\(") {
        for m in re.find_iter(src) {
            let line = line_no(src, m.start());
            errors.push(format!(
                "第 {line} 行：不要写 let x = s.trim()（返回 ()）。请用 str_trim(s)"
            ));
        }
    }

    if src.contains("wb_save") && !src.contains("track_output") {
        warnings.push("调用了 wb_save 但未 track_output，产物可能不会出现在运行结果里".into());
    }

    if src.contains("new_workbook") && !src.contains("wb_new_sheet") {
        warnings.push("new_workbook() 后需 wb_new_sheet 再建表，否则无法写入单元格".into());
    }

    if (src.contains("read_table") || src.contains("open_workbook") || src.contains("find_file"))
        && src.contains(".xlsx")
        && src.contains("xls")
        && !src.contains("\\.xls")
    {
        // soft: many scripts correctly use \\.xlsx$ which also finds .xls via runtime alias
    }
}

fn line_no(src: &str, byte_idx: usize) -> usize {
    src[..byte_idx].bytes().filter(|b| *b == b'\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_for_in_on_map_pattern() {
        let r = validate_script(
            r#"
let stats = #{};
let units = [];
for u in stats {
    units.push(u);
}
"#,
        );
        assert!(!r.ok, "{:?}", r.errors);
        assert!(r.errors.iter().any(|e| e.contains("for-in")));
    }

    #[test]
    fn accepts_while_over_keys() {
        let r = validate_script(
            r#"
let units = [];
let i = 0;
while i < units.len() {
    i += 1;
}
print("ok");
"#,
        );
        assert!(r.ok, "{:?}", r.errors);
    }

    #[test]
    fn rejects_trim_assignment() {
        let r = validate_script(
            r#"
let s = " a ";
let x = s.trim();
print(x);
"#,
        );
        assert!(!r.ok);
        assert!(r.errors.iter().any(|e| e.contains("str_trim")));
    }
}
