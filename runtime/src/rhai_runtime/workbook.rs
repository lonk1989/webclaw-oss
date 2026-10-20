use calamine::{open_workbook_auto, Data, Reader};
use rhai::{Dynamic, EvalAltResult};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use umya_spreadsheet::{self, Spreadsheet};

use super::excel_compat::{normalize_excel_write_path, resolve_excel_path};
use super::table;

/// Rhai 按值传递自定义类型时会 Clone；用 Rc<RefCell> 保证写入落在同一工作簿上。
#[derive(Clone)]
pub struct ScriptWorkbook {
    inner: Rc<RefCell<Spreadsheet>>,
    path: Rc<RefCell<PathBuf>>,
}

pub fn open_workbook(path: &Path) -> Result<ScriptWorkbook, Box<EvalAltResult>> {
    let resolved = resolve_excel_path(path);
    if !resolved.is_file() {
        return Err(runtime_err(format!("打开工作簿失败：文件不存在 {}", path.display())));
    }
    let book = load_spreadsheet(&resolved)?;
    Ok(ScriptWorkbook {
        inner: Rc::new(RefCell::new(book)),
        path: Rc::new(RefCell::new(resolved)),
    })
}

/// 新建空白工作簿（不含默认 Sheet1）。请先 `wb_new_sheet` 再写入。
pub fn new_workbook() -> ScriptWorkbook {
    ScriptWorkbook {
        inner: Rc::new(RefCell::new(umya_spreadsheet::new_file_empty_worksheet())),
        path: Rc::new(RefCell::new(PathBuf::from("untitled.xlsx"))),
    }
}

fn load_spreadsheet(path: &Path) -> Result<Spreadsheet, Box<EvalAltResult>> {
    // 优先 OOXML（含扩展名是 .xls 但实为 zip 的下载）
    if let Ok(book) = umya_spreadsheet::reader::xlsx::read(path) {
        return Ok(book);
    }
    // 旧版 BIFF .xls 等：calamine 读值 → umya 可写簿（样式不保留）
    spreadsheet_from_calamine(path).map_err(|e| {
        runtime_err(format!("打开工作簿失败 {}: {e}", path.display()))
    })
}

fn spreadsheet_from_calamine(path: &Path) -> Result<Spreadsheet, String> {
    let mut input = open_workbook_auto(path).map_err(|e| format!("{e}"))?;
    let names = input.sheet_names().to_vec();
    if names.is_empty() {
        return Err("表格无工作表".into());
    }
    let mut book = umya_spreadsheet::new_file_empty_worksheet();
    for name in &names {
        let range = input
            .worksheet_range(name)
            .map_err(|e| format!("读取工作表「{name}」失败: {e}"))?;
        let ws = book
            .new_sheet(name.as_str())
            .map_err(|e| format!("创建工作表「{name}」失败: {e}"))?;
        for (r, row) in range.rows().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                let coord = ((c + 1) as u32, (r + 1) as u32);
                match cell {
                    Data::Empty => {}
                    Data::Float(f) => {
                        ws.get_cell_mut(coord).set_value_number(*f);
                    }
                    Data::Int(i) => {
                        ws.get_cell_mut(coord).set_value_number(*i as f64);
                    }
                    Data::Bool(b) => {
                        ws.get_cell_mut(coord).set_value_bool(*b);
                    }
                    Data::String(s) => {
                        let s = table::recover_xls_utf16_mojibake(s);
                        ws.get_cell_mut(coord).set_value_string(s);
                    }
                    Data::DateTime(dt) => {
                        // Excel 序列日优先，便于后续数值比较
                        ws.get_cell_mut(coord).set_value_number(dt.as_f64());
                    }
                    Data::DateTimeIso(s) | Data::DurationIso(s) => {
                        ws.get_cell_mut(coord).set_value_string(s.clone());
                    }
                    Data::Error(e) => {
                        ws.get_cell_mut(coord).set_value_string(format!("{e:?}"));
                    }
                }
            }
        }
    }
    Ok(book)
}

pub fn wb_set_title(wb: &ScriptWorkbook, sheet_name: &str, title: &str) -> Result<(), Box<EvalAltResult>> {
    let mut book = wb.inner.borrow_mut();
    let ws = worksheet_mut(&mut book, sheet_name)?;
    let cell = ws.get_cell_mut((1, 1));
    let current = cell.get_value().into_owned();
    let new_title = if current.contains('年') && current.contains('月') {
        regex_replace_month(&current, title)
    } else {
        title.to_string()
    };
    cell.set_value_string(new_title);
    Ok(())
}

fn regex_replace_month(current: &str, title: &str) -> String {
    if let Ok(re) = regex::Regex::new(r"年\d+月") {
        if let Some(m) = re.find(current) {
            if let Ok(re2) = regex::Regex::new(r"年\d+月") {
                if let Some(rep) = re2.find(title) {
                    return current.replacen(m.as_str(), rep.as_str(), 1);
                }
            }
        }
    }
    title.to_string()
}

pub fn ws_set_cell(
    wb: &ScriptWorkbook,
    sheet_name: &str,
    row: i64,
    col: i64,
    value: Dynamic,
) -> Result<(), Box<EvalAltResult>> {
    let mut book = wb.inner.borrow_mut();
    let ws = worksheet_mut(&mut book, sheet_name)?;
    let cell = ws.get_cell_mut((col as u32, row as u32));
    if value.is_unit() {
        cell.set_value_string("");
        return Ok(());
    }
    if let Ok(s) = value.clone().into_string() {
        cell.set_value_string(s.to_string());
        return Ok(());
    }
    if let Ok(n) = value.as_float() {
        cell.set_value_number(n);
        return Ok(());
    }
    if let Ok(n) = value.as_int() {
        cell.set_value_number(n as f64);
        return Ok(());
    }
    if let Ok(b) = value.as_bool() {
        cell.set_value_bool(b);
        return Ok(());
    }
    cell.set_value_string(value.to_string());
    Ok(())
}

/// 在指定单元格嵌入图片（锚点为该格左上角）。`max_width_px` 限制显示宽度，按比例缩放。
pub fn ws_add_image(
    wb: &ScriptWorkbook,
    sheet_name: &str,
    row: i64,
    col: i64,
    image_path: &Path,
    max_width_px: i64,
) -> Result<(), Box<EvalAltResult>> {
    if row < 1 || col < 1 {
        return Err(runtime_err("行列须为 1 基正整数".into()));
    }
    if !image_path.is_file() {
        return Err(runtime_err(format!(
            "图片不存在: {}",
            image_path.display()
        )));
    }
    let bytes = std::fs::read(image_path)
        .map_err(|e| runtime_err(format!("读取图片失败 {}: {e}", image_path.display())))?;

    // Excel 对 xl/media 下非 ASCII 文件名兼容差（中文名常显示红叉）；统一用 ASCII 名
    let ext = image_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .filter(|e| matches!(e.as_str(), "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp"))
        .unwrap_or_else(|| {
            if bytes.len() >= 8 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                "png".into()
            } else {
                "jpg".into()
            }
        });
    let name = format!(
        "img_{}_{}.{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        (row as u64) * 1000 + (col as u64),
        ext
    );

    let (orig_w, orig_h) = image_pixel_size(&bytes).unwrap_or((800, 600));
    let max_w = max_width_px.max(40) as u32;
    let (disp_w, disp_h) = scale_to_max_width(orig_w, orig_h, max_w);

    let mut marker = umya_spreadsheet::structs::drawing::spreadsheet::MarkerType::default();
    let coord = format!("{}{}", col_letter(col as u32), row);
    marker.set_coordinate(coord.as_str());

    let mut image = umya_spreadsheet::structs::Image::default();
    image.new_image_with_dimensions(disp_h, disp_w, &name, bytes, marker);

    let mut book = wb.inner.borrow_mut();
    let ws = worksheet_mut(&mut book, sheet_name)?;
    ws.add_image(image);

    // 行高取「当前行高」与「本图显示高度」较大者，避免整改前/后两张图互相覆盖
    let new_h = (disp_h as f64 * 0.75).clamp(40.0, 320.0);
    let cur_h = ws
        .get_row_dimension(&(row as u32))
        .map(|d| *d.get_height())
        .unwrap_or(0.0);
    let row_h = new_h.max(cur_h);
    ws.get_row_dimension_mut(&(row as u32)).set_height(row_h);

    // 列宽略放大，便于看图
    let col_w = (disp_w as f64 / 7.0).clamp(18.0, 48.0);
    ws.get_column_dimension_mut(&col_letter(col as u32))
        .set_width(col_w);

    Ok(())
}

pub fn ws_set_row_height(
    wb: &ScriptWorkbook,
    sheet_name: &str,
    row: i64,
    height: f64,
) -> Result<(), Box<EvalAltResult>> {
    if row < 1 {
        return Err(runtime_err("行号须为 1 基正整数".into()));
    }
    let mut book = wb.inner.borrow_mut();
    let ws = worksheet_mut(&mut book, sheet_name)?;
    ws.get_row_dimension_mut(&(row as u32))
        .set_height(height.clamp(1.0, 409.0));
    Ok(())
}

fn col_letter(col: u32) -> String {
    let mut n = col;
    let mut s = String::new();
    while n > 0 {
        let rem = ((n - 1) % 26) as u8;
        s.insert(0, (b'A' + rem) as char);
        n = (n - 1) / 26;
    }
    s
}

fn scale_to_max_width(w: u32, h: u32, max_w: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (max_w, max_w * 3 / 4);
    }
    if w <= max_w {
        return (w, h);
    }
    let nh = ((h as u64 * max_w as u64) / w as u64).max(1) as u32;
    (max_w, nh)
}

fn image_pixel_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() > 4 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        let mut i = 2usize;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            if marker == 0xD8 || marker == 0xD9 {
                i += 2;
                continue;
            }
            if i + 3 >= bytes.len() {
                break;
            }
            let seglen = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            if marker == 0xC0 || marker == 0xC2 {
                if i + 8 < bytes.len() {
                    let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                    let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
                    return Some((w.max(1), h.max(1)));
                }
                break;
            }
            i += 2 + seglen;
        }
    }
    if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return Some((w.max(1), h.max(1)));
    }
    None
}

/// 读取单元格：有数字优先返回 f64（便于 Excel 序列日），否则返回字符串
pub fn ws_get_cell(
    wb: &ScriptWorkbook,
    sheet_name: &str,
    row: i64,
    col: i64,
) -> Result<Dynamic, Box<EvalAltResult>> {
    let book = wb.inner.borrow();
    let ws = book
        .get_sheet_by_name(sheet_name)
        .ok_or_else(|| runtime_err(format!("工作表不存在: {sheet_name}")))?;
    let coord = (col as u32, row as u32);
    if let Some(n) = ws.get_value_number(coord) {
        return Ok(Dynamic::from(n));
    }
    let s = ws.get_value(coord);
    if s.is_empty() {
        return Ok(Dynamic::UNIT);
    }
    Ok(Dynamic::from(s))
}

pub fn ws_insert_rows(
    wb: &ScriptWorkbook,
    sheet_name: &str,
    row: i64,
    count: i64,
) -> Result<(), Box<EvalAltResult>> {
    let mut book = wb.inner.borrow_mut();
    let ws = worksheet_mut(&mut book, sheet_name)?;
    let row_index = row as u32;
    let num_rows = count.max(1) as u32;
    ws.insert_new_row(&row_index, &num_rows);
    Ok(())
}

/// 删除从 `col`（1-based）起的连续列；用于去掉模板中当月不存在的日期列
pub fn ws_remove_columns(
    wb: &ScriptWorkbook,
    sheet_name: &str,
    col: i64,
    count: i64,
) -> Result<(), Box<EvalAltResult>> {
    if count <= 0 {
        return Ok(());
    }
    if col < 1 {
        return Err(runtime_err(format!("无效列号: {col}")));
    }
    let mut book = wb.inner.borrow_mut();
    let ws = worksheet_mut(&mut book, sheet_name)?;
    let col_index = col as u32;
    let num_cols = count as u32;
    ws.remove_column_by_index(&col_index, &num_cols);
    Ok(())
}

pub fn ws_copy_row_style(
    wb: &ScriptWorkbook,
    sheet_name: &str,
    src_row: i64,
    dst_row: i64,
    max_col: i64,
) -> Result<(), Box<EvalAltResult>> {
    let mut book = wb.inner.borrow_mut();
    let ws = worksheet_mut(&mut book, sheet_name)?;
    let src = src_row as u32;
    let dst = dst_row as u32;
    let max_col = max_col.max(1) as u32;
    ws.copy_row_styling(&src, &dst, Some(&1), Some(&max_col));
    Ok(())
}

pub fn ws_set_formula(
    wb: &ScriptWorkbook,
    sheet_name: &str,
    row: i64,
    col: i64,
    formula: &str,
) -> Result<(), Box<EvalAltResult>> {
    let mut book = wb.inner.borrow_mut();
    let ws = worksheet_mut(&mut book, sheet_name)?;
    ws.get_cell_mut((col as u32, row as u32))
        .set_formula(formula);
    Ok(())
}

/// Excel 日期序列（1900 日期系统，含兼容假闰年偏差）
pub fn excel_date_serial(year: i64, month: i64, day: i64) -> Result<f64, Box<EvalAltResult>> {
    use chrono::NaiveDate;
    let y = year as i32;
    let m = month as u32;
    let d = day as u32;
    let date = NaiveDate::from_ymd_opt(y, m, d).ok_or_else(|| {
        runtime_err(format!("无效日期: {year}-{month:02}-{day:02}"))
    })?;
    let excel_epoch = NaiveDate::from_ymd_opt(1899, 12, 30).unwrap();
    Ok((date - excel_epoch).num_days() as f64)
}

/// Excel 序列日 → `YYYY-MM-DD`（人事导出里入职/离职常为数值日期）
pub fn excel_serial_ymd(serial: f64) -> Result<String, Box<EvalAltResult>> {
    use chrono::{Duration, NaiveDate};
    if !serial.is_finite() || serial < 1.0 || serial > 100_000.0 {
        return Err(runtime_err(format!("无效 Excel 日期序列: {serial}")));
    }
    let days = serial.floor() as i64;
    let excel_epoch = NaiveDate::from_ymd_opt(1899, 12, 30).unwrap();
    let date = excel_epoch + Duration::days(days);
    Ok(date.format("%Y-%m-%d").to_string())
}

pub fn days_in_month(year: i64, month: i64) -> Result<i64, Box<EvalAltResult>> {
    use chrono::NaiveDate;
    let y = year as i32;
    let m = month as u32;
    if !(1..=12).contains(&m) {
        return Err(runtime_err(format!("无效月份: {month}")));
    }
    let first_next = if m == 12 {
        NaiveDate::from_ymd_opt(y + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(y, m + 1, 1)
    }
    .ok_or_else(|| runtime_err(format!("无效年月: {year}-{month:02}")))?;
    let first = NaiveDate::from_ymd_opt(y, m, 1)
        .ok_or_else(|| runtime_err(format!("无效年月: {year}-{month:02}")))?;
    Ok((first_next - first).num_days())
}

/// 写入 Excel 日期（保留单元格原有数字格式，如 yyyy"年"m"月"）
pub fn ws_set_date(
    wb: &ScriptWorkbook,
    sheet_name: &str,
    row: i64,
    col: i64,
    year: i64,
    month: i64,
    day: i64,
) -> Result<(), Box<EvalAltResult>> {
    let serial = excel_date_serial(year, month, day)?;
    let mut book = wb.inner.borrow_mut();
    let ws = worksheet_mut(&mut book, sheet_name)?;
    ws.get_cell_mut((col as u32, row as u32))
        .set_value_number(serial);
    Ok(())
}

pub fn wb_sheet_names(wb: &ScriptWorkbook) -> Vec<String> {
    let book = wb.inner.borrow();
    (0..book.get_sheet_count())
        .filter_map(|i| book.get_sheet(&i).map(|ws| ws.get_name().to_string()))
        .collect()
}

fn validate_sheet_name(name: &str) -> Result<(), Box<EvalAltResult>> {
    let name = name.trim();
    if name.is_empty() {
        return Err(runtime_err("工作表名称不能为空".into()));
    }
    if name.len() > 31 {
        return Err(runtime_err(format!("工作表名称过长（最多 31 字符）: {name}")));
    }
    if name.contains(['\\', '/', '?', '*', '[', ']', ':']) {
        return Err(runtime_err(format!(
            "工作表名称含非法字符 \\ / ? * [ ] : ：{name}"
        )));
    }
    Ok(())
}

/// 新建空白工作表；名称已存在则报错。
/// 若簿里只剩一张完全空白的默认 Sheet1，则改名复用，避免产出多一张空表。
pub fn wb_new_sheet(wb: &ScriptWorkbook, sheet_name: &str) -> Result<(), Box<EvalAltResult>> {
    validate_sheet_name(sheet_name)?;
    let name = sheet_name.trim();
    let mut book = wb.inner.borrow_mut();
    if book.get_sheet_by_name(name).is_some() {
        return Err(runtime_err(format!("工作表已存在: {name}")));
    }
    if try_reuse_blank_default_sheet(&mut book, name) {
        return Ok(());
    }
    book.new_sheet(name)
        .map_err(|e| runtime_err(format!("新建工作表失败「{name}」: {e}")))?;
    Ok(())
}

fn is_blank_default_sheet(ws: &umya_spreadsheet::Worksheet) -> bool {
    let n = ws.get_name();
    let default = n.eq_ignore_ascii_case("Sheet1") || n == "工作表1";
    if !default {
        return false;
    }
    let (col, row) = ws.get_highest_column_and_row();
    col == 0 && row == 0
}

fn try_reuse_blank_default_sheet(book: &mut Spreadsheet, name: &str) -> bool {
    if book.get_sheet_count() != 1 {
        return false;
    }
    let Some(ws) = book.get_sheet(&0) else {
        return false;
    };
    if !is_blank_default_sheet(ws) {
        return false;
    }
    if let Some(ws) = book.get_sheet_mut(&0) {
        ws.set_name(name);
        return true;
    }
    false
}

/// 复制工作表到新名称（含单元格与样式）；目标名已存在则报错
pub fn wb_copy_sheet(
    wb: &ScriptWorkbook,
    src_sheet: &str,
    new_sheet: &str,
) -> Result<(), Box<EvalAltResult>> {
    validate_sheet_name(new_sheet)?;
    let new_name = new_sheet.trim();
    let mut book = wb.inner.borrow_mut();
    let mut clone_sheet = book
        .get_sheet_by_name(src_sheet)
        .ok_or_else(|| runtime_err(format!("工作表不存在: {src_sheet}")))?
        .clone();
    clone_sheet.set_name(new_name);
    book.add_sheet(clone_sheet)
        .map_err(|e| runtime_err(format!("复制工作表失败「{src_sheet}」→「{new_name}」: {e}")))?;
    Ok(())
}

/// 清空工作表内嵌图片（标准 Drawing；WPS 单元格图片可能残留，保存后一般会丢失）
pub fn ws_clear_images(wb: &ScriptWorkbook, sheet_name: &str) -> Result<(), Box<EvalAltResult>> {
    let mut book = wb.inner.borrow_mut();
    let ws = worksheet_mut(&mut book, sheet_name)?;
    ws.get_image_collection_mut().clear();
    Ok(())
}

/// 清空矩形区域单元格值（含样式保留）
pub fn ws_clear_range(
    wb: &ScriptWorkbook,
    sheet_name: &str,
    start_row: i64,
    end_row: i64,
    start_col: i64,
    end_col: i64,
) -> Result<(), Box<EvalAltResult>> {
    if start_row < 1 || start_col < 1 || end_row < start_row || end_col < start_col {
        return Err(runtime_err("清空区域行列无效".into()));
    }
    let mut book = wb.inner.borrow_mut();
    let ws = worksheet_mut(&mut book, sheet_name)?;
    for r in start_row..=end_row {
        for c in start_col..=end_col {
            ws.get_cell_mut((c as u32, r as u32)).set_value_string("");
        }
    }
    Ok(())
}

pub fn wb_save(wb: &ScriptWorkbook, path: &Path) -> Result<(), Box<EvalAltResult>> {
    // umya 只写 OOXML；目标若为 .xls 则落到同名 .xlsx
    let path = normalize_excel_write_path(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| runtime_err(format!("创建目录失败: {e}")))?;
    }
    let book = wb.inner.borrow();
    umya_spreadsheet::writer::xlsx::write(&book, &path).map_err(|e| {
        runtime_err(format!("保存工作簿失败 {}: {e}", path.display()))
    })?;
    *wb.path.borrow_mut() = path;
    Ok(())
}

fn worksheet_mut<'a>(
    book: &'a mut Spreadsheet,
    sheet_name: &str,
) -> Result<&'a mut umya_spreadsheet::Worksheet, Box<EvalAltResult>> {
    book.get_sheet_by_name_mut(sheet_name)
        .ok_or_else(|| runtime_err(format!("工作表不存在: {sheet_name}")))
}

fn runtime_err(msg: String) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(msg.into(), rhai::Position::NONE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn excel_serial_for_2026_06_01() {
        let s = excel_date_serial(2026, 6, 1).unwrap();
        // 2026-06-01 relative to 1899-12-30
        assert!((s - 46174.0).abs() < 0.5, "serial={s}");
        assert_eq!(days_in_month(2026, 6).unwrap(), 30);
        assert_eq!(days_in_month(2026, 5).unwrap(), 31);
        assert_eq!(excel_serial_ymd(s).unwrap(), "2026-06-01");
    }

    #[test]
    fn new_workbook_has_no_default_sheet1() {
        let wb = new_workbook();
        assert!(wb_sheet_names(&wb).is_empty());
        wb_new_sheet(&wb, "完成率统计").unwrap();
        assert_eq!(wb_sheet_names(&wb), vec!["完成率统计".to_string()]);
        wb_new_sheet(&wb, "明细").unwrap();
        assert_eq!(
            wb_sheet_names(&wb),
            vec!["完成率统计".to_string(), "明细".to_string()]
        );
    }

    #[test]
    fn wb_new_sheet_reuses_blank_sheet1() {
        let wb = ScriptWorkbook {
            inner: Rc::new(RefCell::new(umya_spreadsheet::new_file())),
            path: Rc::new(RefCell::new(PathBuf::from("untitled.xlsx"))),
        };
        assert_eq!(wb_sheet_names(&wb), vec!["Sheet1".to_string()]);
        wb_new_sheet(&wb, "完成率统计").unwrap();
        assert_eq!(wb_sheet_names(&wb), vec!["完成率统计".to_string()]);
    }

    #[test]
    fn new_and_copy_sheet() {
        let dir = std::env::temp_dir().join(format!(
            "webclaw-sheet-fns-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.xlsx");

        let mut book = umya_spreadsheet::new_file();
        book.get_sheet_mut(&0).unwrap().set_name("源表");
        book.get_sheet_by_name_mut("源表")
            .unwrap()
            .get_cell_mut((1, 1))
            .set_value_string("hello");
        umya_spreadsheet::writer::xlsx::write(&book, &path).unwrap();

        let wb = open_workbook(&path).unwrap();
        wb_new_sheet(&wb, "空白页").unwrap();
        wb_copy_sheet(&wb, "源表", "副本").unwrap();
        assert!(wb_new_sheet(&wb, "空白页").is_err());
        assert!(wb_copy_sheet(&wb, "不存在", "x").is_err());

        let names = wb_sheet_names(&wb);
        assert!(names.iter().any(|n| n == "空白页"));
        assert!(names.iter().any(|n| n == "副本"));
        let v = ws_get_cell(&wb, "副本", 1, 1).unwrap();
        assert_eq!(v.to_string(), "hello");

        let out = dir.join("out.xlsx");
        wb_save(&wb, &out).unwrap();
        assert!(out.is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_workbook_xls_alias_and_save_normalizes_ext() {
        let sample = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("data/excel/2026-06月报.xls");
        if !sample.is_file() {
            eprintln!("skip: sample xls missing");
            return;
        }
        // 请求 .xlsx 路径但磁盘上是 .xls
        let as_xlsx = sample.with_extension("xlsx");
        let wb = if as_xlsx.is_file() {
            open_workbook(&sample).unwrap()
        } else {
            open_workbook(&as_xlsx).expect("should resolve .xls sibling")
        };
        assert!(!wb_sheet_names(&wb).is_empty());

        let dir = std::env::temp_dir().join(format!(
            "webclaw-xls-save-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let want_xls = dir.join("out.xls");
        wb_save(&wb, &want_xls).unwrap();
        assert!(!want_xls.is_file(), "should not write raw .xls");
        assert!(dir.join("out.xlsx").is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_image_uses_ascii_media_name_for_chinese_file() {
        use std::fs;
        use std::io::Read;

        let dir = std::env::temp_dir().join(format!(
            "webclaw-img-embed-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // 最小合法 JPEG（1×1）
        let jpg: &[u8] = &[
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00,
            0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06,
            0x07, 0x06, 0x05, 0x08, 0x07, 0x07, 0x07, 0x09, 0x09, 0x08, 0x0A, 0x0C, 0x14, 0x0D,
            0x0C, 0x0B, 0x0B, 0x0C, 0x19, 0x12, 0x13, 0x0F, 0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D,
            0x1A, 0x1C, 0x1C, 0x20, 0x24, 0x2E, 0x27, 0x20, 0x22, 0x2C, 0x23, 0x1C, 0x1C, 0x28,
            0x37, 0x29, 0x2C, 0x30, 0x31, 0x34, 0x34, 0x34, 0x1F, 0x27, 0x39, 0x3D, 0x38, 0x32,
            0x3C, 0x2E, 0x33, 0x34, 0x32, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01, 0x00, 0x01,
            0x01, 0x01, 0x11, 0x00, 0xFF, 0xC4, 0x00, 0x14, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xFF, 0xC4,
            0x00, 0x14, 0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00,
            0x3F, 0x00, 0x7F, 0xFF, 0xD9,
        ];
        let cn = dir.join("测试图片.jpeg");
        fs::write(&cn, jpg).unwrap();

        let tpl = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("data/templates/道路清扫跟踪.xlsx");
        assert!(tpl.is_file(), "missing template");
        let out = dir.join("out.xlsx");
        let wb = open_workbook(&tpl).unwrap();
        ws_add_image(&wb, "Sheet1", 2, 1, &cn, 120).unwrap();
        wb_save(&wb, &out).unwrap();

        let file = fs::File::open(&out).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut media = Vec::new();
        for i in 0..archive.len() {
            let name = archive.by_index(i).unwrap().name().to_string();
            if name.starts_with("xl/media/") {
                media.push(name);
            }
        }
        assert!(!media.is_empty(), "xlsx has no xl/media entries");
        for m in &media {
            let leaf = m.rsplit('/').next().unwrap();
            assert!(leaf.is_ascii(), "media name not ascii: {leaf}");
            assert!(leaf.starts_with("img_"), "unexpected media name: {leaf}");
        }
        let mut entry = archive.by_name(&media[0]).unwrap();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).unwrap();
        assert_eq!(&buf[0..2], &[0xFF, 0xD8]);
        let _ = fs::remove_dir_all(&dir);
    }
}

pub fn register(engine: &mut rhai::Engine) {
    engine.register_type_with_name::<ScriptWorkbook>("Workbook");
}
