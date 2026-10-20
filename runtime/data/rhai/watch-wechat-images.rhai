/// 微信个人数据目录图片监控（轮询 + V2 .dat 解密）
/// 发现新增聊天图片 .dat 后解密并飞书推送（需已绑定飞书）。
///
/// 目标目录默认：E:\Documents\xwechat_files\Lonk1989_ebe2
/// 监控：msg/attach/**/Img/*.dat（忽略 *_t.dat 缩略图；优先 _h / 显示版）
/// 密钥：本机 MMKV kvcomm code + 账号名离线派生，缓存到 files/wechat-image-key.json
///
/// 用法：
///   脚本微信图片监控
///   脚本微信图片监控 once
///   脚本微信图片监控 reset
///   脚本微信图片监控 E:\其它路径 once
///
/// 首次只建基线不推送。每次启动都会把目录里已有图片记入基线（不补录历史），
/// 之后只推送启动后新出现的图片。wxgf 会用本机微信解码器转成 jpg/png 后再推送。
/// 点「停止」可中断。

fn mode_token(s) {
    let t = str_trim(to_str(s));
    if t == "once" || t == "reset" || t == "once-reset" || t == "reset-once" {
        return t;
    }
    ""
}

fn looks_like_path(s) {
    let t = str_trim(to_str(s));
    if t == "" || mode_token(t) != "" {
        return false;
    }
    if t.len() >= 2 && t.sub_string(1, 1) == ":" {
        return true;
    }
    t.sub_string(0, 1) == "/" || t.sub_string(0, 1) == "\\"
}

fn slash_path(s) {
    let root = str_trim(to_str(s));
    let fixed = "";
    let j = 0;
    while j < root.len() {
        let ch = root.sub_string(j, 1);
        if ch == "\\" {
            fixed += "/";
        } else {
            fixed += ch;
        }
        j += 1;
    }
    fixed
}

fn parse_cli() {
    let root = "E:/Documents/xwechat_files/Lonk1989_ebe2";
    let once = false;
    let reset = false;
    let i = 0;
    let n = arg_count();
    while i < n {
        let a = arg(i);
        let m = mode_token(a);
        if m == "once" {
            once = true;
        } else if m == "reset" {
            reset = true;
        } else if m == "once-reset" || m == "reset-once" {
            once = true;
            reset = true;
        } else if looks_like_path(a) {
            root = slash_path(a);
        } else if str_trim(a) != "" {
            print("忽略未知参数: " + a);
        }
        i += 1;
    }
    #{ root: root, once: once, reset: reset }
}

fn dat_quality(path) {
    // 3=高清 _h · 2=显示版 · 1=缩略 _t · 0=其它
    let name = basename(path);
    if name.contains("_t.dat") || str_ends_with(name, "_t.dat") {
        return 1;
    }
    if name.contains("_h.dat") || str_ends_with(name, "_h.dat") {
        return 3;
    }
    if str_ends_with(name, ".dat") {
        return 2;
    }
    0
}

fn content_key(path) {
    let name = basename(path);
    if str_ends_with(name, "_t.dat") {
        return name.sub_string(0, name.len() - 6);
    }
    if str_ends_with(name, "_h.dat") {
        return name.sub_string(0, name.len() - 6);
    }
    if str_ends_with(name, ".dat") {
        return name.sub_string(0, name.len() - 4);
    }
    name
}

fn should_skip_dat(path) {
    let p = to_str(path);
    if p.contains("head_image") || p.contains("Head_Image") {
        return true;
    }
    // walk_files 路径已统一为正斜杠
    if !p.contains("/Img/") {
        return true;
    }
    false
}

fn collect_dat_images(root) {
    let dir = root + "/msg/attach";
    let out = [];
    if !path_exists(dir) {
        return out;
    }
    try {
        let files = walk_files(dir, "(?i)\\.dat$");
        let i = 0;
        while i < files.len() {
            let p = to_str(files[i]);
            // 不处理 *_t.dat：缩略图 OCR 无意义；等显示版/_h
            if !should_skip_dat(p) && dat_quality(p) >= 2 {
                out.push(p);
            }
            i += 1;
        }
    } catch (e) {
        print("扫描失败 " + dir + ": " + e);
    }
    out
}

/// 同一内容多档时只保留最高质量
fn prefer_best_quality(files) {
    let best = #{};
    let i = 0;
    while i < files.len() {
        let p = to_str(files[i]);
        let k = content_key(p);
        let q = dat_quality(p);
        if !best.contains(k) {
            best[k] = #{ path: p, q: q };
        } else if q > best[k].q {
            best[k] = #{ path: p, q: q };
        }
        i += 1;
    }
    let out = [];
    let keys = best.keys();
    let j = 0;
    while j < keys.len() {
        out.push(best[to_str(keys[j])].path);
        j += 1;
    }
    out
}

fn load_seen(path) {
    let seen = #{};
    if !path_exists(path) {
        return seen;
    }
    try {
        let arr = json_parse(read_text_file(path));
        let i = 0;
        while i < arr.len() {
            let p = to_str(arr[i]);
            if p != "" {
                seen[p] = true;
            }
            i += 1;
        }
    } catch (e) {
        print("读取已见清单失败，将重建: " + e);
    }
    seen
}

fn save_seen(path, seen) {
    let arr = [];
    let keys = seen.keys();
    let i = 0;
    while i < keys.len() {
        arr.push(to_str(keys[i]));
        i += 1;
    }
    write_text_file(path, json_stringify(arr));
}

fn load_or_resolve_keys(root, key_path) {
    if path_exists(key_path) {
        try {
            let obj = json_parse(read_text_file(key_path));
            if to_str(obj["aes_key"]).len() == 16 && (obj.xor_checked == 1 || obj.xor_checked == true) {
                print("已加载密钥缓存 account=" + to_str(obj["account"]) + " code=" + to_str(obj["code"]) + " xor=" + to_str(obj["xor_key"]));
                return obj;
            }
            print("密钥缓存未校正异或，重新派生");
        } catch (e) {
            print("密钥缓存无效，重新派生: " + e);
        }
    }
    print("派生图片密钥…");
    let keys = wechat_resolve_image_key(root);
    write_text_file(key_path, json_stringify(keys));
    print("密钥已保存 account=" + to_str(keys["account"]) + " aes=" + to_str(keys["aes_key"]) + " xor=" + to_str(keys["xor_key"]));
    keys
}

fn wait_file_stable(path) {
    let last = -1;
    let n = 0;
    while n < 10 {
        if !path_exists(path) {
            return false;
        }
        let sz = 0;
        try {
            sz = file_size(path);
        } catch {
            return false;
        }
        if sz <= 32 {
            sleep_secs(1);
            n += 1;
            continue;
        }
        if sz == last {
            return true;
        }
        last = sz;
        sleep_secs(1);
        n += 1;
    }
    last > 32
}

fn push_decoded(path, keys) {
    let name = basename(path);
    try {
        let out = wechat_dat_decrypt(path, "files/wechat-inbox", keys);
        track_output(out);

        // 本地 OCR（内嵌 RapidOCR/ONNX）；失败不阻断推图
        let ocr_text = "";
        try {
            ocr_text = ocr_image(out);
            if str_trim(ocr_text) != "" {
                let txt_path = out + ".txt";
                write_text_file(txt_path, ocr_text);
                track_output(txt_path);
                print("OCR " + basename(out) + " → " + ocr_text.len() + " 字符");
                push_text_safe("微信图片文字\n文件: " + basename(out) + "\n────────\n" + ocr_text);
            } else {
                print("OCR 无文字: " + basename(out));
            }
        } catch (e) {
            print("OCR 失败（仍推送图片）: " + e);
        }

        feishu_push_attachment(out);
        print("已推送图片: " + basename(out) + " ← " + name);
        return true;
    } catch (e) {
        print("解密/推送失败 " + name + ": " + e);
        return false;
    }
}

fn push_text_safe(text) {
    try {
        feishu_push_message(text);
    } catch (e) {
        print("文本推送失败（继续）: " + e);
    }
}

// ─── main ───

let cli = parse_cli();
let root = cli.root;
let once = cli.once;
let do_reset = cli.reset;
let state_path = "files/wechat-image-seen.json";
let key_path = "files/wechat-image-key.json";

print("微信图片监控（.dat 解密 + 本地 OCR）");
print("根目录: " + root);
print("监控: msg/attach/**/Img/*.dat（忽略 *_t.dat，仅显示版/_h）");
print("OCR: 内嵌 RapidOCR（ONNX，模型缓存于用户目录 ocr-models）");
print("────────────────────────────────────────");

if !path_exists(root) {
    throw "监控目录不存在: " + root;
}

if do_reset {
    if path_exists(state_path) {
        write_text_file(state_path, "[]");
        print("已清空已见清单（仍将按启动时目录重建基线）");
    }
}

let keys = load_or_resolve_keys(root, key_path);
// 每次启动重建基线：当前已有全部标为已见，不推送、不补录；只监控之后新增
let seen = #{};
print("建立基线（不补录、不推送历史）…");
let baseline = prefer_best_quality(collect_dat_images(root));
let i = 0;
while i < baseline.len() {
    seen[to_str(baseline[i])] = true;
    i += 1;
}
save_seen(state_path, seen);
print("基线 " + baseline.len() + " 张 .dat，开始监控启动后新增");
push_text_safe(
    "微信图片监控已启动\n目录: " + root
        + "\n账号: " + to_str(keys["account"])
        + "\n基线: " + baseline.len() + " 张（仅监控此后新增）"
);

let round = 0;
let poll_secs = 3;

while true {
    round += 1;
    print("[" + now_hm() + "] 第 " + round + " 轮扫描…");

    let files = prefer_best_quality(collect_dat_images(root));
    let added = [];
    let i = 0;
    while i < files.len() {
        let p = to_str(files[i]);
        if !seen.contains(p) {
            added.push(p);
        }
        i += 1;
    }

    if added.len() == 0 {
        print("无新增");
    } else {
        print("发现新增 " + added.len() + " 张");
        let j = 0;
        while j < added.len() {
            let p = to_str(added[j]);
            if wait_file_stable(p) {
                let r = push_decoded(p, keys);
                if r == true {
                    seen[p] = true;
                    save_seen(state_path, seen);
                } else {
                    print("本轮失败，下轮重试: " + basename(p));
                }
            } else {
                print("文件未稳定，下轮再试: " + basename(p));
            }
            j += 1;
        }
    }

    if once {
        print("once 模式结束");
        break;
    }
    sleep_secs(poll_secs);
}
