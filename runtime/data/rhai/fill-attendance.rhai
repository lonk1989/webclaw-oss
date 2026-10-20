/// 考勤记录表填充（内置「合并考勤数据」）：
/// 运营考勤月报 + 一线月报/人员信息/加班台账/请假登记/加班排班/日报 → 一次生成已填充表。
///
/// 模板: 设置 → 模板（默认 考勤记录表-XXX.xlsx）
/// 输出: files/考勤记录表-{部门}-已填充.xlsx
///
/// 用法: 脚本合并考勤数据 [公司] [月份]
///   例: 脚本合并考勤数据 山西阳泉 2026-06
///
/// 人员：运营月报 + 一线月报，冲突时一线优先
/// 日历优先级：请假登记事由 → 加班排班出勤 → 日报（姓名+责任部门 ↔ 结果姓名+部门）→ 月报/默认出勤
/// 重名：日报按姓名+部门；请假/排班/HR/加班在有工号时禁止按姓名兜底
/// 加班五列：一线月报 / 二线加班台账；排班月工时覆盖休息日或法定列
/// 入离职：入职前与离职后日历格留空（入职日、离职日仍计入）
/// 非全日制：岗位名称含「（非全）」的人员不入表（其他系统核算）
/// 姓名：读入时去掉空格（两字名对齐空格等）
/// 备注：Q（其他）类假的具体类型，逗号分隔、去重
/// 日期列：模板固定 31 天，填充后删除当月不存在的空白日期列

fn hours_to_days(hours, hours_per_day) {
    if hours == 0.0 { return 0.0; }
    ((hours / hours_per_day) * 100.0).round() / 100.0
}

fn status_symbol(status, symbols) {
    let s = to_str(status);
    if s == "" { return ""; }
    if symbols.contains(s) {
        return symbols[s];
    }
    // 产假/陪产假等：与请假登记同一套符号（Q 等），避免格子里留下中文原文
    let leave_sym = leave_type_to_symbol(s);
    if leave_sym != "" {
        return leave_sym;
    }
    ""
}

fn leave_type_to_symbol(leave_type) {
    let t = to_str(leave_type);
    if t == "" { return ""; }
    if t == "事假" { return "S"; }
    if t == "病假" { return "B"; }
    if t == "年假" || t == "调休" || t == "休息" || t == "休假" { return "R"; }
    if t == "工伤" || t == "工伤假" { return "G"; }
    if t == "旷工" { return "X"; }
    if t == "迟到" { return "C"; }
    if t == "早退" { return "T"; }
    // 图例 Q=其他：婚假/丧假/陪产假/哺乳假/产假/产检假/探亲假等
    if t == "婚假" || t == "丧假" || t == "陪产假" || t == "哺乳假"
        || t == "产假" || t == "产检假" || t == "探亲假" || t == "其他假" || t == "其他" {
        return "Q";
    }
    // 笼统「请假」须由请假登记事由对齐，不直接落成 Q
    if t == "请假" { return ""; }
    if t.contains("假") { return "Q"; }
    ""
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

fn join_comma(arr) {
    let s = "";
    let i = 0;
    while i < arr.len() {
        if i > 0 { s += ","; }
        s += arr[i];
        i += 1;
    }
    s
}

/// 汇总当月落成 Q 的请假类型名（去重，逗号分隔）写入备注
fn build_other_leave_remark(user_leave, user_daily, emp, month, info) {
    if month == "" || month.len() < 7 {
        return "";
    }
    let dim = days_in_month(to_num(month.sub_string(0, 4)), to_num(month.sub_string(5, 2)));
    let types = [];
    let d = 0;
    while d < dim {
        let day_date = format_day_date(month, d + 1.0);
        if !is_employed_on(day_date, info.hire_date, info.resign_date) {
            d += 1;
            continue;
        }
        let label = "";
        if user_leave != () && user_leave.contains(day_date) {
            label = to_str(user_leave[day_date]);
        } else if user_daily != () && user_daily.contains(day_date) {
            label = to_str(user_daily[day_date]);
        } else {
            label = to_str(emp.daily[d]);
        }
        if leave_type_to_symbol(label) == "Q" {
            types = push_unique_str(types, label);
        }
        d += 1;
    }
    join_comma(types)
}

fn other_leave_hours(emp) {
    emp.annual_hours + emp.maternity_hours + emp.marriage_hours
        + emp.bereave_hours + emp.paternity_hours + emp.nursing_hours
        + emp.prenatal_hours
}

fn empty_daily() {
    let daily = [];
    let d = 0;
    while d < 31 {
        daily.push("");
        d += 1;
    }
    daily
}

/// 一线月报无逐日状态：默认先按出勤打勾，本脚本内再用请假/排班/日报覆盖
fn default_frontline_daily() {
    let daily = [];
    let d = 0;
    while d < 31 {
        daily.push("出勤");
        d += 1;
    }
    daily
}

/// 入职/离职日期：支持字符串与 Excel 序列日（避免身份证/日期被 UTF-16 误修或显示成 44927）
fn format_date(v) {
    if v == () { return ""; }
    let s = to_str(v);
    if s == "" { return ""; }
    let n = to_num(v);
    if n >= 20000.0 && n <= 80000.0 && !s.contains("-") && !s.contains("/") && !s.contains("年") {
        return excel_serial_ymd(n);
    }
    if s.len() >= 10 && (s.sub_string(4, 1) == "-" || s.sub_string(4, 1) == "/") {
        return s.sub_string(0, 4) + "-" + s.sub_string(5, 2) + "-" + s.sub_string(8, 2);
    }
    if s.len() >= 10 { return s.sub_string(0, 10); }
    s
}

fn format_day_date(month_full, day_num) {
    let ym = month_full.sub_string(0, 7);
    let d_str = "";
    if day_num < 10.0 {
        d_str = "0" + to_str(day_num);
    } else {
        d_str = to_str(day_num);
    }
    ym + "-" + d_str
}

/// 入职当天与离职当天仍计出勤；入职前、离职后不打勾
fn is_employed_on(date, hire_date, resign_date) {
    if hire_date != "" && date < hire_date {
        return false;
    }
    if resign_date != "" && date > resign_date {
        return false;
    }
    true
}

/// 去掉姓名中的空格（两字名对齐用的中间空格、首尾空白等）
fn normalize_name(v) {
    let s = to_str(v);
    if s == "" { return ""; }
    let out = "";
    let i = 0;
    while i < s.len() {
        let ch = s.sub_string(i, 1);
        if ch != " " && ch != "\t" && ch != "　" {
            out += ch;
        }
        i += 1;
    }
    out
}

fn make_employee(table, i, h_name, h_org, daily, h_should, h_absent, h_personal, h_annual, h_sick,
    h_maternity, h_paternity, h_bereave, h_injury, h_nursing, h_marriage, h_prenatal,
    h_actual, h_incomplete, h_late, h_early, h_missed, h_ot, code) {
    #{
        code: code,
        name: normalize_name(table_get(table, i, h_name)),
        org: to_str(table_get(table, i, h_org)),
        daily: daily,
        should_hours: to_num(table_get(table, i, h_should)),
        absent_hours: to_num(table_get(table, i, h_absent)),
        personal_hours: to_num(table_get(table, i, h_personal)),
        annual_hours: to_num(table_get(table, i, h_annual)),
        sick_hours: to_num(table_get(table, i, h_sick)),
        maternity_hours: to_num(table_get(table, i, h_maternity)),
        paternity_hours: to_num(table_get(table, i, h_paternity)),
        bereave_hours: to_num(table_get(table, i, h_bereave)),
        injury_hours: to_num(table_get(table, i, h_injury)),
        nursing_hours: to_num(table_get(table, i, h_nursing)),
        marriage_hours: to_num(table_get(table, i, h_marriage)),
        prenatal_hours: to_num(table_get(table, i, h_prenatal)),
        actual_hours: to_num(table_get(table, i, h_actual)),
        incomplete_hours: to_num(table_get(table, i, h_incomplete)),
        late_count: to_num(table_get(table, i, h_late)),
        early_count: to_num(table_get(table, i, h_early)),
        missed_punch: to_num(table_get(table, i, h_missed)),
        overtime_hours: to_num(table_get(table, i, h_ot)),
    }
}

/// 读取考勤月报全部人员（不按考勤组织过滤）
fn load_monthly(path) {
    let table = read_table(path, ());
    let h_code = col_index(table, 0, "编码");
    let h_name = col_index(table, 0, "姓名");
    let h_org = col_index(table, 0, "考勤组织");
    let h_should = col_index(table, 0, "应出勤工时");
    let h_absent = col_index(table, 0, "旷工工时数");
    let h_personal = col_index(table, 0, "事假工时数");
    let h_annual = col_index(table, 0, "年假工时数");
    let h_sick = col_index(table, 0, "病假工时数");
    let h_maternity = col_index(table, 0, "产假工时数");
    let h_paternity = col_index(table, 0, "陪产假工时数");
    let h_bereave = col_index(table, 0, "丧假工时数");
    let h_injury = col_index(table, 0, "工伤假工时数");
    let h_nursing = col_index(table, 0, "哺乳假工时数");
    let h_marriage = col_index(table, 0, "婚假工时数");
    let h_prenatal = col_index(table, 0, "产检假工时数");
    let h_actual = col_index(table, 0, "实际出勤工时数");
    let h_incomplete = col_index(table, 0, "因入离职未出勤工时数");
    let h_late = col_index(table, 0, "迟到次数");
    let h_early = col_index(table, 0, "早退次数");
    let h_missed = col_index(table, 0, "漏打卡次数");
    let h_ot = col_index(table, 0, "加班工时");

    let employees = [];
    let total_rows = table_rows(table);
    let i = 2;
    while i < total_rows {
        let code = table_get(table, i, h_code);
        if code != () && to_str(code) != "" {
            let daily = [];
            let d = 0;
            while d < 31 {
                daily.push(to_str(table_get(table, i + 1, 3 + d)));
                d += 1;
            }
            employees.push(make_employee(table, i, h_name, h_org, daily, h_should, h_absent, h_personal,
                h_annual, h_sick, h_maternity, h_paternity, h_bereave, h_injury, h_nursing,
                h_marriage, h_prenatal, h_actual, h_incomplete, h_late, h_early, h_missed,
                h_ot, to_str(code)));
        }
        i += 2;
    }
    employees
}

fn load_leaves(path) {
    if path == "" { return []; }
    let table = read_table(path, ());
    let records = [];
    let total = table_rows(table);
    let i = 2;
    while i < total {
        let status = to_str(table_get(table, i, 8));
        let doc_status = to_str(table_get(table, i, 10));
        if status == "审批通过" && doc_status == "生效" {
            records.push(#{
                name: normalize_name(table_get(table, i, 1)),
                type: to_str(table_get(table, i, 3)),
                start: to_str(table_get(table, i, 4)),
                hours: to_num(table_get(table, i, 6)),
            });
        }
        i += 1;
    }
    records
}

/// 请假登记.xls：姓名/编码 -> 日期 -> 请假事由（日历符号对齐用）
fn load_leave_register(path, month_ym) {
    if path == "" { return #{ by_name: #{}, by_code: #{} }; }
    let table = read_table(path, ());
    let h_name = col_index(table, 0, "员工姓名");
    let h_code = col_index(table, 0, "员工编码");
    let h_date = col_index(table, 0, "请假日期");
    let h_type = col_index(table, 0, "请假事由");
    let by_name = #{};
    let by_code = #{};
    let total = table_rows(table);
    let i = 1;
    while i < total {
        let name = normalize_name(table_get(table, i, h_name));
        let code = to_str(table_get(table, i, h_code));
        let date = format_date(table_get(table, i, h_date));
        let leave_type = to_str(table_get(table, i, h_type));
        if date == "" || leave_type == "" || (name == "" && code == "") {
            i += 1;
            continue;
        }
        if month_ym != "" && (date.len() < 7 || date.sub_string(0, 7) != month_ym) {
            i += 1;
            continue;
        }
        if name != "" {
            if !by_name.contains(name) { by_name[name] = #{}; }
            by_name[name][date] = leave_type;
        }
        if code != "" {
            if !by_code.contains(code) { by_code[code] = #{}; }
            by_code[code][date] = leave_type;
        }
        i += 1;
    }
    #{ by_name: by_name, by_code: by_code }
}

fn lookup_leave_days(leave_data, code, name) {
    // 有工号时只按工号匹配，禁止姓名兜底（同名不同人会串请假，如两名李素琴）
    if code != "" {
        if leave_data.by_code.contains(code) {
            return leave_data.by_code[code];
        }
        return ();
    }
    if name != "" && leave_data.by_name.contains(name) {
        return leave_data.by_name[name];
    }
    ()
}

fn is_normal_status(status) {
    let s = to_str(status);
    s == "正常" || s == "异常" || s == "出勤" || s == "出差" || s == "外出"
}

/// 日报无工号：用「姓名+部门」区分重名（与结果表姓名+部门列对齐）
fn name_dept_key(name, dept) {
    normalize_name(name) + "\t" + to_str(dept)
}

fn load_daily_status(path, month_ym) {
    if path == "" { return #{}; }
    let table = read_table(path, ());
    let h_name = col_index(table, 0, "姓名");
    let h_dept = col_index(table, 0, "责任部门");
    let h_date = col_index(table, 0, "日期");
    let h_status = col_index(table, 0, "日考勤结果");
    let by_key = #{};
    let total = table_rows(table);
    let i = 1;
    while i < total {
        let name = normalize_name(table_get(table, i, h_name));
        let dept = to_str(table_get(table, i, h_dept));
        let date = format_date(table_get(table, i, h_date));
        let status = to_str(table_get(table, i, h_status));
        if name == "" || date == "" || status == "" {
            i += 1;
            continue;
        }
        if month_ym != "" && (date.len() < 7 || date.sub_string(0, 7) != month_ym) {
            i += 1;
            continue;
        }
        let key = name_dept_key(name, dept);
        if !by_key.contains(key) {
            by_key[key] = #{};
        }
        by_key[key][date] = status;
        i += 1;
    }
    by_key
}

/// 日历优先级：请假登记 → 加班排班 → 日报
fn resolve_day_symbol(user_daily, user_leave, user_ot_days, target_date) {
    if user_leave != () && user_leave.contains(target_date) {
        let sym = leave_type_to_symbol(user_leave[target_date]);
        if sym != "" { return sym; }
    }
    if user_ot_days != () && user_ot_days.contains(target_date) {
        return "√";
    }
    if user_daily == () || !user_daily.contains(target_date) {
        return "";
    }
    let status = to_str(user_daily[target_date]);
    if status == "" { return ""; }
    if is_normal_status(status) { return "√"; }
    let sym = leave_type_to_symbol(status);
    if sym != "" { return sym; }
    // 「请假」等无法定符号的状态：不覆盖，留给请假登记/月报
    ""
}

fn schedule_header_to_date(h, month_ym) {
    if month_ym.len() < 7 || h == "" { return ""; }
    let dash = -1;
    let i = 0;
    while i < h.len() {
        if h.sub_string(i, 1) == "-" {
            dash = i;
            break;
        }
        i += 1;
    }
    if dash <= 0 { return ""; }
    let mon = h.sub_string(0, dash);
    let day = h.sub_string(dash + 1, h.len() - dash - 1);
    if mon == "" || day == "" { return ""; }
    if to_num(mon) != to_num(month_ym.sub_string(5, 2)) { return ""; }
    let day_n = to_num(day);
    if day_n < 1.0 || day_n > 31.0 { return ""; }
    format_day_date(month_ym, day_n)
}

/// 加班排班记录：日期出勤标记 + 月工时（法定/节日 → holiday，否则 restday）
fn load_ot_schedule(path, month_ym) {
    if path == "" {
        return #{ by_code: #{}, by_name: #{} };
    }
    let table = read_table(path, ());
    let h_code = col_index(table, 0, "人员编号");
    let h_name = col_index(table, 0, "人员姓名");
    let h_hours = col_index(table, 0, "月工时合计");
    let day_cols = [];
    let c = 0;
    while c < 100 {
        let h = to_str(table_get(table, 0, c));
        let date = schedule_header_to_date(h, month_ym);
        if date != "" {
            day_cols.push(#{ col: c, date: date });
        }
        c += 1;
    }
    let by_code = #{};
    let by_name = #{};
    let total = table_rows(table);
    let i = 1;
    while i < total {
        let code = to_str(table_get(table, i, h_code));
        let name = normalize_name(table_get(table, i, h_name));
        let hours = to_num(table_get(table, i, h_hours));
        let days = #{};
        let hit = 0;
        let holiday_mark = false;
        let di = 0;
        while di < day_cols.len() {
            let mark = to_str(table_get(table, i, day_cols[di].col));
            if mark != "" {
                days[day_cols[di].date] = true;
                hit += 1;
                if mark.contains("法定") || mark.contains("节日") || mark.contains("节假") {
                    holiday_mark = true;
                }
            }
            di += 1;
        }
        if hit > 0 || hours != 0.0 {
            let rec = #{ days: days, hours: hours, holiday: holiday_mark };
            if code != "" { by_code[code] = rec; }
            if name != "" { by_name[name] = rec; }
        }
        i += 1;
    }
    #{ by_code: by_code, by_name: by_name }
}

fn lookup_ot_schedule(sched, code, name) {
    // 有工号时只按工号匹配，避免重名串排班
    if code != "" {
        if sched.by_code.contains(code) {
            return sched.by_code[code];
        }
        return ();
    }
    if name != "" && sched.by_name.contains(name) {
        return sched.by_name[name];
    }
    ()
}

fn find_source_xls(basename_re) {
    let p = find_file_optional("downloads", basename_re);
    if p == "" {
        p = find_file_optional("excel", basename_re);
    }
    p
}

fn load_hr_info(path) {
    if path == "" { return #{ by_code: #{}, by_name: #{} }; }
    let table = read_table(path, ());
    let h_code = col_index(table, 0, "员工编号");
    let h_name = col_index(table, 0, "姓名");
    let h_position = col_index(table, 0, "岗位");
    let h_id = col_index(table, 0, "身份证号");
    let h_hire = col_index(table, 0, "入职日期");
    let h_resign = col_index(table, 0, "离职时间");

    let by_code = #{};
    let by_name = #{};
    let total = table_rows(table);
    let i = 2;
    while i < total {
        let code = to_str(table_get(table, i, h_code));
        if code != "" {
            let rec = #{
                position: to_str(table_get(table, i, h_position)),
                id_card: to_str(table_get(table, i, h_id)),
                hire_date: format_date(table_get(table, i, h_hire)),
                resign_date: format_date(table_get(table, i, h_resign)),
            };
            by_code[code] = rec;
            let name = normalize_name(table_get(table, i, h_name));
            if name != "" {
                by_name[name] = rec;
            }
        }
        i += 1;
    }
    #{ by_code: by_code, by_name: by_name }
}

fn lookup_hr(hr, code, name) {
    // 有工号时只按工号，避免重名串入职/岗位信息
    if code != "" {
        if hr.by_code.contains(code) {
            return hr.by_code[code];
        }
    } else if name != "" && hr.by_name.contains(name) {
        return hr.by_name[name];
    }
    #{
        position: "",
        id_card: "",
        hire_date: "",
        resign_date: "",
    }
}

/// 岗位名含「（非全）」为非全日制，考勤在其他系统计算，本表剔除
fn is_part_time_position(position) {
    to_str(position).contains("（非全）")
}

fn filter_out_part_time(employees, hr_info) {
    let result = [];
    let skipped = 0;
    let i = 0;
    while i < employees.len() {
        let e = employees[i];
        let info = lookup_hr(hr_info, e.code, e.name);
        if is_part_time_position(info.position) {
            skipped += 1;
        } else {
            result.push(e);
        }
        i += 1;
    }
    #{ employees: result, skipped: skipped }
}

fn load_exceptions(path) {
    if path == "" { return []; }
    let table = read_table(path, ());
    let records = [];
    let total = table_rows(table);
    let i = 2;
    while i < total {
        let code = to_str(table_get(table, i, 0));
        if code != "" {
            records.push(#{
                code: code,
                name: normalize_name(table_get(table, i, 1)),
                date: to_str(table_get(table, i, 3)),
                result: to_str(table_get(table, i, 5)),
                earliest: to_str(table_get(table, i, 6)),
            });
        }
        i += 1;
    }
    records
}

fn empty_ot() {
    #{ special: 0.0, temp: 0.0, night: 0.0, restday: 0.0, holiday: 0.0 }
}

fn find_ot_cols(table) {
    let cols = #{ special: [], temp: [], night: [], restday: [], holiday: [] };
    let c = 0;
    while c < 80 {
        let h = to_str(table_get(table, 0, c));
        if h.contains("专项作业加班") { cols.special.push(c); }
        else if h.contains("临时加班") { cols.temp.push(c); }
        else if h.contains("夜保加班") { cols.night.push(c); }
        else if h.contains("休息日加班") { cols.restday.push(c); }
        else if h.contains("法定假期加班") { cols.holiday.push(c); }
        c += 1;
    }
    cols
}

fn sum_ot_cols(table, row, cols) {
    let total = 0.0;
    let i = 0;
    while i < cols.len() {
        total += to_num(table_get(table, row, cols[i]));
        i += 1;
    }
    total
}

/// 一线 MES 月报：一次扫描同时得到人员清单与五类加班（日历格由请假/排班/日报覆盖）
fn load_frontline_people(path) {
    if path == "" {
        return #{ employees: [], ot: #{ by_code: #{}, by_name: #{} } };
    }
    let table = read_table(path, ());
    let h_code = col_index(table, 0, "姓名_编号");
    let h_name = col_index(table, 0, "姓名_姓名");
    let h_org = col_index(table, 0, "责任部门");
    let h_should = col_index(table, 0, "应出勤工时数");
    let h_actual = col_index(table, 0, "实际出勤工时数");
    let h_incomplete = col_index(table, 0, "因入、离职未出勤工时");
    let h_absent = col_index(table, 0, "旷工小时数");
    let h_annual = col_index(table, 0, "年假");
    let h_personal = col_index(table, 0, "事假");
    let h_sick = col_index(table, 0, "病假");
    let h_lieu = col_index(table, 0, "调休");
    let h_maternity = col_index(table, 0, "产假");
    let h_paternity = col_index(table, 0, "陪产假");
    let h_nursing = col_index(table, 0, "哺乳假");
    let h_marriage = col_index(table, 0, "婚假");
    let h_bereave = col_index(table, 0, "丧假");
    let h_injury = col_index(table, 0, "工伤假");
    let h_visit = col_index(table, 0, "探亲假");
    let h_prenatal = col_index(table, 0, "产检假");
    let h_other = col_index(table, 0, "其他假");

    let ot_cols = find_ot_cols(table);
    let employees = [];
    let by_code = #{};
    let by_name = #{};
    let total = table_rows(table);
    let i = 1;
    while i < total {
        let code = to_str(table_get(table, i, h_code));
        if code != "" {
            let name = normalize_name(table_get(table, i, h_name));
            // 调休并入年假类；探亲/其他并入“其他假”汇总列
            let annual = to_num(table_get(table, i, h_annual)) + to_num(table_get(table, i, h_lieu));
            let prenatal = to_num(table_get(table, i, h_prenatal))
                + to_num(table_get(table, i, h_visit))
                + to_num(table_get(table, i, h_other));
            employees.push(#{
                code: code,
                name: name,
                org: to_str(table_get(table, i, h_org)),
                daily: default_frontline_daily(),
                should_hours: to_num(table_get(table, i, h_should)),
                absent_hours: to_num(table_get(table, i, h_absent)),
                personal_hours: to_num(table_get(table, i, h_personal)),
                annual_hours: annual,
                sick_hours: to_num(table_get(table, i, h_sick)),
                maternity_hours: to_num(table_get(table, i, h_maternity)),
                paternity_hours: to_num(table_get(table, i, h_paternity)),
                bereave_hours: to_num(table_get(table, i, h_bereave)),
                injury_hours: to_num(table_get(table, i, h_injury)),
                nursing_hours: to_num(table_get(table, i, h_nursing)),
                marriage_hours: to_num(table_get(table, i, h_marriage)),
                prenatal_hours: prenatal,
                actual_hours: to_num(table_get(table, i, h_actual)),
                incomplete_hours: to_num(table_get(table, i, h_incomplete)),
                late_count: 0.0,
                early_count: 0.0,
                missed_punch: 0.0,
                overtime_hours: 0.0,
            });
            let rec = #{
                special: sum_ot_cols(table, i, ot_cols.special),
                temp: sum_ot_cols(table, i, ot_cols.temp),
                night: sum_ot_cols(table, i, ot_cols.night),
                restday: sum_ot_cols(table, i, ot_cols.restday),
                holiday: sum_ot_cols(table, i, ot_cols.holiday),
            };
            by_code[code] = rec;
            if name != "" {
                by_name[name] = rec;
            }
        }
        i += 1;
    }
    #{ employees: employees, ot: #{ by_code: by_code, by_name: by_name } }
}

/// 一线优先（工时/加班以一线为准）；若运营月报有同人日历，则保留运营逐日状态（出勤/休息更完整）
fn merge_employees(front_emps, monthly_emps) {
    let monthly_by_code = #{};
    let monthly_by_name = #{};
    let mi = 0;
    while mi < monthly_emps.len() {
        let m = monthly_emps[mi];
        if m.code != "" { monthly_by_code[m.code] = m; }
        if m.name != "" { monthly_by_name[m.name] = m; }
        mi += 1;
    }

    let result = [];
    let seen_code = #{};
    let seen_name = #{};
    let i = 0;
    while i < front_emps.len() {
        let e = front_emps[i];
        let daily = e.daily;
        // 有工号只按工号对齐月报日历，避免同名串日历
        if e.code != "" && monthly_by_code.contains(e.code) {
            daily = monthly_by_code[e.code].daily;
        } else if e.code == "" && e.name != "" && monthly_by_name.contains(e.name) {
            daily = monthly_by_name[e.name].daily;
        }
        result.push(#{
            code: e.code,
            name: e.name,
            org: e.org,
            daily: daily,
            should_hours: e.should_hours,
            absent_hours: e.absent_hours,
            personal_hours: e.personal_hours,
            annual_hours: e.annual_hours,
            sick_hours: e.sick_hours,
            maternity_hours: e.maternity_hours,
            paternity_hours: e.paternity_hours,
            bereave_hours: e.bereave_hours,
            injury_hours: e.injury_hours,
            nursing_hours: e.nursing_hours,
            marriage_hours: e.marriage_hours,
            prenatal_hours: e.prenatal_hours,
            actual_hours: e.actual_hours,
            incomplete_hours: e.incomplete_hours,
            late_count: e.late_count,
            early_count: e.early_count,
            missed_punch: e.missed_punch,
            overtime_hours: e.overtime_hours,
        });
        if e.code != "" {
            seen_code[e.code] = 1;
        } else if e.name != "" {
            seen_name[e.name] = 1;
        }
        i += 1;
    }
    let added = 0;
    let j = 0;
    while j < monthly_emps.len() {
        let e = monthly_emps[j];
        let dup_code = e.code != "" && seen_code.contains(e.code);
        // 无工号才用姓名去重；有工号的同名第二人必须保留
        let dup_name = e.code == "" && e.name != "" && seen_name.contains(e.name);
        if !dup_code && !dup_name {
            result.push(e);
            if e.code != "" {
                seen_code[e.code] = 1;
            } else if e.name != "" {
                seen_name[e.name] = 1;
            }
            added += 1;
        }
        j += 1;
    }
    #{ employees: result, front_count: front_emps.len(), monthly_only: added, monthly_total: monthly_emps.len() }
}

fn ot_bucket(ot_type, ot_project) {
    let s = ot_type + ot_project;
    if s.contains("夜保") { return "night"; }
    if s.contains("专项") { return "special"; }
    if s.contains("法定") || s.contains("节假日") || s.contains("节假") { return "holiday"; }
    if s.contains("休息日") { return "restday"; }
    "temp"
}

fn add_ot_hours(rec, bucket, hours) {
    if bucket == "special" {
        return #{
            special: rec.special + hours,
            temp: rec.temp,
            night: rec.night,
            restday: rec.restday,
            holiday: rec.holiday,
        };
    }
    if bucket == "temp" {
        return #{
            special: rec.special,
            temp: rec.temp + hours,
            night: rec.night,
            restday: rec.restday,
            holiday: rec.holiday,
        };
    }
    if bucket == "night" {
        return #{
            special: rec.special,
            temp: rec.temp,
            night: rec.night + hours,
            restday: rec.restday,
            holiday: rec.holiday,
        };
    }
    if bucket == "restday" {
        return #{
            special: rec.special,
            temp: rec.temp,
            night: rec.night,
            restday: rec.restday + hours,
            holiday: rec.holiday,
        };
    }
    if bucket == "holiday" {
        return #{
            special: rec.special,
            temp: rec.temp,
            night: rec.night,
            restday: rec.restday,
            holiday: rec.holiday + hours,
        };
    }
    rec
}

/// 二线 EHR 加班台账：按加班类型/项目归入五类并按人汇总（不按组织过滤）
fn load_secondline_ot(path) {
    if path == "" { return #{ by_code: #{}, by_name: #{} }; }
    let table = read_table(path, ());
    let h_code = col_index(table, 0, "职员编码");
    let h_name = col_index(table, 0, "职员");
    let h_type = col_index(table, 0, "加班类型");
    let h_project = col_index(table, 0, "加班项目");
    let h_hours = col_index(table, 0, "实际加班时长");
    let h_status = col_index(table, 0, "审批状态");
    let h_doc = col_index(table, 0, "单据状态");

    let by_code = #{};
    let by_name = #{};
    let total = table_rows(table);
    let i = 2;
    while i < total {
        let code = to_str(table_get(table, i, h_code));
        if code != "" {
            let status = to_str(table_get(table, i, h_status));
            let doc = to_str(table_get(table, i, h_doc));
            let ok_status = status == "" || status == "审批通过";
            let ok_doc = doc == "" || doc == "生效";
            if ok_status && ok_doc {
                let hours = to_num(table_get(table, i, h_hours));
                if hours != 0.0 {
                    let bucket = ot_bucket(
                        to_str(table_get(table, i, h_type)),
                        to_str(table_get(table, i, h_project)),
                    );
                    let rec = if by_code.contains(code) { by_code[code] } else { empty_ot() };
                    by_code[code] = add_ot_hours(rec, bucket, hours);
                    let name = normalize_name(table_get(table, i, h_name));
                    if name != "" {
                        let nrec = if by_name.contains(name) { by_name[name] } else { empty_ot() };
                        by_name[name] = add_ot_hours(nrec, bucket, hours);
                    }
                }
            }
        }
        i += 1;
    }
    #{ by_code: by_code, by_name: by_name }
}

/// 一线优先：员工出现在一线月报中则只用一线加班，否则用二线；
/// 加班排班记录有月工时则覆盖对应休息日/法定加班列
fn lookup_ot(ot, code, name) {
    let rec = empty_ot();
    // 有工号时各源只按工号取，避免重名串加班小时
    if code != "" {
        if ot.front.by_code.contains(code) {
            rec = ot.front.by_code[code];
        } else if ot.second.by_code.contains(code) {
            rec = ot.second.by_code[code];
        }
    } else if name != "" {
        if ot.front.by_name.contains(name) {
            rec = ot.front.by_name[name];
        } else if ot.second.by_name.contains(name) {
            rec = ot.second.by_name[name];
        }
    }
    let sched = lookup_ot_schedule(ot.schedule, code, name);
    if sched != () && sched.hours > 0.0 {
        if sched.holiday {
            rec = #{
                special: rec.special,
                temp: rec.temp,
                night: rec.night,
                restday: rec.restday,
                holiday: sched.hours,
            };
        } else {
            rec = #{
                special: rec.special,
                temp: rec.temp,
                night: rec.night,
                restday: sched.hours,
                holiday: rec.holiday,
            };
        }
    }
    rec
}

fn set_hours_cell(wb, sheet, row, col, hours) {
    if hours != 0.0 {
        ws_set_cell(wb, sheet, row, col, hours);
    } else {
        ws_set_cell(wb, sheet, row, col, "");
    }
}

fn fill_row(wb, cfg, symbols, row, emp, info, ot, month, user_leave, user_ot_days) {
    let sheet = cfg.sheet_name;
    let hpd = cfg.hours_per_day;
    let other_hours = other_leave_hours(emp);
    let total_hours = emp.actual_hours + ot.special + ot.temp + ot.night + ot.restday + ot.holiday;

    ws_set_cell(wb, sheet, row, 2, emp.org);
    ws_set_cell(wb, sheet, row, 3, info.position);
    ws_set_cell(wb, sheet, row, 4, info.id_card);
    ws_set_cell(wb, sheet, row, 5, emp.name);

    let dim = 31;
    if month != "" && month.len() >= 7 {
        dim = days_in_month(to_num(month.sub_string(0, 4)), to_num(month.sub_string(5, 2)));
    }
    let d = 0;
    while d < 31 {
        let day_num = d + 1;
        // 当月不存在的日期（如 6 月 31 日）必须留空，不能回落到 emp.daily
        if month != "" && month.len() >= 7 && day_num > dim {
            ws_set_cell(wb, sheet, row, 7 + d, "");
            d += 1;
            continue;
        }
        if month != "" && month.len() >= 7 {
            let day_date = format_day_date(month, day_num + 0.0);
            if !is_employed_on(day_date, info.hire_date, info.resign_date) {
                ws_set_cell(wb, sheet, row, 7 + d, "");
                d += 1;
                continue;
            }
            let sym = "";
            if user_leave != () && user_leave.contains(day_date) {
                sym = leave_type_to_symbol(user_leave[day_date]);
            }
            if sym == "" && user_ot_days != () && user_ot_days.contains(day_date) {
                sym = "√";
            }
            if sym == "" {
                sym = status_symbol(emp.daily[d], symbols);
            }
            if sym != "" {
                ws_set_cell(wb, sheet, row, 7 + d, sym);
            }
        } else {
            let sym = status_symbol(emp.daily[d], symbols);
            if sym != "" {
                ws_set_cell(wb, sheet, row, 7 + d, sym);
            }
        }
        d += 1;
    }

    ws_set_cell(wb, sheet, row, 38, hours_to_days(emp.should_hours, hpd));
    ws_set_cell(wb, sheet, row, 39, emp.should_hours);
    ws_set_cell(wb, sheet, row, 40, hours_to_days(emp.actual_hours, hpd));
    ws_set_cell(wb, sheet, row, 41, emp.actual_hours);
    set_hours_cell(wb, sheet, row, 42, ot.special);
    set_hours_cell(wb, sheet, row, 43, ot.temp);
    set_hours_cell(wb, sheet, row, 44, ot.night);
    set_hours_cell(wb, sheet, row, 45, ot.restday);
    set_hours_cell(wb, sheet, row, 46, ot.holiday);
    ws_set_cell(wb, sheet, row, 47, total_hours);
    ws_set_cell(wb, sheet, row, 48, emp.personal_hours);
    ws_set_cell(wb, sheet, row, 49, emp.sick_hours);
    ws_set_cell(wb, sheet, row, 50, emp.absent_hours);
    ws_set_cell(wb, sheet, row, 51, emp.injury_hours);
    ws_set_cell(wb, sheet, row, 52, 0);
    ws_set_cell(wb, sheet, row, 53, other_hours);
    ws_set_cell(wb, sheet, row, 54, build_other_leave_remark(user_leave, (), emp, month, info));
    ws_set_cell(wb, sheet, row, 55, info.hire_date);
    ws_set_cell(wb, sheet, row, 56, info.resign_date);
    ws_set_cell(wb, sheet, row, 57, emp.incomplete_hours);
}

fn is_month_token(s) {
    if s.len() < 7 { return false; }
    let y = s.sub_string(0, 4);
    let sep = s.sub_string(4, 1);
    let m = s.sub_string(5, 2);
    if sep != "-" && sep != "/" { return false; }
    if y.len() != 4 || m.len() != 2 { return false; }
    to_num(y) >= 2000.0 && to_num(m) >= 1.0 && to_num(m) <= 12.0
}

fn normalize_month(s) {
    if s.len() >= 7 {
        return s.sub_string(0, 4) + "-" + s.sub_string(5, 2);
    }
    s
}

fn split_args(raw) {
    let parts = [];
    let cur = "";
    let i = 0;
    let s = raw + " ";
    while i < s.len() {
        let ch = s.sub_string(i, 1);
        if ch == " " || ch == "\t" {
            if cur != "" {
                parts.push(cur);
                cur = "";
            }
        } else {
            cur += ch;
        }
        i += 1;
    }
    parts
}

/// 解析「脚本合并考勤数据 公司 月份」剩余 ARGS；月份形如 2026-06
fn parse_fill_args(raw, fallback_company, fallback_month) {
    let company = fallback_company;
    let month = fallback_month;
    let parts = split_args(raw);
    if parts.len() == 0 {
        return #{ company: company, month: month };
    }
    if parts.len() == 1 {
        if is_month_token(parts[0]) {
            month = normalize_month(parts[0]);
        } else {
            company = parts[0];
        }
        return #{ company: company, month: month };
    }
    // 末段为月份则其余拼成公司名；否则首段公司、次段月份
    if is_month_token(parts[parts.len() - 1]) {
        month = normalize_month(parts[parts.len() - 1]);
        let name = "";
        let i = 0;
        while i < parts.len() - 1 {
            if i > 0 { name += " "; }
            name += parts[i];
            i += 1;
        }
        if name != "" { company = name; }
    } else {
        company = parts[0];
        if is_month_token(parts[1]) {
            month = normalize_month(parts[1]);
        }
    }
    #{ company: company, month: month }
}

fn month_parts(month) {
    #{
        year: to_num(month.sub_string(0, 4)),
        month: to_num(month.sub_string(5, 2)),
    }
}

/// 写入模板顶部时间（A1）与单位名（U1），并同步第 2 行日序列
fn fill_header(wb, cfg, company, month) {
    let sheet = cfg.sheet_name;
    if month != "" && month.len() >= 7 {
        let p = month_parts(month);
        let y = p.year;
        let m = p.month;
        ws_set_date(wb, sheet, 1, 1, y, m, 1.0);
        let dim = days_in_month(y, m);
        let d = 1;
        while d <= 31 {
            let col = 6 + d;
            if d <= dim {
                ws_set_date(wb, sheet, 2, col, y, m, d + 0.0);
            } else {
                ws_set_cell(wb, sheet, 2, col, "");
            }
            d += 1;
        }
        print("表头月份: " + month + "（" + dim + " 天）");
    }
    if company != "" {
        ws_set_cell(wb, sheet, 1, 21, "考勤记录表 单位：" + company);
        print("表头单位: " + company);
    }
}

// ─── 配置与主流程 ──────────────────────────────────────────────────────────

let cfg = #{
    hours_per_day: 8.0,
    sheet_name: "Sheet1",
    template_row: 3,
    footer_row: 4,
    max_col: 57,
};

// 新模板图例: √出勤 R休假 S事假 B病假 G工伤 X旷工 C迟到 T早退 Q其他
// 产假/陪产假/婚假/丧假/哺乳假/产检假/探亲假等 → Q（由 leave_type_to_symbol 统一处理）
let symbols = #{
    "休息": "R",
    "出勤": "√",
    "出差": "√",
    "迟到": "C",
    "旷工": "X",
    "年假": "R",
    "调休": "R",
    "休假": "R",
    "外出": "√",
    "事假": "S",
    "病假": "B",
    "早退": "T",
    "工伤": "G",
    "工伤假": "G",
    "产假": "Q",
    "陪产假": "Q",
    "婚假": "Q",
    "丧假": "Q",
    "哺乳假": "Q",
    "产检假": "Q",
    "探亲假": "Q",
    "其他假": "Q",
    "其他": "Q",
};

print("考勤记录表填充 (新模板)");
print("────────────────────────────────────────");

let monthly = find_file_optional("downloads", "^考勤月报_.*\\.xlsx$");
let leave = find_file_optional("downloads", "^休假台账_.*\\.xlsx$");
let exception = find_file_optional("downloads", "^异常数据_.*\\.xlsx$");
let hr_export = find_file_optional("downloads", "^页面结果导出_.*\\.xlsx$");
let frontline = find_file_optional("downloads", "^\\d{4}-\\d{2}月报\\.xls$");
let ot_ledger = find_file_optional("downloads", "^加班台账_.*\\.xlsx$");
let leave_register = find_source_xls("^请假登记.*\\.xls[x]?$");
let ot_schedule_file = find_source_xls("^加班排班记录.*\\.xls[x]?$");
let daily_file = find_source_xls("^日报.*\\.xls[x]?$");

if monthly == "" {
    print("⚠ 未找到考勤月报，本次不生成考勤记录表");
    return;
}

let department = department_from_monthly(monthly);
if department == "" {
    print("⚠ 无法从考勤月报文件名解析部门，本次不生成考勤记录表");
    print("  期望格式: 考勤月报_部门名_日期...");
    return;
}

let fill = parse_fill_args(ARGS, department, month_from_monthly(monthly));
let header_company = fill.company;
let header_month = fill.month;

let template = find_template_optional("^考勤记录表-.*\\.xlsx$", "已填充");
let output = "files/考勤记录表-" + department + "-已填充.xlsx";

print("考勤月报: " + basename(monthly));
if leave == "" { print("⚠ 未找到休假台账，跳过休假数据"); }
else { print("休假台账: " + basename(leave)); }
if exception == "" { print("⚠ 未找到异常数据，跳过异常数据"); }
else { print("异常数据: " + basename(exception)); }
if hr_export == "" { print("⚠ 未找到人员信息，岗位/身份证/入离职日期将留空"); }
else { print("人员信息: " + basename(hr_export)); }
if frontline == "" { print("⚠ 未找到一线月报，将仅填充运营考勤月报人员"); }
else { print("一线月报: " + basename(frontline)); }
if ot_ledger == "" { print("⚠ 未找到加班台账，跳过二线加班数据"); }
else { print("加班台账: " + basename(ot_ledger)); }
if leave_register == "" { print("⚠ 未找到请假登记，日历请假符号可能无法按事由对齐"); }
else { print("请假登记: " + basename(leave_register)); }
if ot_schedule_file == "" { print("⚠ 未找到加班排班记录，跳过排班出勤/休息日加班覆盖"); }
else { print("加班排班: " + basename(ot_schedule_file)); }
if daily_file == "" { print("⚠ 未找到日报，跳过日报日历覆盖"); }
else { print("日报: " + basename(daily_file)); }
print("部门: " + department);
print("参数 ARGS: [" + ARGS + "]");
if template == "" {
    print("⚠ 未找到考勤记录表模板，本次不生成考勤记录表");
    return;
}
print("模板: " + basename(template));

let monthly_emps = load_monthly(monthly);
let front_pack = load_frontline_people(frontline);
let merged = merge_employees(front_pack.employees, monthly_emps);
let hr_info = load_hr_info(hr_export);
let filtered = filter_out_part_time(merged.employees, hr_info);
let employees = filtered.employees;
let leave_data = load_leave_register(leave_register, header_month);
let ot_schedule = load_ot_schedule(ot_schedule_file, header_month);
let daily_data = load_daily_status(daily_file, header_month);
let ot_info = #{
    front: front_pack.ot,
    second: load_secondline_ot(ot_ledger),
    schedule: ot_schedule,
};

print("");
print("运营月报: " + merged.monthly_total + " 人");
print("一线月报: " + merged.front_count + " 人");
print("合并后: " + merged.employees.len() + " 人（一线优先，另补运营独有 " + merged.monthly_only + " 人）");
if filtered.skipped > 0 {
    print("剔除非全日制（岗位含「（非全）」）: " + filtered.skipped + " 人，入表 " + employees.len() + " 人");
} else {
    print("入表: " + employees.len() + " 人");
}
if leave != "" { print("休假台账: 月报已含休假汇总，无需重复解析"); }
if exception != "" { print("异常数据: 月报已含异常汇总，无需重复解析"); }

copy_template(template, output);
let wb = open_workbook(output);
fill_header(wb, cfg, header_company, header_month);

let existing = cfg.footer_row - cfg.template_row;
let needed = employees.len();
let extra = needed - existing;

if extra > 0 {
    // 一次插入全部行，避免逐行移动工作表造成 O(n²)；样式/公式在下方填充循环里补一次即可
    ws_insert_rows(wb, cfg.sheet_name, cfg.footer_row, extra);
}

let idx = 0;
while idx < needed {
    let row = cfg.template_row + idx;
    if idx >= existing {
        ws_copy_row_style(wb, cfg.sheet_name, cfg.template_row, row, cfg.max_col);
        ws_set_formula(wb, cfg.sheet_name, row, 1, "ROW()-2");
    }
    // 先取出单人 HR/加班，避免 fill_row 每次深拷贝整张索引表
    let emp = employees[idx];
    let info = lookup_hr(hr_info, emp.code, emp.name);
    let ot = lookup_ot(ot_info, emp.code, emp.name);
    let user_leave = lookup_leave_days(leave_data, emp.code, emp.name);
    let sched = lookup_ot_schedule(ot_schedule, emp.code, emp.name);
    let user_ot_days = if sched != () { sched.days } else { () };
    fill_row(wb, cfg, symbols, row, emp, info, ot, header_month, user_leave, user_ot_days);
    if (idx + 1) % 100 == 0 {
        print("  已填充 " + (idx + 1) + "/" + needed);
    }
    idx += 1;
}

// ─── 日报覆盖日历（与请假登记 / 加班排班同一优先级规则）────────────────
let day_dates = [];
if header_month != "" && header_month.len() >= 7 {
    let dim = days_in_month(to_num(header_month.sub_string(0, 4)), to_num(header_month.sub_string(5, 2)));
    let d0 = 1;
    while d0 <= dim {
        day_dates.push(format_day_date(header_month, d0 + 0.0));
        d0 += 1;
    }
}

let patched_cells = 0;
let patched_people = 0;
if daily_file != "" || leave_register != "" || ot_schedule_file != "" {
    print("");
    print("按请假登记 / 加班排班 / 日报刷新日历格…");
    let pidx = 0;
    while pidx < needed {
        let row = cfg.template_row + pidx;
        let emp = employees[pidx];
        let info = lookup_hr(hr_info, emp.code, emp.name);
        let user_leave = lookup_leave_days(leave_data, emp.code, emp.name);
        let sched = lookup_ot_schedule(ot_schedule, emp.code, emp.name);
        let user_ot_days = if sched != () { sched.days } else { () };
        let daily_key = name_dept_key(emp.name, emp.org);
        let user_daily = if daily_data.contains(daily_key) { daily_data[daily_key] } else { () };
        let person_hit = 0;
        let d = 0;
        while d < day_dates.len() {
            let target_date = day_dates[d];
            if !is_employed_on(target_date, info.hire_date, info.resign_date) {
                d += 1;
                continue;
            }
            let sym = resolve_day_symbol(user_daily, user_leave, user_ot_days, target_date);
            if sym != "" {
                ws_set_cell(wb, cfg.sheet_name, row, 7 + d, sym);
                patched_cells += 1;
                person_hit += 1;
            }
            d += 1;
        }
        if person_hit > 0 {
            patched_people += 1;
        }
        // 清空当月不存在的日期列（模板可能带 31 日）
        let extra = day_dates.len();
        while extra < 31 {
            ws_set_cell(wb, cfg.sheet_name, row, 7 + extra, "");
            extra += 1;
        }
        // 备注：纳入日报后的 Q 类假类型
        ws_set_cell(
            wb,
            cfg.sheet_name,
            row,
            54,
            build_other_leave_remark(user_leave, user_daily, emp, header_month, info),
        );
        pidx += 1;
    }
    print("  日历刷新: " + patched_people + " 人 / " + patched_cells + " 格");
}

// 模板固定 31 天：删除当月不存在的日期列（其后工时等列会左移对齐）
if header_month != "" && header_month.len() >= 7 {
    let dim = days_in_month(to_num(header_month.sub_string(0, 4)), to_num(header_month.sub_string(5, 2)));
    let extra_days = 31 - dim;
    if extra_days > 0 {
        // 第 dim+1 日对应列 = 7 + dim
        ws_remove_columns(wb, cfg.sheet_name, 7 + dim, extra_days);
        print("已删除当月不存在的日期列 " + extra_days + " 列（保留 " + dim + " 天）");
    }
}

wb_save(wb, output);
track_output(output);
print("");
print("✓ 已生成: " + output);
print("  填充 " + needed + " 名员工数据");
if patched_cells > 0 {
    print("  已合并日报/请假/排班日历覆盖");
}
