use calamine::{open_workbook_auto, Data, Reader};
use rhai::{Dynamic, EvalAltResult, ImmutableString};
use std::path::Path;
use std::rc::Rc;

/// Rhai 按值传递自定义类型时会调用 Clone。表格可能有数万行，因此共享底层行数据，
/// 避免每次 table_get/col_index 都深拷贝整张表。
#[derive(Clone)]
pub struct ScriptTable {
    rows: Rc<Vec<Vec<Dynamic>>>,
}

pub fn read_table(path: &Path) -> Result<ScriptTable, Box<EvalAltResult>> {
    let path = super::excel_compat::resolve_excel_path(path);
    let mut workbook = open_workbook_auto(&path).map_err(|e| {
        runtime_err(format!("读取表格失败 {}: {e}", path.display()))
    })?;
    let range = workbook
        .worksheet_range_at(0)
        .ok_or_else(|| runtime_err("表格无工作表".into()))?
        .map_err(|e| runtime_err(format!("读取工作表失败: {e}")))?;

    let mut rows = Vec::new();
    for row in range.rows() {
        let cells = row.iter().map(|cell| cell_to_dynamic(cell)).collect();
        rows.push(cells);
    }
    Ok(ScriptTable {
        rows: Rc::new(rows),
    })
}

/// 部分中文 .xls（MES 月报）会被 calamine 把 UTF-16LE 误读成 Latin-1（夹杂 \\0 或高低字节对）。
///
/// 注意：身份证号、ISO 日期等纯 ASCII 偶数长字符串，高低字节常落在 0x30–0x33，
/// 旧逻辑用「任一 pair」就会误判并解成乱码；必须排除正常 ASCII / 已是 Unicode 的文本。
///
/// 例外：部分汉字的 UTF-16LE 高低字节都落在可打印 ASCII（如「白海兰」→ `}vwmpQ`），
/// 不能一律跳过；此时用「高字节是否在 CJK 基本区 0x4E–0x9F」区分真中文误读与工号/证件号。
pub(crate) fn recover_xls_utf16_mojibake(s: &str) -> String {
    // calamine/encoding_rs 有时会把原始字节按 Windows-1252 解码（如 0x9E → ž），
    // 先无损还原字节；遇到真正的中文等不可还原字符则说明文本已经正常。
    let Some(bytes) = s.chars().map(windows_1252_byte).collect::<Option<Vec<_>>>() else {
        return s.to_string();
    };
    if bytes.len() < 4 || bytes.len() % 2 != 0 {
        return s.to_string();
    }
    let nulls = bytes.iter().filter(|&&b| b == 0).count();
    let printable = bytes
        .iter()
        .filter(|&&b| (0x20..=0x7E).contains(&b))
        .count();
    let pairs = bytes.len() / 2;
    let cjk_bmp_hi = bytes
        .chunks(2)
        .filter(|c| (0x4E..=0x9F).contains(&c[1]))
        .count();
    let cjk_hi_pairs = bytes
        .chunks(2)
        .filter(|c| (0x4E..=0x9F).contains(&c[1]) || (0x30..=0x33).contains(&c[1]))
        .count();

    let pure_ascii = nulls == 0 && printable == bytes.len();
    if pure_ascii {
        // 身份证/日期/工号：跳过。中文名被误读成纯 ASCII 时，高字节多在 CJK 基本区。
        if cjk_bmp_hi * 2 < pairs {
            return s.to_string();
        }
    } else {
        // 至少一半 pair 像 UTF-16LE（大量 \\0，或高字节落在 CJK/扩展区）
        let looks_utf16 = nulls * 2 >= pairs || cjk_hi_pairs * 2 >= pairs;
        if !looks_utf16 {
            return s.to_string();
        }
    }

    let u16s: Vec<u16> = bytes
        .chunks(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let recovered = String::from_utf16_lossy(&u16s);
    // 纯 ASCII 路径：还原结果必须含汉字，否则保持原文（防误伤）
    if pure_ascii
        && !recovered
            .chars()
            .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
    {
        return s.to_string();
    }
    recovered
}

fn windows_1252_byte(c: char) -> Option<u8> {
    let n = u32::from(c);
    if n <= 0xFF {
        return Some(n as u8);
    }
    Some(match c {
        '€' => 0x80,
        '‚' => 0x82,
        'ƒ' => 0x83,
        '„' => 0x84,
        '…' => 0x85,
        '†' => 0x86,
        '‡' => 0x87,
        'ˆ' => 0x88,
        '‰' => 0x89,
        'Š' => 0x8A,
        '‹' => 0x8B,
        'Œ' => 0x8C,
        'Ž' => 0x8E,
        '‘' => 0x91,
        '’' => 0x92,
        '“' => 0x93,
        '”' => 0x94,
        '•' => 0x95,
        '–' => 0x96,
        '—' => 0x97,
        '˜' => 0x98,
        '™' => 0x99,
        'š' => 0x9A,
        '›' => 0x9B,
        'œ' => 0x9C,
        'ž' => 0x9E,
        'Ÿ' => 0x9F,
        _ => return None,
    })
}

fn cell_to_dynamic(cell: &Data) -> Dynamic {
    match cell {
        Data::Empty => Dynamic::UNIT,
        Data::String(s) => Dynamic::from(recover_xls_utf16_mojibake(s)),
        Data::Float(f) => Dynamic::from(*f),
        Data::Int(i) => Dynamic::from(*i as f64),
        Data::Bool(b) => Dynamic::from(*b),
        Data::DateTime(dt) => Dynamic::from(dt.to_string()),
        Data::DateTimeIso(s) | Data::DurationIso(s) => Dynamic::from(s.clone()),
        Data::Error(e) => Dynamic::from(format!("{e:?}")),
    }
}

pub fn table_rows(table: ScriptTable) -> i64 {
    table.rows.len() as i64
}

pub fn table_get(table: ScriptTable, row: i64, col: i64) -> Dynamic {
    let row = row as usize;
    let col = col as usize;
    table
        .rows
        .get(row)
        .and_then(|r| r.get(col))
        .cloned()
        .unwrap_or(Dynamic::UNIT)
}

pub fn col_index(table: ScriptTable, header_row: i64, name: &str) -> i64 {
    let row = header_row as usize;
    let Some(headers) = table.rows.get(row) else {
        return -1;
    };
    for (i, cell) in headers.iter().enumerate() {
        if to_str(cell.clone()) == name {
            return i as i64;
        }
    }
    -1
}

pub fn to_str(v: Dynamic) -> ImmutableString {
    if v.is_unit() {
        return ImmutableString::from("");
    }
    if let Ok(s) = v.clone().into_string() {
        return ImmutableString::from(s);
    }
    if let Ok(n) = v.as_float() {
        if (n.fract()).abs() < f64::EPSILON {
            return ImmutableString::from(format!("{}", n as i64));
        }
        return ImmutableString::from(format!("{n}"));
    }
    if let Ok(n) = v.as_int() {
        return ImmutableString::from(format!("{n}"));
    }
    if let Ok(b) = v.as_bool() {
        return ImmutableString::from(if b { "true" } else { "false" });
    }
    ImmutableString::from(v.to_string())
}

pub fn to_num(v: Dynamic) -> f64 {
    if v.is_unit() {
        return 0.0;
    }
    if let Ok(n) = v.as_float() {
        return n;
    }
    if let Ok(n) = v.as_int() {
        return n as f64;
    }
    if let Ok(s) = v.clone().into_string() {
        let t = s.trim();
        if t.is_empty() {
            return 0.0;
        }
        return t.parse().unwrap_or(0.0);
    }
    0.0
}

fn runtime_err(msg: String) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(msg.into(), rhai::Position::NONE))
}

pub fn register(engine: &mut rhai::Engine) {
    engine.register_type_with_name::<ScriptTable>("Table");

    engine.register_fn("table_rows", table_rows);
    engine.register_fn("table_get", table_get);
    engine.register_fn("col_index", col_index);
    engine.register_fn(
        "col_index",
        |table: ScriptTable, header_row: i64, name: ImmutableString| -> i64 {
            col_index(table, header_row, name.as_str())
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn recover_ascii_utf16_mojibake() {
        let raw: String = "0\u{0}1\u{0}0\u{0}1\u{0}5\u{0}0\u{0}0\u{0}0\u{0}4\u{0}5\u{0}".into();
        assert_eq!(recover_xls_utf16_mojibake(&raw), "0101500045");
    }

    #[test]
    fn does_not_corrupt_id_card_or_iso_date() {
        assert_eq!(
            recover_xls_utf16_mojibake("330102199001011234"),
            "330102199001011234"
        );
        // 末位 X 的身份证：末 pair 高字节 0x58 落在 CJK 区，但仍应保持原文
        assert_eq!(
            recover_xls_utf16_mojibake("14030219820128043X"),
            "14030219820128043X"
        );
        assert_eq!(recover_xls_utf16_mojibake("2024-06-01"), "2024-06-01");
        assert_eq!(
            recover_xls_utf16_mojibake("2024-06-01 00:00:00"),
            "2024-06-01 00:00:00"
        );
        assert_eq!(recover_xls_utf16_mojibake("0101700227"), "0101700227");
        assert_eq!(recover_xls_utf16_mojibake("张春娇"), "张春娇");
        assert_eq!(recover_xls_utf16_mojibake(" _ÑSžX"), "张发增");
    }

    #[test]
    fn recovers_chinese_names_misread_as_printable_ascii() {
        // 白海兰 / 周捷 / 杨晓强：UTF-16LE 字节全落在可打印 ASCII，旧逻辑会跳过
        assert_eq!(recover_xls_utf16_mojibake("}vwmpQ"), "白海兰");
        assert_eq!(recover_xls_utf16_mojibake("hTwc"), "周捷");
        assert_eq!(recover_xls_utf16_mojibake("hgSf:_"), "杨晓强");
        assert_eq!(recover_xls_utf16_mojibake("NgKfs^"), "李晋平");
        assert_eq!(recover_xls_utf16_mojibake(" _=N=N"), "张丽丽");
    }

    #[test]
    fn reads_xls_frontline_monthly() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/excel/2026-06月报.xls");
        if !path.exists() {
            return;
        }
        let table = read_table(&path).expect("read xls");
        assert!(table_rows(table.clone()) >= 2);
        assert_eq!(col_index(table.clone(), 0, "姓名_编号"), 3);
        assert_eq!(col_index(table.clone(), 0, "姓名_姓名"), 2);
        let first_name = to_str(table_get(table.clone(), 1, 2));
        assert!(
            first_name.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "name should be decoded Chinese: {first_name}"
        );
        // 曾被误读成 "}vwmpQ" / "hTwc" 的姓名，读表后必须是汉字
        let mut saw_bai = false;
        let mut saw_zhou = false;
        let total = table_rows(table.clone());
        let mut r = 1i64;
        while r < total {
            let name = to_str(table_get(table.clone(), r, 2));
            if name.as_str() == "白海兰" {
                saw_bai = true;
            }
            if name.as_str() == "周捷" {
                saw_zhou = true;
            }
            r += 1;
        }
        assert!(saw_bai, "白海兰 should be recovered from xls");
        assert!(saw_zhou, "周捷 should be recovered from xls");
        assert!(!to_str(table_get(table.clone(), 1, 3)).is_empty());
        assert!(to_num(table_get(table.clone(), 1, 7)).is_finite());
        // 加班列可按前缀定位
        assert!(col_index(table.clone(), 0, "夜保加班-夜保加班工时数") >= 0);
    }

    #[test]
    fn reads_leave_register_and_ot_schedule() {
        let leave = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/excel/请假登记.xls");
        if leave.exists() {
            let table = read_table(&leave).expect("read leave");
            assert_eq!(col_index(table.clone(), 0, "员工姓名"), 2, "姓名列");
            assert_eq!(col_index(table.clone(), 0, "请假事由"), 4, "事由列");
            assert_eq!(col_index(table.clone(), 0, "请假日期"), 5, "日期列");
            assert_eq!(to_str(table_get(table.clone(), 1, 4)).as_str(), "事假");
        }
        let ot = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/excel/加班排班记录.xls");
        if ot.exists() {
            let table = read_table(&ot).expect("read ot schedule");
            assert_eq!(col_index(table.clone(), 0, "人员编号"), 1);
            assert_eq!(col_index(table.clone(), 0, "人员姓名"), 2);
            assert_eq!(col_index(table.clone(), 0, "月工时合计"), 5);
            assert!(col_index(table.clone(), 0, "06-19") >= 0);
        }
    }
}
