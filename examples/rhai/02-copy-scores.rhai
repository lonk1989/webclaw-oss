/// 用法：脚本示例复制成绩表
/// 从内置模板「学生各科分数」复制到 files/，供后续核算示例使用

let tpl = find_template_optional("^学生各科分数\\.xlsx$", "");
if tpl == "" { throw "未找到内置模板「学生各科分数」，请刷新模板列表"; }

let dest = "files/学生各科分数.xlsx";
copy_template(tpl, dest);
track_output(dest);
print("已复制模板 → " + dest);
