use super::store::AssertScriptDef;
use chrono::Utc;

pub fn default_assert_scripts() -> Vec<AssertScriptDef> {
    let now = Utc::now();
    vec![
        // —— 标题 / URL（无需 XPath）——
        def(
            now,
            "builtin_assert_check_title",
            "检查页面标题",
            "断言标题包含 ARGS。用法：断言检查页面标题 登录",
            CHECK_TITLE_SCRIPT,
        ),
        def(
            now,
            "builtin_assert_title_equals",
            "标题等于",
            "断言标题全等（去空白）。用法：断言标题等于 工作台",
            TITLE_EQUALS_SCRIPT,
        ),
        def(
            now,
            "builtin_assert_url_contains",
            "URL含",
            "断言地址包含 ARGS。用法：断言URL含 /dashboard",
            URL_CONTAINS_SCRIPT,
        ),
        def(
            now,
            "builtin_assert_url_equals",
            "URL等于",
            "断言地址全等。用法：断言URL等于 https://example.com/home",
            URL_EQUALS_SCRIPT,
        ),
        // —— 存在 / 不存在 / 可见 ——
        def(
            now,
            "builtin_assert_element_exists",
            "元素存在",
            "断言 ARGS 整段 XPath 至少匹配 1 个。用法：断言元素存在 //button",
            ELEMENT_EXISTS_SCRIPT,
        ),
        def(
            now,
            "builtin_assert_element_not_exists",
            "元素不存在",
            "断言 ARGS 的 XPath 匹配数为 0。用法：断言元素不存在 //div[contains(@class,'error')]",
            ELEMENT_NOT_EXISTS_SCRIPT,
        ),
        def(
            now,
            "builtin_assert_element_visible",
            "元素可见",
            "断言 XPath 首个节点存在且可见。用法：断言元素可见 //nav",
            ELEMENT_VISIBLE_SCRIPT,
        ),
        // —— 常用 XPath 模式（ARGS 尽量只填变化部分）——
        def(
            now,
            "builtin_assert_by_tag",
            "按标签存在",
            "断言存在该标签。用法：断言按标签存在 button（默认 button）",
            BY_TAG_SCRIPT,
        ),
        def(
            now,
            "builtin_assert_by_id",
            "按ID存在",
            "断言 //*[@id=…]。用法：断言按ID存在 loginBtn",
            BY_ID_SCRIPT,
        ),
        def(
            now,
            "builtin_assert_class_contains",
            "class含",
            "断言存在 class 包含关键字的元素。用法：断言class含 error",
            CLASS_CONTAINS_SCRIPT,
        ),
        def(
            now,
            "builtin_assert_button_text",
            "按钮含文案",
            "断言按钮文字包含 ARGS（空白不敏感）。用法：断言按钮含文案 查询",
            BUTTON_TEXT_SCRIPT,
        ),
        def(
            now,
            "builtin_assert_button_text_exact",
            "精确按钮文案",
            "断言存在 normalize-space 文案精确等于 ARGS 的按钮。用法：断言精确按钮文案 确定",
            BUTTON_TEXT_EXACT_SCRIPT,
        ),
        def(
            now,
            "builtin_assert_nth_button",
            "第N个按钮",
            "断言第 N 个匹配按钮存在。用法：断言第N个按钮 1 或 断言第N个按钮 2 删除",
            NTH_BUTTON_SCRIPT,
        ),
        def(
            now,
            "builtin_assert_form_submit",
            "表单内提交",
            "断言表单内 type=submit 按钮存在。用法：断言表单内提交 login（表单 id）",
            FORM_SUBMIT_SCRIPT,
        ),
        def(
            now,
            "builtin_assert_button_enabled",
            "按钮可点",
            "断言含文案且未 disabled 的按钮存在。用法：断言按钮可点 提交",
            BUTTON_ENABLED_SCRIPT,
        ),
        // —— 文本 / 属性 / 计数 ——
        def(
            now,
            "builtin_assert_text_contains",
            "文本包含",
            "断言节点文本包含期望（去空白）。用法：断言文本包含 //h1|||欢迎",
            TEXT_CONTAINS_SCRIPT,
        ),
        def(
            now,
            "builtin_assert_attr_equals",
            "属性等于",
            "断言属性 trim 后全等。用法：断言属性等于 //input[@id='phone']|||placeholder|||手机号",
            ATTR_EQUALS_SCRIPT,
        ),
        def(
            now,
            "builtin_assert_count_equals",
            "恰好N个",
            "断言 XPath 匹配数恰好为 N。用法：断言恰好N个 //div[contains(@class,'toast')]|||1",
            COUNT_EQUALS_SCRIPT,
        ),
    ]
}

fn def(
    now: chrono::DateTime<Utc>,
    id: &str,
    name: &str,
    description: &str,
    script: &str,
) -> AssertScriptDef {
    AssertScriptDef {
        id: id.into(),
        name: name.into(),
        description: description.into(),
        builtin: true,
        script: script.into(),
        updated_at: now,
    }
}

// —— scripts ——

const CHECK_TITLE_SCRIPT: &str = r#"/// 用法：断言检查页面标题 登录

let expect = ARGS;
if expect == "" {
    throw "请提供标题关键字，例如：断言检查页面标题 登录";
}
print("断言标题包含: " + expect);
assert_title_contains(expect);
print("标题断言通过");
"#;

const TITLE_EQUALS_SCRIPT: &str = r#"/// 用法：断言标题等于 工作台

let expect = ARGS;
if expect == "" {
    throw "请提供完整标题，例如：断言标题等于 工作台";
}
print("断言标题等于: " + expect);
assert_title_equals(expect);
print("标题等于断言通过");
"#;

const URL_CONTAINS_SCRIPT: &str = r#"/// 用法：断言URL含 /dashboard

let expect = ARGS;
if expect == "" {
    throw "请提供 URL 片段，例如：断言URL含 /dashboard";
}
print("断言 URL 包含: " + expect);
assert_url_contains(expect);
print("URL 包含断言通过");
"#;

const URL_EQUALS_SCRIPT: &str = r#"/// 用法：断言URL等于 https://example.com/home

let expect = ARGS;
if expect == "" {
    throw "请提供完整 URL，例如：断言URL等于 https://example.com/home";
}
print("断言 URL 等于: " + expect);
assert_url_equals(expect);
print("URL 等于断言通过");
"#;

const ELEMENT_EXISTS_SCRIPT: &str = r#"/// 用法：断言元素存在 //button
///       断言元素存在 //button[contains(.,'提交')]

let xp = ARGS;
if xp == "" {
    throw "请提供 XPath，例如：断言元素存在 //button";
}
print("断言存在: " + xp);
assert_exists(xp, 10);
print("元素存在断言通过");
"#;

const ELEMENT_NOT_EXISTS_SCRIPT: &str = r#"/// 用法：断言元素不存在 //div[contains(@class,'error')]

let xp = ARGS;
if xp == "" {
    throw "请提供 XPath，例如：断言元素不存在 //div[contains(@class,'error')]";
}
print("断言不存在: " + xp);
assert_not_exists(xp, 5);
print("元素不存在断言通过");
"#;

const ELEMENT_VISIBLE_SCRIPT: &str = r#"/// 用法：断言元素可见 //nav

let xp = ARGS;
if xp == "" {
    throw "请提供 XPath，例如：断言元素可见 //button[contains(.,'查询')]";
}
print("断言可见: " + xp);
assert_visible(xp, 10);
print("元素可见断言通过");
"#;

const BY_TAG_SCRIPT: &str = r#"/// 用法：断言按标签存在 button
///       断言按标签存在 h1

let tag = str_trim(ARGS);
if tag == "" {
    tag = "button";
}
if str_index_of(tag, "/") >= 0 || str_index_of(tag, "[") >= 0 || str_index_of(tag, " ") >= 0 {
    throw "标签名请写简单名，如 button、h1、input；完整 XPath 请用：断言元素存在";
}
let xp = "//" + tag;
print("断言按标签存在: " + xp);
assert_exists(xp, 10);
print("按标签存在断言通过");
"#;

const BY_ID_SCRIPT: &str = r#"/// 用法：断言按ID存在 loginBtn

let id = str_trim(ARGS);
if id == "" {
    throw "请提供元素 id，例如：断言按ID存在 loginBtn";
}
if str_index_of(id, "'") >= 0 {
    throw "id 中请勿包含单引号";
}
let xp = "//*[@id='" + id + "']";
print("断言按 ID 存在: " + xp);
assert_exists(xp, 10);
print("按 ID 存在断言通过");
"#;

const CLASS_CONTAINS_SCRIPT: &str = r#"/// 用法：断言class含 error

let tip = str_trim(ARGS);
if tip == "" {
    throw "请提供 class 关键字，例如：断言class含 error";
}
if str_index_of(tip, "'") >= 0 {
    throw "关键字中请勿包含单引号";
}
let xp = "//*[contains(@class,'" + tip + "')]";
print("断言 class 含: " + xp);
assert_exists(xp, 10);
print("class 含断言通过");
"#;

const BUTTON_TEXT_SCRIPT: &str = r#"/// 用法：断言按钮含文案 查询
/// 页面写成「查 询」也能命中（空白不敏感）

let t = str_trim(ARGS);
if t == "" {
    throw "请提供按钮文案，例如：断言按钮含文案 查询";
}
if str_index_of(t, "'") >= 0 {
    throw "文案中请勿包含单引号；复杂定位请用：断言元素存在";
}
let xp = "//button[contains(.,'" + t + "')]";
print("断言按钮含文案: " + xp);
assert_exists(xp, 10);
print("按钮含文案断言通过");
"#;

const BUTTON_TEXT_EXACT_SCRIPT: &str = r#"/// 用法：断言精确按钮文案 确定

let t = str_trim(ARGS);
if t == "" {
    throw "请提供精确按钮文案，例如：断言精确按钮文案 确定";
}
if str_index_of(t, "'") >= 0 {
    throw "文案中请勿包含单引号";
}
let xp = "//button[normalize-space(.)='" + t + "']";
print("断言精确按钮文案: " + xp);
assert_exists(xp, 10);
print("精确按钮文案断言通过");
"#;

const NTH_BUTTON_SCRIPT: &str = r#"/// 用法：断言第N个按钮 1
///       断言第N个按钮 2 删除   ← 第 2 个「含删除」的按钮

let s = str_trim(ARGS);
if s == "" {
    throw "用法：断言第N个按钮 1  或  断言第N个按钮 2 删除";
}
let n_str = s;
let tip = "";
let sp = str_index_of(s, " ");
if sp >= 0 {
    n_str = str_trim(str_slice(s, 0, sp));
    tip = str_trim(str_slice(s, sp + 1, str_len(s)));
}
let n = arg_int(n_str);
if n < 1 {
    throw "序号从 1 开始，例如：断言第N个按钮 1";
}
let xp = "";
if tip == "" {
    xp = "(//button)[" + n + "]";
} else {
    if str_index_of(tip, "'") >= 0 {
        throw "文案中请勿包含单引号";
    }
    xp = "(//button[contains(.,'" + tip + "')])[" + n + "]";
}
print("断言第 N 个按钮: " + xp);
assert_exists(xp, 10);
print("第 N 个按钮断言通过");
"#;

const FORM_SUBMIT_SCRIPT: &str = r#"/// 用法：断言表单内提交 login
/// 对应 XPath：//form[@id='login']//button[@type='submit']

let fid = str_trim(ARGS);
if fid == "" {
    throw "请提供表单 id，例如：断言表单内提交 login";
}
if str_index_of(fid, "'") >= 0 {
    throw "id 中请勿包含单引号";
}
let xp = "//form[@id='" + fid + "']//button[@type='submit']";
print("断言表单内提交: " + xp);
assert_exists(xp, 10);
print("表单内提交断言通过");
"#;

const BUTTON_ENABLED_SCRIPT: &str = r#"/// 用法：断言按钮可点 提交
/// 对应：//button[contains(.,'提交') and not(@disabled)]

let t = str_trim(ARGS);
if t == "" {
    throw "请提供按钮文案，例如：断言按钮可点 提交";
}
if str_index_of(t, "'") >= 0 {
    throw "文案中请勿包含单引号";
}
let xp = "//button[contains(.,'" + t + "') and not(@disabled)]";
print("断言按钮可点: " + xp);
assert_exists(xp, 10);
assert_visible(xp, 5);
print("按钮可点断言通过");
"#;

const TEXT_CONTAINS_SCRIPT: &str = r#"/// 用法：断言文本包含 //h1|||欢迎
/// 分隔符为三个竖线 |||

if arg_parts(ARGS, "|||") < 2 {
    throw "用法：断言文本包含 //h1|||欢迎";
}
let xp = arg_part(ARGS, "|||", 0);
let expect = arg_part(ARGS, "|||", 1);
if xp == "" || expect == "" {
    throw "XPath 与期望文案都不能为空：断言文本包含 //h1|||欢迎";
}
print("断言文本包含: " + xp + " ⊇ " + expect);
assert_text(xp, expect, 10);
print("文本包含断言通过");
"#;

const ATTR_EQUALS_SCRIPT: &str = r#"/// 用法：断言属性等于 //input[@id='phone']|||placeholder|||手机号

if arg_parts(ARGS, "|||") < 3 {
    throw "用法：断言属性等于 //input[@id='phone']|||placeholder|||手机号";
}
let xp = arg_part(ARGS, "|||", 0);
let name = arg_part(ARGS, "|||", 1);
let expect = arg_part(ARGS, "|||", 2);
if xp == "" || name == "" {
    throw "XPath 与属性名不能为空";
}
print("断言属性等于: " + xp + " @" + name + " = " + expect);
assert_attr(xp, name, expect, 10);
print("属性等于断言通过");
"#;

const COUNT_EQUALS_SCRIPT: &str = r#"/// 用法：断言恰好N个 //div[contains(@class,'toast')]|||1

if arg_parts(ARGS, "|||") < 2 {
    throw "用法：断言恰好N个 //button|||3";
}
let xp = arg_part(ARGS, "|||", 0);
let n = arg_int(arg_part(ARGS, "|||", 1));
if xp == "" {
    throw "请提供 XPath";
}
if n < 0 {
    throw "数量不能为负";
}
print("断言恰好 " + n + " 个: " + xp);
let got = xpath_count(xp);
if got != n {
    throw "断言失败：期望 " + n + " 个，实际 " + got;
}
print("恰好 N 个断言通过（" + got + "）");
"#;
