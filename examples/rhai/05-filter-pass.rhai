/// 用法：脚本示例筛及格 [分数线] [班级]
/// 平均分 >= 分数线（默认 60）视为及格；输出 files/及格名单.txt

let line = to_num(arg(0, "60"));
let class_filter = arg(1);
if line <= 0.0 { line = 60.0; }

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

let lines = "分数线=" + line + "\n";
let pass_n = 0;
let total_n = 0;
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
        total_n += 1;
        let name = to_str(table_get(t, r, name_col));
        if avg >= line {
            pass_n += 1;
            lines += name + "\t" + cls + "\t平均=" + avg + "\t及格\n";
        } else {
            lines += name + "\t" + cls + "\t平均=" + avg + "\t不及格\n";
        }
    }
    r += 1;
}
lines += "合计 " + total_n + " 人，及格 " + pass_n + " 人\n";

let out = "files/及格名单.txt";
write_text_file(out, lines);
track_output(out);
print("及格 " + pass_n + "/" + total_n + " → " + out);
