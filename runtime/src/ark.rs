use crate::browser_context::{self, BrowserTabSnapshot};
use crate::commands::{self, CommandKind};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use crate::ui_host::{AppHandle, Emitter, Runtime};

const ARK_BASE_URL: &str = "https://ark.cn-beijing.volces.com/api/v3";
const ARK_ENDPOINT_ID: &str = "doubao-seed-2-0-code-preview-260215";
/// Optional Volcano Ark key — set via env `WEBCLAW_ARK_API_KEY` (never commit secrets).
fn ark_api_key() -> String {
    std::env::var("WEBCLAW_ARK_API_KEY").unwrap_or_default()
}

/// 全站统一思考档位：minimal / low / medium / high
const ARK_REASONING_EFFORT: &str = "medium";

/// 为请求体统一注入思考档位（覆盖调用方未指定的情况）
fn apply_thinking_defaults(mut body: Value) -> Value {
    if let Some(obj) = body.as_object_mut() {
        obj.insert(
            "thinking".into(),
            json!({ "type": "enabled" }),
        );
        obj.insert(
            "reasoning_effort".into(),
            json!(ARK_REASONING_EFFORT),
        );
    }
    body
}

const SYSTEM_PROMPT_GENERIC: &str = r#"你是浏览器自动化快捷指令编写助手。
请只输出快捷指令正文，不要解释、不要 markdown 代码块。

步骤用中文逗号、英文逗号、顿号、分号、句号（。/.）、空格或换行分隔。英文句号后接字母数字时不拆分（保留 URL）。

## 试探 / 备选（括号 + 问号）
存在则点、不存在则新建等场景用：
- （试探步骤？备选步骤…） 或 (试探?备选…)
- 备选可省略：（试探步骤？）— 仅试探，失败则跳过并继续后续步骤
- 中英文括号/问号均可：（）？ 与 ()?
- 试探成功 → 跳过问号后全部备选；试探失败 → 不刷新重试，有备选则执行备选，无备选则继续
- 例：（点击2026-06？点击新建，点击子文件夹，输入名称=2026-06，点击创建）
- 例：（点击可选弹窗？），点击下一步

## 核心指令语法
- 打开<URL>
- 点击<页面可见文字>  或  点击<文字>#N（第 N 个匹配，如 点击考勤组织#2；文字含 # 用 \\#）
- 点击<结构化目标> — 例：点击按钮#2、点击图标按钮#1、点击表格#1行#1图标按钮#2
- 点击<x> <y> — 视口像素坐标（空格分隔，单位 px），用于点空白关弹层；无参时 点击 等价 点击 0 0
- 点击图标:<name> — 例：点击图标:close-circle、点击图标:close-circle#2（须带冒号；名来自 class/data-icon/aria/svg 等，与文字点击区分）
- 输入<字段>=<内容> — 例：手机=13800138000、密码=你的密码、月份=2026-05、输入框#2=关键词（仅填内容不写=字段时，自动匹配主搜索框）
- 按<按键> — 例：按回车、按Tab、按Esc、按空格、按下箭头（可写次数：按回车2）；可写 按回车到输入框#1 显式指定焦点
- 参数名称=值 — 例：参数公司=科技事业部；正文用 {{公司}} 引用（执行时绑定）
- 动态页签：{{页签地址}} / {{页签标题}}；系统剪切板：{{剪切板}}
- 捕获后再用：参数云盘地址={{页签地址}}，…，输入输入框#1={{云盘地址}}
- 也可：输入={{剪切板}}（执行该步时读取用户当前剪切板）
- 打开标签<URL>（可选，新建标签页）
- 关闭标签 — 关闭当前标签并切到剩余活跃标签（至少保留 1 个）
- 页签#N — 切换到第 N 个页签；页签关闭 / 页签关闭#N — 关闭当前或指定页签（之后自动激活活跃标签）
- 剪切板复制=内容 — 写入系统剪切板；剪切板复制 — 复制页面选中文字；剪切板粘贴 — 粘贴到当前焦点
- 刷新 — 刷新当前标签页并等待页面就绪
- 滚动<像素> / 滚动下500 / 滚动上300
- 截图（保存到运行产物 screenshots/）
- 采集（当前页可见文字与表格写入 documents/*.md；飞书多维表格页写入「表格」；无参数=全部；可写分类：采集表格、采集按钮、采集标题、采集链接、采集输入框、采集其它文字）
- AI（分析本次运行产物中的 .md/.log/.txt/.csv/.xlsx/.xls/.docx；无参数=自动总结；例：AI 哪些记录异常？）
- 宏:<宏名称>（引用已定义宏）
- 技能<技能名称> — 展开技能列表中同名技能的全部步骤，如：技能申请年假；可组合：技能申请年假，刷新，点击返回
- 脚本<脚本名> — 运行命名脚本

## 等待（智能，条件满足即继续，数字为超时秒数）
优先用智能等待，少用固定秒数：
- 等待2 — 固定 sleep 2 秒（仅简单缓冲时用）
- 等待文字<文字><秒> — 例：等待文字登录10
- 等待选择器<CSS><秒> — 例：等待选择器#login_submit15
- 等待消失<文字><秒> — 例：等待消失加载中10（遮罩/loading 消失）
- 等待加载<秒> — 例：等待加载10（页面 readyState）
- 等待遮罩<秒> — 例：等待遮罩10（常见 loading 遮罩）
- 等待下载<秒> — 等到本轮新文件已落入产物目录再继续（默认 200 秒）；下载在任务期间自动截获落盘，本指令只作同步。例：等待下载、等待下载60
- 打印→PDF：任务期间拦截页面 `window.print()`（不弹 Chrome 打印预览），静默导出 PDF 到 downloads/；菜谱仍写：点击打印，等待下载
- 等待<结构化目标><秒> — 例：等待输入框#1 15、等待按钮#2 10

## 结构化目标（图标按钮/表格等无文字元素时优先）
类型：表格、行、列、按钮、输入框、图标按钮；多层直接拼接，如 表格#1行#1列#4、表格#1行#1图标按钮#2。
- 点击按钮#2、点击图标按钮#1
- 点击表格#1行#1列#4
- 点击表格#1行#1图标按钮#2
- 输入输入框#1=关键词
- 等待输入框#1 15

## 飞书多维表格（需开放平台凭证；目标表=浏览器当前页）
- 先在浏览器打开目标多维表，再执行：
- 飞书新增,字段=值,...（也可用空格分隔；值可用 {{参数}} / {{页签地址}}，勿写死日常会变的数据）
- 飞书更新@别名,唯一键字段=值,...,其他字段=新值（@别名用于带上登记的唯一键）
- 飞书删除,字段=值,...
- 飞书查询,字段=值,...（条件可选；也支持 !=、has、!has、empty、!empty；结果保存到 documents/）
- 填报示例：参数公司=科技事业部，参数月份=2026-06，打开云盘文件夹，参数资料链接={{页签地址}}，打开多维表，飞书新增 项目名称={{公司}} 归属月份={{月份}} 资料存放链接={{资料链接}}
- 飞书同步,来源=files/或downloads/下的Excel路径
- 可选 @别名（如 @项目提报台账）不改变目标表地址，仅补充唯一键/显示名

## 页面文件上传（CDP，对照「等待下载」）
菜谱含「上传」时，整单执行期间自动屏蔽系统选文件窗口，结束后取消。用户按正常顺序写即可：
- 点击上传，点击上传文件，上传 — 仅 1 个附件
- 点击上传，点击上传文件，上传 考勤 — 部分文件名（files/ + downloads/）
- 点击上传，点击上传文件，上传 月报#2 — 多命中时 #序号
「上传」本身只写入文件，不代替点击。

## 飞书云盘上传（Open API，不依赖页面文件框）
- 先在浏览器打开目标云盘文件夹（URL 含 /drive/folder/…），「添加附件」到 files/ 后：
- 飞书上传 — 仅 1 个附件时
- 飞书上传 考勤 — 部分文件名（files/ + downloads/）
- 飞书上传 月报#2 — 多命中时 #序号
- 飞书上传 考勤,文件夹=https://xxx.feishu.cn/drive/folder/TOKEN — 不依赖当前页
- 与页面「上传」不同：飞书上传走开放平台 API，不点网页上传菜单

## 飞书推送（经中继发到已绑定用户私聊；与「飞书上传」不同）
- 推送消息 任务已完成 — 发送纯文本
- 推送附件 — 仅 1 个产物附件时可省略文件名
- 推送附件 考勤 — 部分文件名
- 推送附件 考勤 月报#2 — 多个附件用空格 / 逗号 / 顿号 / 分号分隔（勿与下一步指令混写在同一段）
- 推送AI — 自动总结本次运行产物并推送
- 推送AI 请列出异常 — 按提问分析后推送
- 需已开启飞书并完成绑定；不依赖浏览器当前页

## 编写要求
1. 步骤顺序符合真实操作流程
2. 点击文字必须与页面上可见文字一致（可用部分匹配）；不要使用“搜索框/搜索按钮”这种页面未必存在的抽象词，优先用占位符/aria 文案或「等待选择器」
3. 登录：优先用 登录<系统名>（如 登录EHR系统）；或 打开登录 URL → 输入账号 → 输入密码 → 点击登录
4. 页面跳转/菜单展开后：等待文字<下一屏关键文字>10，而非盲目 等待2
5. 导出/下载：点击导出/下载 → 等待下载（默认 200 秒，或 等待下载N）→ 点击关闭；若页面是「打印」走 window.print，同样：点击打印 → 等待下载（程序拦截打印窗并静默存 PDF）
6. 网页上传文件：点击上传 → 点击上传文件 → 上传（或 上传 部分名）
7. 参数内若含逗号，用 \, 转义；点击目标文字若含 #，用 \\# 转义（否则末尾 #数字 表示第 N 个匹配）
8. 每步只用一种指令；用点击菜单导航，不要写 URL hash
9. 搜索场景：等待输入框#1 15 → 输入输入框#1=关键词 → 按回车（自动继承输入焦点）；若页面是图标搜索按钮，可写 点击图标按钮#1；勿用「点击搜索框/搜索」等抽象词
10. 表格单元格/行内操作：点击表格#1行#N列#K；行内图标用 点击表格#1行#N图标按钮#M
11. 下方「已注册指令」列表中的自定义关键词也可使用
12. 若用户需求与「已缓存技能」中某条名称/用途相符，优先写 技能<技能名称>（名称须与列表完全一致），不要把整段技能步骤再抄一遍；可与其它步骤组合：技能请假，刷新，点击返回

## 当前浏览器与对话上下文（必须遵守）
若用户消息中包含「--- 当前浏览器 ---」或「--- 对话上下文 ---」段落：
1. **站点延续**：用户已在某站点（见 URL/站点）且只说「搜索/输入/查找/最新…」而未指定新站点时，**禁止** 打开百度/Google/必应等其它搜索引擎；直接在**当前页**写：等待输入框#1 15 → 输入输入框#1=关键词 → 按回车
2. **避免重复打开**：若 URL 已是目标站，不要写 打开<同站 URL>
3. **对话优先**：若对话上下文显示用户刚打开/访问某站，后续模糊指令默认在该站继续
4. **显式切换除外**：用户明确说「去百度搜」「打开京东」时才切换站点
5. **技能优先**：若消息含「--- 已缓存技能 ---」且需求匹配其中技能，优先输出 技能名称"#;

const SYSTEM_PROMPT_FAILURE_ANALYSIS: &str = r#"你是浏览器自动化任务的失败诊断助手。
根据失败步骤、错误信息、当前浏览器状态、探针快照（首次失败 / 刷新后对照）与近期执行日志，用简洁中文说明失败原因与处理建议。

输出要求：
1. 只输出诊断正文，不要 markdown 代码块、不要开场寒暄
2. 固定用以下三行标题（可多行补充，全文 3～8 行）：
可能原因：…
建议处理：…
继续建议：适合「继续当前步骤」/「继续下一步骤」/「需先手工处理后继续」之一，并一句说明
3. 结合错误原文与页面信号判断（元素未找到、页面未加载、登录失败、只读输入框、验证码等）
4. 若有「首次失败时」与「刷新重试后」两份探针：优先对照同一序号（如图标按钮#N）的 class/文案是否漂移，判断是否点错结构化序号
5. 不要编造未见过的页面内容；不确定时明确写「不确定」
6. 不要给出新的完整快捷指令菜谱"#;

const SYSTEM_PROMPT_STEP_REPAIR: &str = r#"你是浏览器自动化失败修复助手。根据失败步骤、错误、当前页探针目录、探针快照对照（首次失败 / 刷新后）与后续步骤，输出一份可立即执行的补丁计划。

只输出一个 JSON 对象，不要 markdown 围栏、不要解释。格式：
{"confidence":"high"|"medium"|"low","need_human":false,"reason":"一句话原因","ops":[...]}

ops 允许的 op：
- {"op":"retry_as","step":"点击查询"}  用新菜谱替换当前失败步（优先用探针里的 click_cmd / index_cmd / text_cmd）
- {"op":"insert_before","steps":["点击关闭","等待查询10"]}  在当前步前插入（关弹窗、展开菜单、等待、按Esc）
- {"op":"skip"}  目标已达成（如等待的文字已在页面上）
- {"op":"reload_then_retry"}  页面会话错乱，需要刷新后再跑当前步
- {"op":"ask_human"}  验证码、需改账号、页面完全对不上

规则：
1. 优先从「页面探针」里选已有指令，不要编造未见过的按钮文字
2. 一次最多 insert_before 5 步；步骤必须是合法快捷指令（点击/输入/等待/按/刷新/滚动/打开/打开标签/关闭标签/页签/登录/人机验证/剪切板/截图/采集/AI）
3. 禁止输出飞书、上传、脚本、技能、宏、参数
4. 不确定就 need_human=true 并 ops=[{"op":"ask_human"}]，confidence=low
5. 只读/禁用输入框：改成点击对应日期/下拉控件，不要再输入
6. 出现登录/密码/会话过期：insert_before 登录<系统名>（若上下文能判断系统），否则 ask_human
7. 弹窗挡住：先点击关闭/确定/取消或按Esc，再 retry 原步骤
8. 若探针快照显示同一序号在首次失败时指向错误控件、刷新后序号回归正确：优先 retry_as 为当前探针里正确的图标按钮/按钮序号，或 insert_before 打开筛选/组织树后再点目标文字
"#;

fn extract_url(text: &str) -> Option<String> {
    for token in text.split_whitespace() {
        let t = token.trim_matches(|c: char| "，。；;,.()（）\"'".contains(c));
        if t.starts_with("http://") || t.starts_with("https://") {
            return Some(t.to_string());
        }
        if t.contains("hcmcloud.cn") || t.contains("://") {
            let url = if t.starts_with("http") {
                t.to_string()
            } else {
                format!("https://{t}")
            };
            return Some(url);
        }
    }
    for line in text.lines() {
        if let Some(rest) = line.split('：').nth(1).or_else(|| line.split(':').nth(1)) {
            let t = rest.trim();
            if t.starts_with("http") {
                return Some(t.split_whitespace().next()?.to_string());
            }
        }
    }
    None
}

fn extract_credentials(text: &str) -> (Option<String>, Option<String>) {
    let mut account = None;
    let mut password = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("账号") {
            if let Some((_, v)) = line.split_once('：').or_else(|| line.split_once(':')) {
                let v = v.trim();
                if !v.is_empty() {
                    account = Some(v.to_string());
                }
            }
        }
        if line.starts_with("密码") {
            if let Some((_, v)) = line.split_once('：').or_else(|| line.split_once(':')) {
                let v = v.trim();
                if !v.is_empty() {
                    password = Some(v.to_string());
                }
            }
        }
    }
    (account, password)
}

#[derive(Debug, Clone, Default)]
pub struct RecipeAiContext {
    pub browser: Option<BrowserTabSnapshot>,
    pub chat_transcript: Option<String>,
}

fn build_recipe_user_message(
    user_prompt: &str,
    existing: Option<&str>,
    ai_ctx: Option<&RecipeAiContext>,
) -> Result<(String, String), String> {
    let system = SYSTEM_PROMPT_GENERIC.to_string();

    let mut user_msg = String::new();
    user_msg.push_str("请将以下自然语言需求转换为可执行快捷指令（仅输出快捷指令正文）：\n\n");

    if let Some(ctx) = ai_ctx {
        if let Some(ref snap) = ctx.browser {
            if !snap.url.is_empty() && snap.url != "about:blank" {
                user_msg.push_str("--- 当前浏览器 ---\n");
                user_msg.push_str(&browser_context::format_browser_context_section(snap));
                user_msg.push_str("\n\n");
            }
        }
        if let Some(ref transcript) = ctx.chat_transcript {
            let t = transcript.trim();
            if !t.is_empty() {
                user_msg.push_str("--- 对话上下文（最近几轮）---\n");
                user_msg.push_str(t);
                user_msg.push_str("\n\n");
            }
        }
    }

    user_msg.push_str("--- 当前需求 ---\n");
    user_msg.push_str(user_prompt);

    if let Some(url) = extract_url(user_prompt) {
        user_msg.push_str(&format!("\n\n[解析] 打开 URL: {url}"));
    }
    let (acc, pwd) = extract_credentials(user_prompt);
    if let Some(a) = acc {
        user_msg.push_str(&format!("\n[解析] 账号: {a}"));
    }
    if pwd.is_some() {
        user_msg.push_str("\n[解析] 已提供密码（快捷指令中用 输入密码=xxx）");
    }

    if let Some(ex) = existing {
        if !ex.trim().is_empty() {
            user_msg.push_str(&format!("\n\n当前草稿（可修改完善）：\n{ex}"));
        }
    }

    if let Ok(catalog) = command_catalog_text() {
        user_msg.push_str("\n\n---\n");
        user_msg.push_str(&catalog);
    }
    if let Ok(skills) = skills_catalog_text() {
        if !skills.trim().is_empty() {
            user_msg.push_str("\n\n--- 已缓存技能 ---\n");
            user_msg.push_str(&skills);
        }
    }

    Ok((system, user_msg))
}

pub fn build_recipe_chat_body(
    user_prompt: &str,
    existing: Option<&str>,
    ai_ctx: Option<&RecipeAiContext>,
) -> Result<Value, String> {
    ensure_ark_key()?;
    let (system, user_msg) =
        build_recipe_user_message(user_prompt, existing, ai_ctx)?;
    Ok(json!({
        "model": ARK_ENDPOINT_ID,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user_msg }
        ],
        "temperature": 0.2,
        "max_tokens": 4096
    }))
}

pub async fn chat_completion_with_messages(
    system: &str,
    user: &str,
    temperature: f64,
    max_tokens: u64,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<RecipeAssistResult, String> {
    ensure_ark_key()?;
    let body = json!({
        "model": ARK_ENDPOINT_ID,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user }
        ],
        "temperature": temperature,
        "max_tokens": max_tokens
    });
    let once = chat_completion_once(body, cancel).await?;
    Ok(RecipeAssistResult {
        content: once.content,
        usage: once.usage,
    })
}

pub async fn generate_recipe_content(
    user_prompt: &str,
    existing: Option<&str>,
    ai_ctx: Option<&RecipeAiContext>,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<RecipeAssistResult, String> {
    let body = build_recipe_chat_body(user_prompt, existing, ai_ctx)?;
    let once = chat_completion_once(body, cancel).await?;
    Ok(RecipeAssistResult {
        content: clean_recipe_output(&once.content),
        usage: once.usage,
    })
}

/// AI 增强返回：快捷指令正文 + Token 用量
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipeAssistResult {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

fn parse_token_usage(v: &Value) -> Option<TokenUsage> {
    let u = v.get("usage")?;
    let prompt_tokens = u
        .get("prompt_tokens")
        .and_then(|x| x.as_u64())
        .or_else(|| u.get("input_tokens").and_then(|x| x.as_u64()))
        .unwrap_or(0);
    let completion_tokens = u
        .get("completion_tokens")
        .and_then(|x| x.as_u64())
        .or_else(|| u.get("output_tokens").and_then(|x| x.as_u64()))
        .unwrap_or(0);
    let total_tokens = u
        .get("total_tokens")
        .and_then(|x| x.as_u64())
        .unwrap_or(prompt_tokens.saturating_add(completion_tokens));
    if prompt_tokens == 0 && completion_tokens == 0 && total_tokens == 0 {
        return None;
    }
    Some(TokenUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
    })
}

struct ChatCompletionOnce {
    content: String,
    usage: Option<TokenUsage>,
}

const SYSTEM_PROMPT_SUMMARIZE: &str = r#"你是浏览器自动化快捷指令编写专家。根据用户与助手的对话记录，提炼出一条完整、可执行的快捷指令正文。
要求：
- 只输出快捷指令正文，不要解释、不要 markdown
- 用中文逗号、英文逗号、空格或换行分隔步骤
- 合并重复操作，补全合理的等待与关闭弹窗步骤
- 只使用已注册指令关键词（见下方 catalog）：打开、点击、等待、输入、打开标签、关闭标签、页签、剪切板、刷新、滚动、截图、采集、AI、推送消息、推送附件、推送AI、宏:名称、技能名称 及用户自定义指令
- 若有「已缓存技能」且与对话目标相符，优先写 技能<技能名称>（名称完全一致），勿重复展开技能正文
- 等待优先用智能形式：等待文字…、等待下载（默认200）、等待遮罩10、等待消失加载中10；固定 等待N 仅作简单缓冲
- 导出/下载后必须 等待下载（或 等待下载N），避免尚未落盘就点关闭
- 若对话中已有可执行步骤，以最终达成目标的最短路径为准"#;

pub async fn summarize_chat_to_recipe(chat_transcript: &str) -> Result<String, String> {
    if ark_api_key().trim().is_empty() {
        return Err("请先设置环境变量 WEBCLAW_ARK_API_KEY".into());
    }
    if chat_transcript.trim().is_empty() {
        return Err("对话记录为空".into());
    }

    let system = SYSTEM_PROMPT_SUMMARIZE.to_string();

    let mut user_msg = String::from("请根据以下对话记录总结为一条可执行快捷指令（仅输出快捷指令正文）：\n\n");
    user_msg.push_str(chat_transcript);

    if let Ok(catalog) = command_catalog_text() {
        user_msg.push_str("\n\n---\n");
        user_msg.push_str(&catalog);
    }
    if let Ok(skills) = skills_catalog_text() {
        if !skills.trim().is_empty() {
            user_msg.push_str("\n\n--- 已缓存技能 ---\n");
            user_msg.push_str(&skills);
        }
    }

    let body = apply_thinking_defaults(json!({
        "model": ARK_ENDPOINT_ID,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user_msg }
        ],
        "temperature": 0.2,
        "max_tokens": 4096
    }));

    let url = format!("{ARK_BASE_URL}/chat/completions");
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .bearer_auth(ark_api_key())
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("方舟 API 请求失败: {e}"))?;

    let status = resp.status();
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("方舟 API 错误 ({status}): {v}"));
    }

    let content = v["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("方舟 API 返回内容为空")?
        .trim()
        .to_string();

    Ok(clean_recipe_output(&content))
}

pub fn clean_recipe_output(content: &str) -> String {
    let mut s = content
        .trim()
        .trim_start_matches("```")
        .trim_start_matches("text")
        .trim_end_matches("```")
        .trim()
        .to_string();
    if let Some(start) = s.find("打开") {
        if start > 0 {
            s = s[start..].to_string();
        }
    }
    s
}

fn command_catalog_text() -> Result<String, String> {
    let mut s = String::from("### 已注册指令（快捷指令中直接使用关键词）\n");
    for c in commands::list_commands()? {
        match c.kind {
            CommandKind::Script => {
                s.push_str(&format!(
                    "- {}：{}（例: {}{}）\n",
                    c.keyword,
                    c.description,
                    c.keyword,
                    example_args(&c.keyword)
                ));
            }
            CommandKind::Macro => {
                s.push_str(&format!("- 宏:{}（宏，多步展开）\n", c.name));
            }
        }
    }
    if let Ok(scripts) = crate::scripts::list_scripts() {
        if !scripts.is_empty() {
            s.push_str("\n### 命名脚本（写法：脚本名称，可加参数）\n");
            for sc in scripts {
                s.push_str(&format!(
                    "- 脚本{}：{}\n",
                    sc.name, sc.description
                ));
            }
        }
    }
    Ok(s)
}

/// 技能缓存目录（名称 + 正文摘要），控制长度以免撑爆上下文
fn skills_catalog_text() -> Result<String, String> {
    let skills = crate::recipe::list_skills_cache()?;
    if skills.is_empty() {
        return Ok(String::new());
    }
    const MAX_SKILLS: usize = 80;
    const MAX_PREVIEW_CHARS: usize = 120;
    let mut s = String::from(
        "### 已缓存技能（写法：技能名称；名称须与下列完全一致；匹配需求时优先引用）\n",
    );
    let total = skills.len();
    for (i, sk) in skills.iter().enumerate() {
        if i >= MAX_SKILLS {
            s.push_str(&format!("…另有 {} 个技能未列出\n", total - MAX_SKILLS));
            break;
        }
        let name = sk.name.trim();
        if name.is_empty() {
            continue;
        }
        let preview = truncate_skill_preview(&sk.content, MAX_PREVIEW_CHARS);
        if preview.is_empty() {
            s.push_str(&format!("- 技能{name}\n"));
        } else {
            s.push_str(&format!("- 技能{name}：{preview}\n"));
        }
    }
    Ok(s)
}

fn truncate_skill_preview(content: &str, max_chars: usize) -> String {
    let one_line: String = content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("，");
    let one_line = one_line.replace('\t', " ");
    if one_line.chars().count() <= max_chars {
        return one_line;
    }
    let truncated: String = one_line.chars().take(max_chars).collect();
    format!("{truncated}…")
}

fn example_args(keyword: &str) -> &'static str {
    match keyword {
        "打开" => "https://example.com",
        "点击" => "登录",
        "等待" => "文字登录10",
        "输入" => "密码=13800000000",
        "参数" => "云盘地址={{页签地址}}",
        "动态参数" => "云盘地址={{页签地址}}",
        "打开标签" => "https://example.com",
        "关闭标签" => "",
        "页签" => "#2",
        "剪切板" => "复制=示例",
        "滚动" => "下500",
        "截图" => "",
        "采集" => "表格",
        "脚本" => "合并考勤数据",
        "上传" => "考勤",
        "飞书上传" => "考勤",
        "推送消息" => "任务已完成",
        "推送附件" => "考勤",
        "推送AI" => "请列出异常",
        _ => "",
    }
}

const SYSTEM_PROMPT_SCRIPT: &str = r#"你是浏览器自动化指令脚本专家。用户编辑的是在某网页内执行的 JavaScript。
脚本通过 new Function('ctx', body) 执行，ctx = { args: string, runDir?, runId?, downloadsDir? }。
必须 return 一个对象，例如：
- 成功: { ok: true, message: "说明", x?, y?, matched?, needInsertText? }
- 失败: { ok: false, error: "原因" }
- 委托 Rust: { ok: true, delegate: "rust_open"|"rust_wait"|"rust_new_tab"|"rust_close_tab"|"rust_screenshot"|"rust_collect_page"|"rust_wait_download", timeoutSecs?, message? }

点击类脚本应返回 x,y 坐标（视口像素，矩形中心点）并设 clicked: false，由 Rust 通过 CDP 在中心单击；若已在脚本内 el.click() 可设 clicked: true 跳过 CDP。
输入类可返回 needInsertText 供 Rust 调用 CDP insertText。
「等待」类脚本：支持固定 sleep、轮询文字/选择器/遮罩；下载同步屏障用 delegate: "rust_wait_download"（落盘由任务自动下载完成）。
勿用 jQuery（$），用 document.querySelector 等原生 API。
不要 markdown 代码块，只输出脚本正文。"#;

pub fn build_script_chat_body(
    command_name: &str,
    keyword: &str,
    runner_hint: &str,
    user_prompt: &str,
    existing_script: Option<&str>,
) -> Result<Value, String> {
    ensure_ark_key()?;
    let mut user_msg = format!(
        "指令名称: {command_name}\n关键词: {keyword}\n运行方式: {runner_hint}\n\n需求:\n{user_prompt}"
    );
    if let Some(s) = existing_script {
        if !s.trim().is_empty() {
            user_msg.push_str(&format!("\n\n当前脚本:\n{s}"));
        }
    }

    Ok(json!({
        "model": ARK_ENDPOINT_ID,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT_SCRIPT },
            { "role": "user", "content": user_msg }
        ],
        "temperature": 0.2,
        "max_tokens": 2048
    }))
}

pub fn clean_script_output(content: &str) -> String {
    content
        .trim()
        .trim_start_matches("```javascript")
        .trim_start_matches("```js")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
        .to_string()
}

pub async fn generate_command_script(
    command_name: &str,
    keyword: &str,
    runner_hint: &str,
    user_prompt: &str,
    existing_script: Option<&str>,
) -> Result<String, String> {
    let body = build_script_chat_body(command_name, keyword, runner_hint, user_prompt, existing_script)?;
    let once = chat_completion_once(body, None).await?;
    Ok(clean_script_output(&once.content))
}

/// 步骤失败后供 AI 诊断的上下文
#[derive(Debug, Clone, Default)]
pub struct FailureAnalysisContext {
    pub step_label: String,
    pub error: String,
    /// 是否已做过页面恢复（刷新）重试
    pub recovery_attempted: bool,
    pub browser: Option<BrowserTabSnapshot>,
    pub recent_logs: Vec<String>,
    /// 页面探针摘要（可选，提高「继续建议」准确度）
    pub probe_summary: Option<String>,
    /// 首次失败瞬间的探针序号/class 快照（刷新前）
    pub probe_dump_at_fail: Option<String>,
    /// 自动刷新重试就绪后的探针快照
    pub probe_dump_after_recovery: Option<String>,
}

fn append_probe_dump_section(user_msg: &mut String, title: &str, dump: Option<&str>) {
    let Some(dump) = dump.map(str::trim).filter(|s| !s.is_empty()) else {
        return;
    };
    user_msg.push_str(&format!("\n--- {title} ---\n"));
    user_msg.push_str(dump);
    user_msg.push('\n');
}

fn build_failure_analysis_user_message(ctx: &FailureAnalysisContext) -> String {
    let mut user_msg = String::new();
    user_msg.push_str("--- 失败步骤 ---\n");
    user_msg.push_str(ctx.step_label.trim());
    user_msg.push('\n');

    user_msg.push_str("\n--- 错误信息 ---\n");
    user_msg.push_str(ctx.error.trim());
    user_msg.push('\n');

    user_msg.push_str("\n--- 恢复重试 ---\n");
    if ctx.recovery_attempted {
        user_msg.push_str("已自动刷新页面并重试，仍失败\n");
    } else {
        user_msg.push_str("未进入自动刷新重试（或为登录/验证码等即时中断）\n");
    }

    if let Some(ref snap) = ctx.browser {
        if !snap.url.is_empty() && snap.url != "about:blank" {
            user_msg.push_str("\n--- 当前浏览器 ---\n");
            user_msg.push_str(&browser_context::format_browser_context_section(snap));
            user_msg.push('\n');
        }
    }

    append_probe_dump_section(
        &mut user_msg,
        "探针快照（首次失败时，刷新前）",
        ctx.probe_dump_at_fail.as_deref(),
    );
    append_probe_dump_section(
        &mut user_msg,
        "探针快照（刷新重试后）",
        ctx.probe_dump_after_recovery.as_deref(),
    );

    if let Some(probe) = ctx.probe_summary.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        user_msg.push_str("\n--- 页面探针（可见目标）---\n");
        user_msg.push_str(probe);
        user_msg.push('\n');
    }

    if !ctx.recent_logs.is_empty() {
        user_msg.push_str("\n--- 近期执行日志（由旧到新）---\n");
        for line in &ctx.recent_logs {
            let t = line.trim();
            if !t.is_empty() {
                user_msg.push_str(t);
                user_msg.push('\n');
            }
        }
    }

    user_msg.push_str("\n请按系统要求输出失败诊断。");
    user_msg
}

/// 分析步骤失败原因（文本模型；失败/取消由调用方降级）
pub async fn analyze_step_failure(
    ctx: &FailureAnalysisContext,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<FailureAnalysisResult, String> {
    ensure_ark_key()?;
    let body = json!({
        "model": ARK_ENDPOINT_ID,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT_FAILURE_ANALYSIS },
            { "role": "user", "content": build_failure_analysis_user_message(ctx) }
        ],
        "temperature": 0.2,
        "max_tokens": 1024
    });
    let once = chat_completion_once(body, cancel).await?;
    let text = once.content.trim().to_string();
    if text.is_empty() {
        return Err("方舟 API 返回内容为空".into());
    }
    Ok(FailureAnalysisResult {
        content: text,
        usage: once.usage,
        reasoning: String::new(),
    })
}

/// 页面探针中的一条可执行候选（给修复模型看）
#[derive(Debug, Clone, Default)]
pub struct RepairProbeItem {
    pub cmd: String,
    pub desc: String,
    pub frame: Option<String>,
}

/// 步骤自动修复的观察上下文
#[derive(Debug, Clone, Default)]
pub struct StepRepairContext {
    pub step_label: String,
    pub error: String,
    pub recovery_attempted: bool,
    pub browser: Option<BrowserTabSnapshot>,
    pub upcoming_steps: Vec<String>,
    pub probe_items: Vec<RepairProbeItem>,
    pub recent_logs: Vec<String>,
    pub previous_notes: Vec<String>,
    /// 首次失败瞬间的探针序号/class 快照（刷新前）
    pub probe_dump_at_fail: Option<String>,
    /// 自动刷新重试就绪后的探针快照
    pub probe_dump_after_recovery: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairOpKind {
    RetryAs,
    InsertBefore,
    Skip,
    ReloadThenRetry,
    AskHuman,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairOp {
    pub op: RepairOpKind,
    #[serde(default)]
    pub step: Option<String>,
    #[serde(default)]
    pub steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairPlan {
    #[serde(default)]
    pub confidence: String,
    #[serde(default)]
    pub need_human: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub ops: Vec<RepairOp>,
}

impl RepairPlan {
    pub fn is_auto_applicable(&self) -> bool {
        if self.need_human {
            return false;
        }
        let c = self.confidence.trim().to_lowercase();
        if c == "low" {
            return false;
        }
        if self.ops.is_empty() {
            return false;
        }
        !self
            .ops
            .iter()
            .any(|op| matches!(op.op, RepairOpKind::AskHuman))
    }

    pub fn ask_human_reason(&self) -> Option<String> {
        if self.need_human || self.ops.iter().any(|op| matches!(op.op, RepairOpKind::AskHuman))
        {
            let r = self.reason.trim();
            if r.is_empty() {
                Some("需要人工处理".into())
            } else {
                Some(r.to_string())
            }
        } else if self.confidence.trim().eq_ignore_ascii_case("low") {
            Some(if self.reason.trim().is_empty() {
                "置信度过低，未自动改写".into()
            } else {
                self.reason.clone()
            })
        } else {
            None
        }
    }
}

pub fn build_step_repair_user_message(ctx: &StepRepairContext) -> String {
    let mut user_msg = String::new();
    user_msg.push_str("--- 失败步骤 ---\n");
    user_msg.push_str(ctx.step_label.trim());
    user_msg.push('\n');

    user_msg.push_str("\n--- 错误信息 ---\n");
    user_msg.push_str(ctx.error.trim());
    user_msg.push('\n');

    user_msg.push_str("\n--- 恢复重试 ---\n");
    if ctx.recovery_attempted {
        user_msg.push_str("已自动刷新页面并重试，仍失败\n");
    } else {
        user_msg.push_str("尚未刷新页面（请优先在当前页修复，避免丢掉表单）\n");
    }

    if let Some(ref snap) = ctx.browser {
        if !snap.url.is_empty() && snap.url != "about:blank" {
            user_msg.push_str("\n--- 当前浏览器 ---\n");
            user_msg.push_str(&browser_context::format_browser_context_section(snap));
            user_msg.push('\n');
        }
    }

    append_probe_dump_section(
        &mut user_msg,
        "探针快照（首次失败时，刷新前）",
        ctx.probe_dump_at_fail.as_deref(),
    );
    append_probe_dump_section(
        &mut user_msg,
        "探针快照（刷新重试后）",
        ctx.probe_dump_after_recovery.as_deref(),
    );

    if !ctx.probe_items.is_empty() {
        user_msg.push_str("\n--- 页面探针（可见目标，优先选用 cmd）---\n");
        for (i, item) in ctx.probe_items.iter().enumerate() {
            let frame = item
                .frame
                .as_deref()
                .map(|f| format!(" [{f}]"))
                .unwrap_or_default();
            let desc = item.desc.trim();
            if desc.is_empty() || desc == item.cmd {
                user_msg.push_str(&format!("{}. {}{}\n", i + 1, item.cmd, frame));
            } else {
                user_msg.push_str(&format!("{}. {}{} · {}\n", i + 1, item.cmd, frame, desc));
            }
        }
    }

    if !ctx.upcoming_steps.is_empty() {
        user_msg.push_str("\n--- 后续未执行步骤 ---\n");
        for (i, s) in ctx.upcoming_steps.iter().enumerate() {
            user_msg.push_str(&format!("{}. {}\n", i + 1, s.trim()));
        }
    }

    if !ctx.previous_notes.is_empty() {
        user_msg.push_str("\n--- 本步已尝试的修复 ---\n");
        for note in &ctx.previous_notes {
            let t = note.trim();
            if !t.is_empty() {
                user_msg.push_str(t);
                user_msg.push('\n');
            }
        }
    }

    if !ctx.recent_logs.is_empty() {
        user_msg.push_str("\n--- 近期执行日志（由旧到新）---\n");
        for line in &ctx.recent_logs {
            let t = line.trim();
            if !t.is_empty() {
                user_msg.push_str(t);
                user_msg.push('\n');
            }
        }
    }

    user_msg.push_str("\n请按系统要求只输出 JSON。");
    user_msg
}

fn extract_json_object(text: &str) -> Option<&str> {
    let t = text.trim();
    let t = t
        .trim_start_matches("```json")
        .trim_start_matches("```JSON")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let start = t.find('{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escape = false;
    for (i, c) in t[start..].char_indices() {
        if in_str {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&t[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_repair_op_kind(raw: &str) -> Option<RepairOpKind> {
    match raw.trim().to_lowercase().replace('-', "_").as_str() {
        "retry_as" | "retryas" | "replace" => Some(RepairOpKind::RetryAs),
        "insert_before" | "insertbefore" | "insert" => Some(RepairOpKind::InsertBefore),
        "skip" => Some(RepairOpKind::Skip),
        "reload_then_retry" | "reloadthenretry" | "reload" => Some(RepairOpKind::ReloadThenRetry),
        "ask_human" | "askhuman" | "human" => Some(RepairOpKind::AskHuman),
        _ => None,
    }
}

pub fn parse_repair_plan(text: &str) -> Result<RepairPlan, String> {
    let json_str = extract_json_object(text).ok_or("修复计划不是合法 JSON")?;
    let v: Value =
        serde_json::from_str(json_str).map_err(|e| format!("修复计划 JSON 解析失败: {e}"))?;
    let confidence = v
        .get("confidence")
        .and_then(|x| x.as_str())
        .unwrap_or("medium")
        .trim()
        .to_lowercase();
    let need_human = v
        .get("need_human")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let reason = v
        .get("reason")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let mut ops = Vec::new();
    if let Some(arr) = v.get("ops").and_then(|x| x.as_array()) {
        for item in arr {
            let kind_raw = item
                .get("op")
                .and_then(|x| x.as_str())
                .ok_or("修复计划 ops 缺少 op")?;
            let op = parse_repair_op_kind(kind_raw)
                .ok_or_else(|| format!("不支持的修复操作：{kind_raw}"))?;
            let step = item
                .get("step")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let steps = item
                .get("steps")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            ops.push(RepairOp { op, step, steps });
        }
    }
    Ok(RepairPlan {
        confidence,
        need_human,
        reason,
        ops,
    })
}

fn image_content_part(png: &[u8]) -> Value {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    json!({
        "type": "image_url",
        "image_url": { "url": format!("data:image/png;base64,{b64}") }
    })
}

fn is_vision_unsupported_error(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains("image")
        || e.contains("vision")
        || e.contains("multimodal")
        || e.contains("unsupported")
        || e.contains("invalid") && e.contains("content")
        || e.contains("unknown field")
}

/// 为失败步骤生成结构化修复计划（可选附带截图；视觉端点失败则回退纯文本）
pub async fn propose_step_repair(
    ctx: &StepRepairContext,
    screenshot_png: Option<&[u8]>,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<RepairPlan, String> {
    ensure_ark_key()?;
    let user_text = build_step_repair_user_message(ctx);
    let text_part = json!({ "type": "text", "text": user_text });

    let try_once = |model: &str, with_image: bool| {
        let content = if with_image {
            if let Some(png) = screenshot_png.filter(|p| !p.is_empty()) {
                json!([text_part.clone(), image_content_part(png)])
            } else {
                json!([{ "type": "text", "text": user_text.clone() }])
            }
        } else {
            json!(user_text.clone())
        };
        json!({
            "model": model,
            "messages": [
                { "role": "system", "content": SYSTEM_PROMPT_STEP_REPAIR },
                { "role": "user", "content": content }
            ],
            "temperature": 0.15,
            "max_tokens": 800
        })
    };

    let has_image = screenshot_png.map(|p| !p.is_empty()).unwrap_or(false);
    let mut last_err = String::new();
    let attempts: Vec<(&str, bool)> = if has_image {
        vec![
            (ARK_ENDPOINT_ID, true),
            (ARK_ENDPOINT_ID, false),
        ]
    } else {
        vec![(ARK_ENDPOINT_ID, false)]
    };

    for (model, with_image) in attempts {
        if cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst)) {
            return Err("已取消".into());
        }
        let body = try_once(model, with_image);
        match chat_completion_once(body, cancel.clone()).await {
            Ok(once) => {
                let text = once.content.trim().to_string();
                if text.is_empty() {
                    last_err = "方舟 API 返回内容为空".into();
                    continue;
                }
                return parse_repair_plan(&text);
            }
            Err(e) => {
                last_err = e;
                if with_image && is_vision_unsupported_error(&last_err) {
                    continue;
                }
                if last_err.contains("已取消") {
                    return Err(last_err);
                }
                if !with_image {
                    return Err(last_err);
                }
            }
        }
    }
    Err(last_err)
}

/// 失败分析返回：诊断正文 + Token 用量
#[derive(Debug, Clone)]
pub struct FailureAnalysisResult {
    pub content: String,
    pub usage: Option<TokenUsage>,
    pub reasoning: String,
}

/// 用户可读的 Token 消耗标记，如 `Token 消耗：输入 1200 / 输出 80 / 合计 1280`
pub fn format_token_usage(usage: &TokenUsage) -> String {
    format!(
        "Token 消耗：输入 {} / 输出 {} / 合计 {}",
        usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
    )
}

fn ensure_ark_key() -> Result<(), String> {
    if ark_api_key().trim().is_empty() {
        return Err("请先设置环境变量 WEBCLAW_ARK_API_KEY".into());
    }
    Ok(())
}

/// 供脚本工作台等模块复用
pub fn ensure_ark_configured() -> Result<(), String> {
    ensure_ark_key()
}

pub fn has_api_key() -> bool {
    !ark_api_key().trim().is_empty()
}

pub fn api_key() -> String {
    ark_api_key()
}

pub fn base_url() -> &'static str {
    ARK_BASE_URL
}

pub fn model_id() -> &'static str {
    ARK_ENDPOINT_ID
}

pub fn reasoning_effort() -> &'static str {
    ARK_REASONING_EFFORT
}

pub fn endpoint_id() -> &'static str {
    ARK_ENDPOINT_ID
}

async fn wait_until_cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::SeqCst) {
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }
}

async fn chat_completion_once(
    body: Value,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<ChatCompletionOnce, String> {
    if cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst)) {
        return Err("已取消".into());
    }

    let body = apply_thinking_defaults(body);
    let url = format!("{ARK_BASE_URL}/chat/completions");
    let send_fut = reqwest::Client::new()
        .post(&url)
        .bearer_auth(ark_api_key())
        .json(&body)
        .send();

    let resp = if let Some(ref cancel) = cancel {
        tokio::select! {
            biased;
            _ = wait_until_cancelled(cancel) => {
                return Err("已取消".into());
            }
            result = send_fut => {
                result.map_err(|e| format!("方舟 API 请求失败: {e}"))?
            }
        }
    } else {
        send_fut
            .await
            .map_err(|e| format!("方舟 API 请求失败: {e}"))?
    };

    if cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst)) {
        return Err("已取消".into());
    }

    let status = resp.status();
    let json_fut = resp.json::<Value>();
    let v = if let Some(ref cancel) = cancel {
        tokio::select! {
            biased;
            _ = wait_until_cancelled(cancel) => {
                return Err("已取消".into());
            }
            result = json_fut => {
                result.map_err(|e| e.to_string())?
            }
        }
    } else {
        json_fut.await.map_err(|e| e.to_string())?
    };

    if !status.is_success() {
        return Err(format!("方舟 API 错误 ({status}): {v}"));
    }

    let content = v["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("方舟 API 返回内容为空")?
        .trim()
        .to_string();
    Ok(ChatCompletionOnce {
        content,
        usage: parse_token_usage(&v),
    })
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiStreamChunkPayload {
    pub stream_id: String,
    pub delta: String,
    /// `content` | `reasoning`
    pub kind: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiStreamDonePayload {
    pub stream_id: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    pub aborted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Default)]
pub struct StreamChatResult {
    pub content: String,
    pub reasoning: String,
    pub usage: Option<TokenUsage>,
}

pub async fn stream_chat_completion<R: Runtime>(
    app: &AppHandle<R>,
    stream_id: &str,
    body: Value,
    cancel: Arc<AtomicBool>,
) -> Result<StreamChatResult, String> {
    ensure_ark_key()?;
    let mut body = apply_thinking_defaults(body);
    if let Some(obj) = body.as_object_mut() {
        obj.insert("stream".into(), json!(true));
    }

    let url = format!("{ARK_BASE_URL}/chat/completions");
    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(ark_api_key())
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("方舟 API 请求失败: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(format!("方舟 API 错误 ({status}): {err}"));
    }

    let mut content = String::new();
    let mut reasoning = String::new();
    let mut usage: Option<TokenUsage> = None;
    let mut buf = String::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let bytes = chunk.map_err(|e| format!("读取流失败: {e}"))?;
        buf.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(pos) = buf.find('\n') {
            let mut line: String = buf.drain(..pos).collect();
            if buf.starts_with('\n') {
                buf.drain(..1);
            }
            line = line.trim().to_string();
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if !line.starts_with("data:") {
                continue;
            }
            let data = line.trim_start_matches("data:").trim();
            if data == "[DONE]" {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            if let Some(u) = parse_token_usage(&v) {
                usage = Some(u);
            }

            let delta_obj = &v["choices"][0]["delta"];
            let reasoning_delta = delta_obj["reasoning_content"]
                .as_str()
                .or_else(|| delta_obj["reasoning"].as_str())
                .unwrap_or("");
            if !reasoning_delta.is_empty() {
                reasoning.push_str(reasoning_delta);
                let _ = app.emit(
                    "ai-stream-chunk",
                    AiStreamChunkPayload {
                        stream_id: stream_id.to_string(),
                        delta: reasoning_delta.to_string(),
                        kind: "reasoning".into(),
                    },
                );
            }

            let content_delta = delta_obj["content"]
                .as_str()
                .or_else(|| v["choices"][0]["message"]["content"].as_str())
                .unwrap_or("");
            if !content_delta.is_empty() {
                content.push_str(content_delta);
                let _ = app.emit(
                    "ai-stream-chunk",
                    AiStreamChunkPayload {
                        stream_id: stream_id.to_string(),
                        delta: content_delta.to_string(),
                        kind: "content".into(),
                    },
                );
            }
        }
    }

    Ok(StreamChatResult {
        content,
        reasoning,
        usage,
    })
}

/// 流式失败分析（思考过程通过 ai-stream-chunk 推送）
pub async fn analyze_step_failure_stream<R: Runtime>(
    app: &AppHandle<R>,
    stream_id: &str,
    ctx: &FailureAnalysisContext,
    cancel: Arc<AtomicBool>,
) -> Result<FailureAnalysisResult, String> {
    ensure_ark_key()?;
    let body = json!({
        "model": ARK_ENDPOINT_ID,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT_FAILURE_ANALYSIS },
            { "role": "user", "content": build_failure_analysis_user_message(ctx) }
        ],
        "temperature": 0.2,
        "max_tokens": 1024
    });
    let streamed = stream_chat_completion(app, stream_id, body, cancel).await?;
    let text = streamed.content.trim().to_string();
    if text.is_empty() {
        return Err("方舟 API 返回内容为空".into());
    }
    let reasoning = streamed.reasoning;
    let _ = app.emit(
        "ai-stream-done",
        AiStreamDonePayload {
            stream_id: stream_id.to_string(),
            content: text.clone(),
            reasoning: Some(reasoning.clone()).filter(|s| !s.trim().is_empty()),
            aborted: false,
            error: None,
            usage: streamed.usage.clone(),
        },
    );
    Ok(FailureAnalysisResult {
        content: text,
        usage: streamed.usage,
        reasoning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser_context::BrowserTabSnapshot;

    fn bilibili_snap() -> BrowserTabSnapshot {
        BrowserTabSnapshot {
            port: 9222,
            target_id: "t1".into(),
            title: "哔哩哔哩".into(),
            url: "https://www.bilibili.com/".into(),
            hostname: "www.bilibili.com".into(),
            site_hint: Some("B站".into()),
            page_signal: None,
        }
    }

    #[test]
    fn user_message_includes_browser_and_transcript() {
        let ctx = RecipeAiContext {
            browser: Some(bilibili_snap()),
            chat_transcript: Some("用户: 打开b站\n助手: 已转为快捷指令：打开https://www.bilibili.com".into()),
        };
        let (_, user_msg) =
            build_recipe_user_message("搜索最新vr游戏", None, Some(&ctx)).unwrap();
        assert!(user_msg.contains("--- 当前浏览器 ---"));
        assert!(user_msg.contains("bilibili.com"));
        assert!(user_msg.contains("--- 对话上下文"));
        assert!(user_msg.contains("打开b站"));
        assert!(user_msg.contains("--- 当前需求 ---"));
        assert!(user_msg.contains("搜索最新vr游戏"));
    }

    #[test]
    fn user_message_omits_blank_browser() {
        let ctx = RecipeAiContext {
            browser: Some(BrowserTabSnapshot {
                port: 9222,
                target_id: "t".into(),
                title: String::new(),
                url: "about:blank".into(),
                hostname: String::new(),
                site_hint: None,
                page_signal: None,
            }),
            chat_transcript: None,
        };
        let (_, user_msg) = build_recipe_user_message("测试", None, Some(&ctx)).unwrap();
        assert!(!user_msg.contains("--- 当前浏览器 ---"));
    }

    #[test]
    fn parse_token_usage_from_ark_shape() {
        let v = serde_json::json!({
            "usage": {
                "prompt_tokens": 1200,
                "completion_tokens": 80,
                "total_tokens": 1280
            }
        });
        let u = parse_token_usage(&v).expect("usage");
        assert_eq!(u.prompt_tokens, 1200);
        assert_eq!(u.completion_tokens, 80);
        assert_eq!(u.total_tokens, 1280);
    }

    #[test]
    fn format_token_usage_line() {
        let u = TokenUsage {
            prompt_tokens: 1200,
            completion_tokens: 80,
            total_tokens: 1280,
        };
        assert_eq!(
            format_token_usage(&u),
            "Token 消耗：输入 1200 / 输出 80 / 合计 1280"
        );
    }

    #[test]
    fn skills_catalog_includes_cached_skills() {
        let unique = format!("__ai_skill_ctx_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
        crate::recipe::with_skills_cache_exclusive(|| {
            crate::recipe::replace_skills_cache(vec![crate::recipe::SkillCacheEntry {
                id: "t1".into(),
                name: unique.clone(),
                content: "打开https://example.com，点击登录".into(),
                status: Some(1),
            }])
            .expect("write cache");
            let text = skills_catalog_text().expect("catalog");
            assert!(text.contains(&format!("技能{unique}")));
            assert!(text.contains("打开https://example.com"));
            let _ = crate::recipe::replace_skills_cache(vec![]);
        });
    }

    #[test]
    fn failure_analysis_message_includes_context() {
        let ctx = FailureAnalysisContext {
            step_label: "点击提交".into(),
            error: "未找到可点击目标".into(),
            recovery_attempted: true,
            browser: Some(bilibili_snap()),
            recent_logs: vec!["步骤 1 成功".into(), "步骤 2 失败".into()],
            probe_summary: Some("点击关闭\n点击提交".into()),
            probe_dump_at_fail: Some(
                "探针快照 | 首次失败时 | 图标按钮 1\n── 图标按钮 ──\n  图标按钮#9  DIV  class=\"user-box-banner\""
                    .into(),
            ),
            probe_dump_after_recovery: Some(
                "探针快照 | 刷新重试后 | 图标按钮 1\n── 图标按钮 ──\n  图标按钮#9  DIV  class=\"input-icon icon-hcm-screening\""
                    .into(),
            ),
        };
        let msg = build_failure_analysis_user_message(&ctx);
        assert!(msg.contains("--- 失败步骤 ---"));
        assert!(msg.contains("点击提交"));
        assert!(msg.contains("未找到可点击目标"));
        assert!(msg.contains("已自动刷新页面并重试"));
        assert!(msg.contains("--- 当前浏览器 ---"));
        assert!(msg.contains("bilibili.com"));
        assert!(msg.contains("步骤 2 失败"));
        assert!(msg.contains("--- 页面探针"));
        assert!(msg.contains("点击关闭"));
        assert!(msg.contains("探针快照（首次失败时"));
        assert!(msg.contains("user-box-banner"));
        assert!(msg.contains("探针快照（刷新重试后）"));
        assert!(msg.contains("icon-hcm-screening"));
    }

    #[test]
    fn parse_repair_plan_from_fenced_json() {
        let raw = r#"```json
{"confidence":"high","need_human":false,"reason":"弹窗挡住查询","ops":[{"op":"insert_before","steps":["点击关闭"]},{"op":"retry_as","step":"点击查询"}]}
```"#;
        let plan = parse_repair_plan(raw).expect("plan");
        assert_eq!(plan.confidence, "high");
        assert!(!plan.need_human);
        assert!(plan.is_auto_applicable());
        assert_eq!(plan.ops.len(), 2);
        assert_eq!(plan.ops[0].op, RepairOpKind::InsertBefore);
        assert_eq!(plan.ops[0].steps, vec!["点击关闭".to_string()]);
        assert_eq!(plan.ops[1].op, RepairOpKind::RetryAs);
        assert_eq!(plan.ops[1].step.as_deref(), Some("点击查询"));
    }

    #[test]
    fn parse_repair_plan_ask_human() {
        let plan = parse_repair_plan(
            r#"{"confidence":"low","need_human":true,"reason":"验证码","ops":[{"op":"ask_human"}]}"#,
        )
        .unwrap();
        assert!(!plan.is_auto_applicable());
        assert_eq!(plan.ask_human_reason().as_deref(), Some("验证码"));
    }
}
