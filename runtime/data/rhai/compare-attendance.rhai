/// 考勤数据对比（内置）：
/// 从「加班工时申请-*.zip」解压出各公司「考勤记录表」与「计划外加班工时汇总表」，
/// 按公司+月份配对，对比加班时长合计，给出审核意见。
///
/// 用法：脚本考勤数据对比
///       脚本考勤数据对比 陈倩
/// ZIP 查找顺序：downloads/files → zip → excel；可用 ARGS 关键字过滤文件名。

fn cell_str(wb, sheet, row, col) {
    to_str(ws_get_cell(wb, sheet, row, col))
}

fn cell_num(wb, sheet, row, col) {
    to_num(ws_get_cell(wb, sheet, row, col))
}

fn pick_summary_sheet(names) {
    let i = 0;
    while i < names.len() {
        let n = to_str(names[i]);
        if n.contains("汇总") {
            return n;
        }
        i += 1;
    }
    if names.len() > 0 {
        return to_str(names[0]);
    }
    ""
}

fn find_header_row_ot(wb, sheet) {
    let r = 1;
    while r <= 5 {
        let c = 1;
        while c <= 60 {
            let v = cell_str(wb, sheet, r, c);
            if v.contains("专项加班") {
                return r;
            }
            c += 1;
        }
        r += 1;
    }
    0
}

fn collect_ot_cols(wb, sheet, header_row) {
    let cols = [];
    let c = 1;
    while c <= 60 {
        let v = cell_str(wb, sheet, header_row, c);
        if v.contains("专项加班") || v.contains("临时加班") || v.contains("夜保加班")
            || v.contains("休息日加班") || v.contains("法定") {
            cols.push(c);
        }
        c += 1;
    }
    cols
}

/// 考勤记录表：汇总五类加班工时
fn sum_attendance_ot_hours(path) {
    let wb = open_workbook(path);
    let sheets = wb_sheet_names(wb);
    let sheet = pick_summary_sheet(sheets);
    if sheet == "" {
        return #{ ok: false, hours: 0.0, people: 0, sheet: "", detail: "无工作表" };
    }
    let header_row = find_header_row_ot(wb, sheet);
    if header_row <= 0 {
        return #{ ok: false, hours: 0.0, people: 0, sheet: sheet, detail: "未找到加班列表头" };
    }
    let ot_cols = collect_ot_cols(wb, sheet, header_row);
    if ot_cols.len() == 0 {
        return #{ ok: false, hours: 0.0, people: 0, sheet: sheet, detail: "未识别加班列" };
    }
    let total = 0.0;
    let people = 0;
    let r = header_row + 1;
    // 扫描足够行；遇连续空行可提前停，这里按常见规模扫到 3000
    let empty_streak = 0;
    while r <= header_row + 3000 {
        let name = cell_str(wb, sheet, r, 6);
        let id_or_dept = cell_str(wb, sheet, r, 5);
        if name == "" && id_or_dept == "" {
            empty_streak += 1;
            if empty_streak >= 20 { break; }
            r += 1;
            continue;
        }
        empty_streak = 0;
        // 跳过表尾说明行（无姓名且部门也不像数据）
        if name == "" {
            r += 1;
            continue;
        }
        people += 1;
        let ci = 0;
        while ci < ot_cols.len() {
            total += cell_num(wb, sheet, r, ot_cols[ci]);
            ci += 1;
        }
        r += 1;
    }
    #{ ok: true, hours: round2(total), people: people, sheet: sheet, detail: "" }
}

fn find_hours_col(wb, sheet) {
    let r = 1;
    while r <= 6 {
        let c = 1;
        while c <= 20 {
            let v = cell_str(wb, sheet, r, c);
            if v.contains("本月时长") {
                return c;
            }
            c += 1;
        }
        r += 1;
    }
    0
}

/// 计划外加班工时汇总表：取「总计」行的本月时长
fn read_ot_summary_total(path) {
    let wb = open_workbook(path);
    let sheets = wb_sheet_names(wb);
    let sheet = pick_summary_sheet(sheets);
    if sheet == "" {
        return #{ ok: false, hours: 0.0, sheet: "", detail: "无工作表" };
    }
    let hours_col = find_hours_col(wb, sheet);
    if hours_col <= 0 {
        return #{ ok: false, hours: 0.0, sheet: sheet, detail: "未找到「本月时长」列" };
    }
    let r = 1;
    while r <= 30 {
        let a = cell_str(wb, sheet, r, 1);
        if a.contains("总计") {
            let h = round2(cell_num(wb, sheet, r, hours_col));
            return #{ ok: true, hours: h, sheet: sheet, detail: "" };
        }
        r += 1;
    }
    #{ ok: false, hours: 0.0, sheet: sheet, detail: "未找到「总计」行" }
}

fn find_zip_file(keyword) {
    let pattern = "^加班工时申请.*\\.zip$";
    if keyword != "" {
        pattern = ".*" + keyword + ".*\\.zip$";
    }
    let p = find_file_optional("downloads", pattern);
    if p == "" { p = find_file_optional("zip", pattern); }
    if p == "" { p = find_file_optional("excel", pattern); }
    if p == "" && keyword == "" {
        p = find_file_optional("downloads", ".*\\.zip$");
        if p == "" { p = find_file_optional("zip", ".*\\.zip$"); }
    }
    p
}

fn pair_key(company, month) {
    company + "\t" + month
}

// ─── 主流程 ───────────────────────────────────────────────────────────────

print("════════════════════════════════════════");
print("  考勤数据对比");
print("════════════════════════════════════════");
print("参数 ARGS: [" + ARGS + "]");

let keyword = "";
if ARGS != "" {
    // 取第一段非空 token 作 ZIP 文件名关键字
    let raw = ARGS;
    let i = 0;
    let buf = "";
    while i < raw.len() {
        let ch = raw.sub_string(i, 1);
        if ch == " " || ch == "\t" {
            if buf != "" { break; }
        } else {
            buf += ch;
        }
        i += 1;
    }
    keyword = buf;
}

let zip_path = find_zip_file(keyword);
if zip_path == "" {
    print("⚠ 未找到加班工时申请 ZIP（downloads/files、zip、excel）");
    print("  期望文件名形如：加班工时申请-姓名-日期_*.zip");
    return;
}
print("ZIP: " + basename(zip_path));

let dest = "files/_unzip_attendance_compare";
let extracted = extract_zip(zip_path, dest);
print("解压 " + extracted.len() + " 个文件 → " + dest);

let att_map = #{};
let ot_map = #{};
let att_paths = #{};
let ot_paths = #{};
let keys = [];
let ei = 0;
while ei < extracted.len() {
    let path = to_str(extracted[ei]);
    let name = basename(path);
    let att = parse_attendance_record_name(name);
    if att.ok {
        let key = pair_key(att.company, att.month);
        if !att_map.contains(key) {
            keys.push(key);
        }
        att_map[key] = true;
        att_paths[key] = path;
        print("  考勤记录表: " + att.company + " / " + att.month);
    }
    let ot = parse_ot_summary_name(name);
    if ot.ok {
        let key = pair_key(ot.company, ot.month);
        ot_map[key] = true;
        ot_paths[key] = path;
        print("  加班汇总表: " + ot.company + " / " + ot.month);
    }
    ei += 1;
}

// 仅保留两边都有的键
let paired = [];
let missing_ot = [];
let missing_att = [];
let ki0 = 0;
while ki0 < keys.len() {
    let key = keys[ki0];
    if ot_map.contains(key) {
        paired.push(key);
    } else {
        missing_ot.push(key);
    }
    ki0 += 1;
}
// OT 有而 ATT 无：扫 ot_paths 键不在 att_map
// （ot 键可能未进 keys）——再扫一遍 extracted
ei = 0;
while ei < extracted.len() {
    let path = to_str(extracted[ei]);
    let ot = parse_ot_summary_name(basename(path));
    if ot.ok {
        let key = pair_key(ot.company, ot.month);
        if !att_map.contains(key) {
            let seen = false;
            let j = 0;
            while j < missing_att.len() {
                if missing_att[j] == key { seen = true; break; }
                j += 1;
            }
            if !seen { missing_att.push(key); }
        }
    }
    ei += 1;
}

if paired.len() == 0 {
    print("⚠ 未能配对任何「公司+月份」的考勤记录表与加班汇总表");
    if missing_ot.len() > 0 {
        print("  仅有考勤记录表: " + missing_ot.len() + " 组");
    }
    if missing_att.len() > 0 {
        print("  仅有加班汇总表: " + missing_att.len() + " 组");
    }
    return;
}

print("");
print("开始对比 " + paired.len() + " 组…");
let report = "考勤数据对比审核意见\n";
report += "来源ZIP: " + basename(zip_path) + "\n";
report += "────────────────────────────────────────\n";

let pass_n = 0;
let fail_n = 0;
let ki = 0;
while ki < paired.len() {
    let key = paired[ki];
    let att_path = att_paths[key];
    let ot_path = ot_paths[key];
    let meta = parse_attendance_record_name(basename(att_path));
    let company = meta.company;
    let month = meta.month;

    print("");
    print("【" + company + " " + month + "】");
    let att_sum = sum_attendance_ot_hours(att_path);
    let ot_sum = read_ot_summary_total(ot_path);

    if !att_sum.ok {
        print("  ✗ 考勤记录表读取失败: " + att_sum.detail);
        report += company + " " + month + "：考勤记录表读取失败（" + att_sum.detail + "）→ 不通过\n";
        fail_n += 1;
        ki += 1;
        continue;
    }
    if !ot_sum.ok {
        print("  ✗ 加班汇总表读取失败: " + ot_sum.detail);
        report += company + " " + month + "：加班汇总表读取失败（" + ot_sum.detail + "）→ 不通过\n";
        fail_n += 1;
        ki += 1;
        continue;
    }

    let diff = round2(att_sum.hours - ot_sum.hours);
    print("  考勤记录表加班合计: " + att_sum.hours + " 小时（" + att_sum.people + " 人，表 " + att_sum.sheet + "）");
    print("  加班汇总表总计时长: " + ot_sum.hours + " 小时（表 " + ot_sum.sheet + "）");
    print("  差额: " + diff + " 小时");

    let opinion = "";
    if abs_num(diff) < 0.05 {
        opinion = "通过（加班时长一致）";
        pass_n += 1;
        print("  审核意见: " + opinion);
    } else {
        opinion = "不通过（加班时长不一致，差额 " + diff + " 小时）";
        fail_n += 1;
        print("  审核意见: " + opinion);
    }
    report += company + " " + month + "：考勤 " + att_sum.hours + " / 汇总 " + ot_sum.hours
        + " / 差额 " + diff + " → " + opinion + "\n";
    ki += 1;
}

if missing_ot.len() > 0 || missing_att.len() > 0 {
    report += "────────────────────────────────────────\n";
    report += "未配对文件：\n";
    let mi = 0;
    while mi < missing_ot.len() {
        report += "  缺加班汇总: " + missing_ot[mi] + "\n";
        mi += 1;
    }
    mi = 0;
    while mi < missing_att.len() {
        report += "  缺考勤记录表: " + missing_att[mi] + "\n";
        mi += 1;
    }
}

report += "────────────────────────────────────────\n";
report += "合计: 通过 " + pass_n + " / 不通过 " + fail_n + "\n";
if fail_n == 0 && pass_n > 0 {
    report += "总审核意见: 全部通过\n";
} else if pass_n == 0 {
    report += "总审核意见: 全部不通过\n";
} else {
    report += "总审核意见: 部分不通过，请核查差额公司\n";
}

let out_path = "files/考勤数据对比审核意见.txt";
write_text_file(out_path, report);
track_output(out_path);

print("");
print("────────────────────────────────────────");
print("通过 " + pass_n + " 组，不通过 " + fail_n + " 组");
if fail_n == 0 && pass_n > 0 {
    print("总审核意见: 全部通过");
} else if pass_n == 0 {
    print("总审核意见: 全部不通过");
} else {
    print("总审核意见: 部分不通过");
}
print("审核意见已写入: " + out_path);
print("════════════════════════════════════════");
