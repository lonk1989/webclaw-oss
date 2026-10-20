use std::collections::HashMap;

const SOURCE_KEY: &str = "来源";

/// 飞书查询筛选运算符（与指令参数语法一致）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryOp {
    Is,
    IsNot,
    Contains,
    DoesNotContain,
    IsEmpty,
    IsNotEmpty,
}

impl QueryOp {
    pub fn label(self) -> &'static str {
        match self {
            QueryOp::Is => "=",
            QueryOp::IsNot => "!=",
            QueryOp::Contains => "has",
            QueryOp::DoesNotContain => "!has",
            QueryOp::IsEmpty => "empty",
            QueryOp::IsNotEmpty => "!empty",
        }
    }

    pub fn needs_value(self) -> bool {
        !matches!(self, QueryOp::IsEmpty | QueryOp::IsNotEmpty)
    }
}

/// 单条飞书查询筛选条件
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryCondition {
    pub field: String,
    pub op: QueryOp,
    pub value: Option<String>,
}

const QUERY_OPS: &[(&str, QueryOp)] = &[
    ("!has", QueryOp::DoesNotContain),
    ("!empty", QueryOp::IsNotEmpty),
    ("!=", QueryOp::IsNot),
    ("has", QueryOp::Contains),
    ("empty", QueryOp::IsEmpty),
    ("=", QueryOp::Is),
];

fn find_leftmost_query_op(s: &str) -> Option<(usize, &'static str, QueryOp)> {
    let mut best: Option<(usize, &'static str, QueryOp)> = None;
    for &(token, op) in QUERY_OPS {
        if let Some(pos) = s.find(token) {
            let field = s[..pos].trim_end();
            if field.is_empty() || field.contains(' ') {
                continue;
            }
            if best.map(|(p, _, _)| pos < p).unwrap_or(true) {
                best = Some((pos, token, op));
            }
        }
    }
    best
}

/// 在值字符串中找下一条件起始空白（字段名不含空格）
fn find_next_query_condition_start(s: &str) -> Option<usize> {
    for (i, ch) in s.char_indices() {
        if i == 0 || !ch.is_whitespace() {
            continue;
        }
        let tail = s[i..].trim_start();
        if tail.is_empty() {
            continue;
        }
        let Some((pos, _, _)) = find_leftmost_query_op(tail) else {
            continue;
        };
        let field = tail[..pos].trim_end();
        if !field.is_empty() && !field.contains(' ') {
            return Some(i);
        }
    }
    None
}

fn peel_query_conditions(s: &str) -> Result<Vec<QueryCondition>, String> {
    let mut rest = s.trim();
    let mut out = Vec::new();
    while !rest.is_empty() {
        let Some((pos, token, op)) = find_leftmost_query_op(rest) else {
            if out.is_empty() {
                return Err(format!(
                    "无法解析「{rest}」，支持：字段=值、字段!=值、字段has值、字段!has值、字段empty、字段!empty"
                ));
            }
            break;
        };
        let field = rest[..pos].trim();
        if field.is_empty() {
            return Err("字段名不能为空".into());
        }
        let after_op = pos + token.len();
        let (value, remaining) = if op.needs_value() {
            let value_part = &rest[after_op..];
            let end = find_next_query_condition_start(value_part).unwrap_or(value_part.len());
            let v = value_part[..end].trim();
            if v.is_empty() {
                return Err(format!("字段「{field}」{} 需要填写比较值", op.label()));
            }
            (Some(v.to_string()), value_part[end..].trim_start())
        } else {
            let tail = rest[after_op..].trim();
            if !tail.is_empty() {
                return Err(format!("字段「{field}」{} 不需要填写值", op.label()));
            }
            (None, "")
        };
        out.push(QueryCondition {
            field: field.to_string(),
            op,
            value,
        });
        rest = remaining;
    }
    Ok(out)
}

fn split_query_arg_parts(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == ',' || c == '，' || c == '\n' || c == '\r' || c == ';' || c == '；')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// 解析飞书查询参数；条件可选，支持扩展运算符
pub fn parse_bitable_query_args(
    raw: &str,
) -> Result<(Option<String>, Vec<QueryCondition>), String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok((None, Vec::new()));
    }

    let parts = split_query_arg_parts(raw);
    if parts.is_empty() {
        return Ok((None, Vec::new()));
    }

    let mut table_name = None;
    let mut start = 0;

    if let Some(first) = parts.first() {
        if let Some(name) = first.strip_prefix('@') {
            let name = name.trim();
            if name.is_empty() {
                return Err("表名不能为空".into());
            }
            table_name = Some(name.to_string());
            start = 1;
        } else if parts.len() == 1 && find_leftmost_query_op(first).is_none() {
            return Err(format!("无法识别参数「{first}」"));
        }
    }

    let mut conditions = Vec::new();
    for part in parts.iter().skip(start) {
        conditions.extend(peel_query_conditions(part)?);
    }
    Ok((table_name, conditions))
}

pub fn query_conditions_summary(conditions: &[QueryCondition]) -> String {
    conditions
        .iter()
        .map(|c| {
            if let Some(v) = &c.value {
                format!("{}{}{}", c.field, c.op.label(), v)
            } else {
                format!("{}{}", c.field, c.op.label())
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `字段=` 起始位置：空白（或串首）后的 `键=`，键内不含空白/`=`。
/// 值里可含空格与 `=`（如 URL 查询串），不会被误切。
fn kv_field_starts(s: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut at_boundary = true;
    let mut key_begin = 0usize;
    for (idx, ch) in s.char_indices() {
        if ch.is_whitespace() {
            at_boundary = true;
            continue;
        }
        if at_boundary {
            key_begin = idx;
            at_boundary = false;
        }
        if ch == '=' {
            let key = &s[key_begin..idx];
            if !key.is_empty() && !key.chars().any(|c| c.is_whitespace() || c == '=') {
                starts.push(key_begin);
            }
            at_boundary = false;
        }
    }
    starts
}

/// 将空格分隔的 `a=1 b=hello world c=2` 切成多段；单段或无法识别时原样返回。
fn split_space_kv_fields(chunk: &str) -> Vec<String> {
    let chunk = chunk.trim();
    if chunk.is_empty() {
        return Vec::new();
    }
    let starts = kv_field_starts(chunk);
    if starts.is_empty() {
        return vec![chunk.to_string()];
    }

    let mut parts = Vec::new();
    if starts[0] > 0 {
        let lead = chunk[..starts[0]].trim();
        if !lead.is_empty() {
            parts.push(lead.to_string());
        }
    }
    for (i, &start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(chunk.len());
        let part = chunk[start..end].trim();
        if !part.is_empty() {
            parts.push(part.to_string());
        }
    }
    parts
}

/// 配方解析会把 `,`/`，` 等转成换行；也支持空格分隔的多个 `字段=值`。
fn split_arg_parts(raw: &str) -> Vec<String> {
    let coarse: Vec<String> = raw
        .split(|c: char| c == ',' || c == '，' || c == '\n' || c == '\r' || c == ';' || c == '；')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    let mut out = Vec::new();
    for chunk in coarse {
        out.extend(split_space_kv_fields(&chunk));
    }
    out
}

fn parse_field_parts(
    parts: &[String],
    start: usize,
) -> Result<HashMap<String, String>, String> {
    let mut fields = HashMap::new();
    for part in parts.iter().skip(start) {
        let Some((key, value)) = part.split_once('=') else {
            return Err(format!("无法解析「{part}」，应为 字段=值"));
        };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() {
            return Err("字段名不能为空".into());
        }
        if value.is_empty() {
            return Err(format!("字段「{key}」的值不能为空"));
        }
        fields.insert(key.to_string(), value.to_string());
    }
    Ok(fields)
}

/// 解析 `飞书新增,项目名称=鹤山` 或 `飞书新增@别名,项目名称=鹤山`
/// `@别名` 可选，用于补充唯一键；目标表由浏览器当前页决定。
pub fn parse_bitable_step_args(
    raw: &str,
) -> Result<(Option<String>, HashMap<String, String>), String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("参数不能为空".into());
    }

    let parts = split_arg_parts(raw);
    if parts.is_empty() {
        return Err("参数不能为空".into());
    }

    let mut table_name = None;
    let mut start = 0;

    if let Some(first) = parts.first() {
        if let Some(name) = first.strip_prefix('@') {
            let name = name.trim();
            if name.is_empty() {
                return Err("表名不能为空，格式：@项目提报台账".into());
            }
            table_name = Some(name.to_string());
            start = 1;
        }
    }

    let fields = parse_field_parts(&parts, start)?;
    Ok((table_name, fields))
}

/// 飞书同步专用：提取 `来源=files/xxx.xlsx`，其余为可选默认字段
pub fn parse_sync_args(
    raw: &str,
) -> Result<(Option<String>, String, HashMap<String, String>), String> {
    let (table_name, fields) = parse_bitable_step_args_allow_empty(raw)?;
    let source = fields
        .get(SOURCE_KEY)
        .cloned()
        .ok_or_else(|| "飞书同步需要指定 来源=files/或downloads/下的 Excel 路径".to_string())?;
    let mut defaults = fields;
    defaults.remove(SOURCE_KEY);
    Ok((table_name, source, defaults))
}

/// 查询允许无字段（查全部）；其余操作至少一个字段
pub fn parse_bitable_step_args_allow_empty(
    raw: &str,
) -> Result<(Option<String>, HashMap<String, String>), String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok((None, HashMap::new()));
    }

    let parts = split_arg_parts(raw);
    if parts.is_empty() {
        return Ok((None, HashMap::new()));
    }

    let mut table_name = None;
    let mut start = 0;

    if let Some(first) = parts.first() {
        if let Some(name) = first.strip_prefix('@') {
            let name = name.trim();
            if name.is_empty() {
                return Err("表名不能为空".into());
            }
            table_name = Some(name.to_string());
            start = 1;
        } else if !first.contains('=') && parts.len() == 1 {
            return Err(format!("无法识别参数「{first}」"));
        }
    }

    let fields = parse_field_parts(&parts, start)?;
    Ok((table_name, fields))
}

pub fn field_summary(fields: &HashMap<String, String>) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_with_table_and_fields() {
        let (name, fields) =
            parse_bitable_step_args("@项目提报台账,项目名称=鹤山").expect("parse");
        assert_eq!(name.as_deref(), Some("项目提报台账"));
        assert_eq!(fields.get("项目名称"), Some(&"鹤山".to_string()));
    }

    #[test]
    fn parse_with_newline_after_recipe_normalize() {
        // 配方解析会把逗号换成换行，执行时 args 形如：
        let (name, fields) =
            parse_bitable_step_args("@项目提报台账\n项目名称=鹤山").expect("parse");
        assert_eq!(name.as_deref(), Some("项目提报台账"));
        assert_eq!(fields.get("项目名称"), Some(&"鹤山".to_string()));
    }

    #[test]
    fn parse_with_chinese_comma() {
        let (name, fields) =
            parse_bitable_step_args("@项目提报台账，项目名称=鹤山，状态=进行中").expect("parse");
        assert_eq!(name.as_deref(), Some("项目提报台账"));
        assert_eq!(fields.get("项目名称"), Some(&"鹤山".to_string()));
        assert_eq!(fields.get("状态"), Some(&"进行中".to_string()));
    }

    #[test]
    fn parse_space_separated_fields() {
        let (name, fields) = parse_bitable_step_args(
            "项目名称=科技事业部 归属月份=2026-06 项目类别=考勤 提报人=我",
        )
        .expect("parse");
        assert!(name.is_none());
        assert_eq!(fields.get("项目名称"), Some(&"科技事业部".to_string()));
        assert_eq!(fields.get("归属月份"), Some(&"2026-06".to_string()));
        assert_eq!(fields.get("项目类别"), Some(&"考勤".to_string()));
        assert_eq!(fields.get("提报人"), Some(&"我".to_string()));
    }

    #[test]
    fn parse_space_separated_value_with_spaces_and_url_equals() {
        let (_, fields) = parse_bitable_step_args(
            "项目名称=科技 事业部 资料存放链接=https://x.cn/base/a?table=tbl1&view=v1",
        )
        .expect("parse");
        assert_eq!(fields.get("项目名称"), Some(&"科技 事业部".to_string()));
        assert_eq!(
            fields.get("资料存放链接"),
            Some(&"https://x.cn/base/a?table=tbl1&view=v1".to_string())
        );
    }

    #[test]
    fn parse_alias_then_space_fields() {
        let (name, fields) =
            parse_bitable_step_args("@台账 项目名称=鹤山 状态=进行中").expect("parse");
        assert_eq!(name.as_deref(), Some("台账"));
        assert_eq!(fields.get("项目名称"), Some(&"鹤山".to_string()));
        assert_eq!(fields.get("状态"), Some(&"进行中".to_string()));
    }

    #[test]
    fn trailing_bare_token_joins_previous_value() {
        // 无法可靠区分「值内空格」与「漏写 =」；缺 = 的尾词会并进上一字段
        let (_, fields) =
            parse_bitable_step_args("项目名称=鹤山 资料存放链接").expect("parse");
        assert_eq!(fields.get("项目名称"), Some(&"鹤山 资料存放链接".to_string()));
        assert!(!fields.contains_key("资料存放链接"));
    }

    #[test]
    fn parse_sync_source() {
        let (name, source, _) =
            parse_sync_args("@项目提报台账,来源=files/考勤.xlsx").expect("parse");
        assert_eq!(name.as_deref(), Some("项目提报台账"));
        assert_eq!(source, "files/考勤.xlsx");
    }

    #[test]
    fn parse_sync_source_with_newline() {
        let (name, source, _) =
            parse_sync_args("@项目提报台账\n来源=files/考勤.xlsx").expect("parse");
        assert_eq!(name.as_deref(), Some("项目提报台账"));
        assert_eq!(source, "files/考勤.xlsx");
    }

    #[test]
    fn parse_query_equals() {
        let (_, conds) = parse_bitable_query_args("项目名称=鹤山").expect("parse");
        assert_eq!(conds.len(), 1);
        assert_eq!(conds[0].field, "项目名称");
        assert_eq!(conds[0].op, QueryOp::Is);
        assert_eq!(conds[0].value.as_deref(), Some("鹤山"));
    }

    #[test]
    fn parse_query_extended_ops() {
        let (_, conds) = parse_bitable_query_args(
            "项目名称!=鹤山，提报状态has未，备注!has作废，资料链接empty，提报人!empty",
        )
        .expect("parse");
        assert_eq!(conds.len(), 5);
        assert_eq!(conds[0].op, QueryOp::IsNot);
        assert_eq!(conds[1].op, QueryOp::Contains);
        assert_eq!(conds[2].op, QueryOp::DoesNotContain);
        assert_eq!(conds[3].op, QueryOp::IsEmpty);
        assert_eq!(conds[4].op, QueryOp::IsNotEmpty);
    }

    #[test]
    fn parse_query_space_separated() {
        let (_, conds) =
            parse_bitable_query_args("项目名称has鹤山 提报状态empty").expect("parse");
        assert_eq!(conds.len(), 2);
        assert_eq!(conds[0].value.as_deref(), Some("鹤山"));
        assert_eq!(conds[1].op, QueryOp::IsEmpty);
    }

    #[test]
    fn parse_query_value_with_spaces() {
        let (_, conds) =
            parse_bitable_query_args("项目名称=科技 事业部 提报状态=未提报").expect("parse");
        assert_eq!(conds.len(), 2);
        assert_eq!(conds[0].value.as_deref(), Some("科技 事业部"));
        assert_eq!(conds[1].field, "提报状态");
    }

    #[test]
    fn parse_query_with_alias() {
        let (name, conds) =
            parse_bitable_query_args("@项目提报台账,提报状态=未提报").expect("parse");
        assert_eq!(name.as_deref(), Some("项目提报台账"));
        assert_eq!(conds.len(), 1);
    }
}
