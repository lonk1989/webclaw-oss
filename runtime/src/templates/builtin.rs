use super::store::TemplateDef;
use chrono::Utc;

pub const BUILTIN_ATTENDANCE_ID: &str = "builtin_attendance_summary";
pub const BUILTIN_ATTENDANCE_FILENAME: &str = "考勤记录表-XXX.xlsx";

pub const BUILTIN_STUDENT_SCORES_ID: &str = "builtin_student_scores";
pub const BUILTIN_STUDENT_SCORES_FILENAME: &str = "学生各科分数.xlsx";

pub fn default_templates() -> Vec<TemplateDef> {
    let now = Utc::now();
    vec![
        TemplateDef {
            id: BUILTIN_STUDENT_SCORES_ID.into(),
            name: "学生各科分数".into(),
            filename: BUILTIN_STUDENT_SCORES_FILENAME.into(),
            description: "示例脚本共用样例：青禾/白鹭等，东湖班/西岭班；列含姓名/班级/语文/数学/英语/科学，另有「核算结果」空表头".into(),
            builtin: true,
            updated_at: now,
        },
        TemplateDef {
            id: BUILTIN_ATTENDANCE_ID.into(),
            name: "考勤汇总模板".into(),
            filename: BUILTIN_ATTENDANCE_FILENAME.into(),
            description: "用于「脚本合并考勤数据」，填充月度考勤汇总表".into(),
            builtin: true,
            updated_at: now,
        },
    ]
}

pub fn embedded_bytes(filename: &str) -> Option<&'static [u8]> {
    if filename == BUILTIN_ATTENDANCE_FILENAME {
        Some(include_bytes!("../../data/templates/考勤记录表-XXX.xlsx"))
    } else if filename == BUILTIN_STUDENT_SCORES_FILENAME {
        Some(include_bytes!("../../data/templates/学生各科分数.xlsx"))
    } else {
        None
    }
}
