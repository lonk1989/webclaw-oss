/// 用法：脚本示例核算总分 [班级]
/// 按各科求和/平均，写入 files/学生成绩-已核算.xlsx（核算结果表）

let class_filter = arg(0);

let src = find_file_optional("files", "^学生各科分数\\.xlsx$");
if src == "" {
    let tpl = find_template_optional("^学生各科分数\\.xlsx$", "");
    if tpl == "" { throw "未找到学生各科分数表"; }
    copy_template(tpl, "files/学生各科分数.xlsx");
    src = "files/学生各科分数.xlsx";
}

let t = read_table(src, ());
let name_col = col_index(t, 0, "姓名");
let class_col = col_index(t, 0, "班级");
let subjects = ["语文", "数学", "英语", "科学"];
let sub_cols = [];
let si = 0;
while si < subjects.len() {
    let c = col_index(t, 0, subjects[si]);
    if c < 0 { throw "缺少科目列：" + subjects[si]; }
    sub_cols.push(c);
    si += 1;
}
if name_col < 0 || class_col < 0 { throw "缺少姓名或班级列"; }

let out = "files/学生成绩-已核算.xlsx";
copy_file(src, out);
let wb = open_workbook(out);
let sheet = "核算结果";
wb_set_title(wb, sheet, "学生成绩核算");
ws_set_cell(wb, sheet, 2, 1, "姓名");
ws_set_cell(wb, sheet, 2, 2, "班级");
ws_set_cell(wb, sheet, 2, 3, "总分");
ws_set_cell(wb, sheet, 2, 4, "平均");

let out_row = 3;
let r = 1;
while r < table_rows(t) {
    let cls = to_str(table_get(t, r, class_col));
    if class_filter == "" || cls == class_filter {
        let sum = 0.0;
        let j = 0;
        while j < sub_cols.len() {
            sum += to_num(table_get(t, r, sub_cols[j]));
            j += 1;
        }
        let avg = round2(sum / to_num(sub_cols.len()));
        ws_set_cell(wb, sheet, out_row, 1, to_str(table_get(t, r, name_col)));
        ws_set_cell(wb, sheet, out_row, 2, cls);
        ws_set_cell(wb, sheet, out_row, 3, sum);
        ws_set_cell(wb, sheet, out_row, 4, avg);
        out_row += 1;
    }
    r += 1;
}

wb_save(wb, out);
track_output(out);
print("已核算 " + (out_row - 3) + " 人 → " + out);
