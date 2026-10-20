/// 用法：脚本示例预览成绩 [班级]
/// 读取「学生各科分数」（优先 files/，否则从模板复制），打印各科分数

let class_filter = arg(0);

let path = find_file_optional("files", "^学生各科分数\\.xlsx$");
if path == "" {
    let tpl = find_template_optional("^学生各科分数\\.xlsx$", "");
    if tpl == "" { throw "未找到学生各科分数表"; }
    copy_template(tpl, "files/学生各科分数.xlsx");
    path = "files/学生各科分数.xlsx";
}

let t = read_table(path, ());
let name_col = col_index(t, 0, "姓名");
let class_col = col_index(t, 0, "班级");
let c_cn = col_index(t, 0, "语文");
let c_math = col_index(t, 0, "数学");
let c_en = col_index(t, 0, "英语");
let c_sci = col_index(t, 0, "科学");
if name_col < 0 || class_col < 0 || c_cn < 0 || c_math < 0 || c_en < 0 || c_sci < 0 {
    throw "表头需含：姓名、班级、语文、数学、英语、科学";
}

print("姓名 班级 语/数/英/科");
let r = 1;
let n = 0;
while r < table_rows(t) {
    let cls = to_str(table_get(t, r, class_col));
    if class_filter == "" || cls == class_filter {
        print(
            to_str(table_get(t, r, name_col)) + " " + cls + " "
            + to_str(table_get(t, r, c_cn)) + "/"
            + to_str(table_get(t, r, c_math)) + "/"
            + to_str(table_get(t, r, c_en)) + "/"
            + to_str(table_get(t, r, c_sci))
        );
        n += 1;
    }
    r += 1;
}
print("共 " + n + " 人");
