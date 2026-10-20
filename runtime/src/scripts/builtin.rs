use super::store::ScriptDef;
use chrono::Utc;

/// 教学示例（学生成绩核算）— 与 `examples/rhai/` 同源
const EX_HELLO: &str = include_str!("../../../examples/rhai/01-hello-args.rhai");
const EX_COPY: &str = include_str!("../../../examples/rhai/02-copy-scores.rhai");
const EX_PREVIEW: &str = include_str!("../../../examples/rhai/03-preview-scores.rhai");
const EX_CALC: &str = include_str!("../../../examples/rhai/04-calc-total.rhai");
const EX_PASS: &str = include_str!("../../../examples/rhai/05-filter-pass.rhai");
const EX_HTTP_GET: &str = include_str!("../../../examples/rhai/06-http-get.rhai");

pub const SCRIPT_DOC_GUIDE: &str = include_str!("../../../examples/业务脚本编写指南.md");
pub const SCRIPT_DOC_AI_PROMPT: &str = include_str!("../../../examples/业务脚本-AI生成提示词.md");

pub fn default_scripts() -> Vec<ScriptDef> {
    let now = Utc::now();
    let mut out = example_scripts(now);
    out.extend([
        ScriptDef {
            id: "builtin_merge_attendance".into(),
            name: "合并考勤数据".into(),
            description: merge_attendance_description().into(),
            builtin: true,
            script: crate::rhai_runtime::FILL_ATTENDANCE_SCRIPT.to_string(),
            updated_at: now,
        },
        ScriptDef {
            id: "builtin_compare_attendance".into(),
            name: "考勤数据对比".into(),
            description: compare_attendance_description().into(),
            builtin: true,
            script: crate::rhai_runtime::COMPARE_ATTENDANCE_SCRIPT.to_string(),
            updated_at: now,
        },
        ScriptDef {
            id: "builtin_ocr_road_clean_classify".into(),
            name: "OCR验证道路清扫".into(),
            description: ocr_road_clean_classify_description().into(),
            builtin: true,
            script: crate::rhai_runtime::OCR_ROAD_CLEAN_CLASSIFY_SCRIPT.to_string(),
            updated_at: now,
        },
        ScriptDef {
            id: "builtin_scene_pair_score".into(),
            name: "场景配对打分".into(),
            description: scene_pair_score_description().into(),
            builtin: true,
            script: crate::rhai_runtime::SCENE_PAIR_SCORE_SCRIPT.to_string(),
            updated_at: now,
        },
    ]);
    out
}

fn example_scripts(now: chrono::DateTime<Utc>) -> Vec<ScriptDef> {
    vec![
        ScriptDef {
            id: "builtin_ex_hello_args".into(),
            name: "示例你好".into(),
            description: "演示参数。用法：脚本示例你好 东湖班 青禾".into(),
            builtin: true,
            script: EX_HELLO.to_string(),
            updated_at: now,
        },
        ScriptDef {
            id: "builtin_ex_find_and_copy".into(),
            name: "示例复制成绩表".into(),
            description: "复制内置模板「学生各科分数」到 files/。用法：脚本示例复制成绩表".into(),
            builtin: true,
            script: EX_COPY.to_string(),
            updated_at: now,
        },
        ScriptDef {
            id: "builtin_ex_read_table".into(),
            name: "示例预览成绩".into(),
            description: "预览各科分数。用法：脚本示例预览成绩 [班级]".into(),
            builtin: true,
            script: EX_PREVIEW.to_string(),
            updated_at: now,
        },
        ScriptDef {
            id: "builtin_ex_fill_template".into(),
            name: "示例核算总分".into(),
            description: "核算总分/平均并写回 Excel。用法：脚本示例核算总分 [班级]".into(),
            builtin: true,
            script: EX_CALC.to_string(),
            updated_at: now,
        },
        ScriptDef {
            id: "builtin_ex_unzip_scan".into(),
            name: "示例筛及格".into(),
            description: "按平均分筛及格。用法：脚本示例筛及格 [分数线] [班级]".into(),
            builtin: true,
            script: EX_PASS.to_string(),
            updated_at: now,
        },
        ScriptDef {
            id: "builtin_ex_http_get".into(),
            name: "示例HTTP GET".into(),
            description: "演示 http_get + json_parse。用法：脚本示例HTTP GET [可选URL]".into(),
            builtin: true,
            script: EX_HTTP_GET.to_string(),
            updated_at: now,
        },
    ]
}

/// 同步教学示例内置脚本（名称/备注/正文与仓库示例保持一致）
pub fn refresh_example_builtins(scripts: &mut Vec<ScriptDef>) -> bool {
    let mut changed = false;
    let now = Utc::now();
    for def in example_scripts(now) {
        if let Some(s) = scripts
            .iter_mut()
            .find(|c| c.id == def.id && c.builtin)
        {
            if s.name != def.name {
                s.name = def.name.clone();
                changed = true;
            }
            if s.description != def.description {
                s.description = def.description.clone();
                changed = true;
            }
            if s.script != def.script {
                s.script = def.script.clone();
                changed = true;
            }
        }
    }
    changed
}

pub fn merge_attendance_description() -> &'static str {
    "一次完成考勤记录表填充：运营月报+一线月报+人员信息+加班/请假登记/加班排班/日报。参数：公司 月份（如：脚本合并考勤数据 山西阳泉 2026-06）"
}

pub fn compare_attendance_description() -> &'static str {
    "解压加班工时申请 ZIP，按公司+月份对比考勤记录表与计划外加班工时汇总表的加班时长，输出审核意见。参数可选 ZIP 文件名关键字（如：脚本考勤数据对比 陈倩）"
}

pub fn ocr_road_clean_classify_description() -> &'static str {
    "生成「每日检查记录表」。入参：整改日期 微信账号根 片区名 整改前群ID 整改后群ID […]。整改前/后分属两群；回复群只取整改后图（忽略对比原图）。历史日一次出表，当日轮询至 24:00。示例：脚本OCR验证道路清扫 2026-09-14 E:\\Documents\\xwechat_files\\账号 上站所 前群ID 后群ID"
}

pub fn scene_pair_score_description() -> &'static str {
    "调试一对整改前/后图的配对分数。先添加恰好 2 张图片附件（第1张整改前，第2张整改后），再运行：脚本场景配对打分。附件不是 2 张会直接报错。"
}
