/// 机械化作业里程完成率：
/// 读取「作业过程监管日报-机械化作业」导出表，
/// 筛选 I 列车辆类型 =「全部」，按 C 列所属单位（项目公司）汇总当月
/// J 列排班作业里程、K 列作业里程，计算完成率 = 累计作业里程 / 累计排班作业里程。
///
/// 源表：downloads/作业过程监管日报-机械化作业*.xls(x)
/// 输出：files/机械化作业-里程完成率汇总.xlsx（工作表「里程完成率汇总」）
///
/// 用法: 脚本机械化作业里程完成率
///       脚本机械化作业里程完成率 [项目公司关键字]
///   无参数：汇总全部项目公司；有参数：仅保留所属单位名称包含该关键字的公司。

/// 完成率百分比固定 2 位并带 %（如 97.25%）；排班里程为 0 时返回 "-"
fn format_rate_pct(rate) {
    if rate == () {
        return "-";
    }
    rate
}

/// 完成率 = 作业/排班 → 百分比字符串（两位小数 + %）；排班为 0 时返回 ()
fn rate_of(work_km, sched_km) {
    if sched_km == 0.0 {
        return ();
    }
    // *100 转百分比，再 *100 保留两位小数
    let scaled = ((work_km / sched_km) * 10000.0).round();
    let neg = scaled < 0.0;
    let abs = if neg { 0.0 - scaled } else { scaled };
    let ip = (abs / 100.0).floor();
    let fp = abs - ip * 100.0;
    let frac = to_str(fp);
    let pad = "";
    let n = frac.len();
    let i = 0;
    while i + n < 2 {
        pad += "0";
        i += 1;
    }
    let s = to_str(ip) + "." + pad + frac + "%";
    if neg {
        return "-" + s;
    }
    s
}

fn push_unique_str(arr, item) {
    let i = 0;
    while i < arr.len() {
        if arr[i] == item {
            return arr;
        }
        i += 1;
    }
    arr.push(item);
    arr
}

fn find_mech_daily() {
    let p = find_file_optional("downloads", "^作业过程监管日报-机械化作业.*\\.xlsx$");
    if p == "" {
        p = find_file_optional("downloads", "^作业过程监管日报-机械化作业.*\\.xls$");
    }
    if p == "" {
        p = find_file_optional("files", "^作业过程监管日报-机械化作业.*\\.xlsx$");
    }
    if p == "" {
        p = find_file_optional("excel", "^作业过程监管日报-机械化作业.*\\.xlsx$");
    }
    p
}

print("机械化作业里程完成率");
print("────────────────────────────────────────");

let company_kw = str_trim(arg(0));
let src = find_mech_daily();
if src == "" {
    print("⚠ 未找到「作业过程监管日报-机械化作业」导出表");
    print("  请先下载到 downloads/，文件名需以「作业过程监管日报-机械化作业」开头");
    return;
}

print("源表: " + basename(src));
if company_kw != "" {
    print("项目公司关键字: " + company_kw);
}

let table = read_table(src, ());
let h_date = col_index(table, 0, "日期");
let h_company = col_index(table, 0, "所属单位");
let h_vehicle = col_index(table, 0, "B车辆类型");
let h_sched = col_index(table, 0, "B里程完成率-排班作业里程（千米）");
let h_work = col_index(table, 0, "B里程完成率-作业里程（千米）");

if h_company < 0 || h_vehicle < 0 || h_sched < 0 || h_work < 0 {
    throw "表头缺少必要列：所属单位 / B车辆类型 / 排班作业里程 / 作业里程";
}

let companies = [];
let sched_map = #{};
let work_map = #{};
let rows_map = #{};
let date_min = "";
let date_max = "";
let matched_rows = 0;
let skipped_not_all = 0;

let r = 1;
let total = table_rows(table);
while r < total {
    let vehicle = str_trim(to_str(table_get(table, r, h_vehicle)));
    if vehicle != "全部" {
        skipped_not_all += 1;
        r += 1;
        continue;
    }

    let company = str_trim(to_str(table_get(table, r, h_company)));
    if company == "" {
        r += 1;
        continue;
    }
    if company_kw != "" && !company.contains(company_kw) {
        r += 1;
        continue;
    }

    if h_date >= 0 {
        let d = str_trim(to_str(table_get(table, r, h_date)));
        if d != "" {
            if date_min == "" || d < date_min { date_min = d; }
            if date_max == "" || d > date_max { date_max = d; }
        }
    }

    let sched = to_num(table_get(table, r, h_sched));
    let work = to_num(table_get(table, r, h_work));

    if !sched_map.contains(company) {
        companies = push_unique_str(companies, company);
        sched_map[company] = 0.0;
        work_map[company] = 0.0;
        rows_map[company] = 0;
    }
    sched_map[company] = sched_map[company] + sched;
    work_map[company] = work_map[company] + work;
    rows_map[company] = rows_map[company] + 1;
    matched_rows += 1;
    r += 1;
}

if companies.len() == 0 {
    print("⚠ 筛选后无数据（I 列车辆类型需为「全部」）");
    return;
}

print("日期范围: " + date_min + " ~ " + date_max);
print("车型筛选: 全部（跳过其他车型行 " + skipped_not_all + "）");
print("命中明细行: " + matched_rows + "，项目公司数: " + companies.len());
print("");

// 源表多为 BIFF .xls：先按 .xls 落盘再打开（避免误用 .xlsx 扩展名导致 Zip 解析失败）
let work = "files/_mech_daily_work.xls";
let out = "files/机械化作业-里程完成率汇总.xlsx";
copy_file(src, work);
let wb = open_workbook(work);
let sheet = "里程完成率汇总";
wb_new_sheet(wb, sheet);

ws_set_cell(wb, sheet, 1, 1, "机械化作业-当月里程完成率汇总");
ws_set_cell(wb, sheet, 2, 1, "源表");
ws_set_cell(wb, sheet, 2, 2, basename(src));
ws_set_cell(wb, sheet, 3, 1, "日期范围");
ws_set_cell(wb, sheet, 3, 2, date_min + " ~ " + date_max);
ws_set_cell(wb, sheet, 4, 1, "车型筛选");
ws_set_cell(wb, sheet, 4, 2, "全部");

ws_set_cell(wb, sheet, 6, 1, "序号");
ws_set_cell(wb, sheet, 6, 2, "项目公司");
ws_set_cell(wb, sheet, 6, 3, "累计排班作业里程(千米)");
ws_set_cell(wb, sheet, 6, 4, "累计作业里程(千米)");
ws_set_cell(wb, sheet, 6, 5, "当月作业里程完成率");
ws_set_cell(wb, sheet, 6, 6, "明细行数");

let total_sched = 0.0;
let total_work = 0.0;
let i = 0;
while i < companies.len() {
    let company = companies[i];
    let sched = round2(sched_map[company]);
    let work_km = round2(work_map[company]);
    let rate = rate_of(work_km, sched);
    let rate_text = format_rate_pct(rate);
    let rows = rows_map[company];
    total_sched += sched;
    total_work += work_km;

    let out_row = 7 + i;
    ws_set_cell(wb, sheet, out_row, 1, i + 1);
    ws_set_cell(wb, sheet, out_row, 2, company);
    ws_set_cell(wb, sheet, out_row, 3, sched);
    ws_set_cell(wb, sheet, out_row, 4, work_km);
    if rate == () {
        ws_set_cell(wb, sheet, out_row, 5, "");
    } else {
        // 写入百分比文本，如 97.25%
        ws_set_cell(wb, sheet, out_row, 5, rate);
    }
    ws_set_cell(wb, sheet, out_row, 6, rows);

    print(
        (i + 1) + ". " + company
            + " | 排班=" + sched
            + " | 作业=" + work_km
            + " | 完成率=" + rate_text
            + " | 行数=" + rows
    );
    i += 1;
}

let sum_row = 7 + companies.len();
let total_rate = rate_of(total_work, total_sched);
ws_set_cell(wb, sheet, sum_row, 1, "");
ws_set_cell(wb, sheet, sum_row, 2, "合计");
ws_set_cell(wb, sheet, sum_row, 3, round2(total_sched));
ws_set_cell(wb, sheet, sum_row, 4, round2(total_work));
if total_rate == () {
    ws_set_cell(wb, sheet, sum_row, 5, "");
} else {
    ws_set_cell(wb, sheet, sum_row, 5, total_rate);
}
ws_set_cell(wb, sheet, sum_row, 6, matched_rows);

print("");
print(
    "合计 | 排班=" + round2(total_sched)
        + " | 作业=" + round2(total_work)
        + " | 完成率=" + format_rate_pct(total_rate)
);

wb_save(wb, out);
track_output(out);
print("");
print("✓ 已生成: " + out);
print("  工作表「" + sheet + "」含 " + companies.len() + " 家项目公司");
