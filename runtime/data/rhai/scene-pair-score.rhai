/// 调试：用本次运行的两张图片附件计算配对分数（画面分 + 地点加分 + 时间约束）
///
/// 用法：先「添加附件」恰好 2 张图，再运行：
///   脚本场景配对打分
/// 第 1 张（文件名排序靠前）= 整改前，第 2 张 = 整改后。
/// 附件不是 2 张时直接报错。
///
/// 阈值：有共同地点词时画面分≥46；否则画面分≥70。整改后时间须晚于整改前。

fn preview(text, n) {
    let t = str_trim(to_str(text));
    if t == "" {
        return "(无文字)";
    }
    if t.len() > n {
        return t.sub_string(0, n) + "…";
    }
    t
}

fn collect_image_attachments() {
    let out = [];
    let dirs = ["files", "downloads"];
    let pat = "(?i)\\.(jpg|jpeg|png|bmp|webp)$";
    let i = 0;
    while i < dirs.len() {
        try {
            let files = walk_files(dirs[i], pat);
            let j = 0;
            while j < files.len() {
                let p = to_str(files[j]);
                let seen = false;
                let k = 0;
                while k < out.len() {
                    if to_str(out[k]) == p {
                        seen = true;
                    }
                    k += 1;
                }
                if !seen && p != "" {
                    out.push(p);
                }
                j += 1;
            }
        } catch {
        }
        i += 1;
    }
    out
}

let images = collect_image_attachments();
if images.len() != 2 {
    throw "需要恰好 2 张图片附件（第1张整改前，第2张整改后），当前 " + images.len() + " 张";
}
let before_path = to_str(images[0]);
let after_path = to_str(images[1]);
print("整改前 " + basename(before_path));
print("整改后 " + basename(after_path));

print("════════════════════════════════════════");
print("OCR 整改前…");
let before_text = ocr_image(before_path);
let before_meta = ocr_parse_meta(before_text);
print("  时间=" + before_meta.time + "  地点=" + before_meta.location);
print("  文=" + preview(before_text, 120));

print("OCR 整改后…");
let after_text = ocr_image(after_path);
let after_meta = ocr_parse_meta(after_text);
print("  时间=" + after_meta.time + "  地点=" + after_meta.location);
print("  文=" + preview(after_text, 120));

let r = score_scene_pair(
    #{
        path: before_path,
        text: before_text,
        time: to_str(before_meta.time),
        datetime: to_str(before_meta.datetime)
    },
    #{
        path: after_path,
        text: after_text,
        time: to_str(after_meta.time),
        datetime: to_str(after_meta.datetime)
    }
);

print("════════════════════════════════════════");
print("画面分 visual = " + r.visual);
print("地点加分 bonus = " + r.bonus);
print("合计 score   = " + r.score + "  （visual+bonus，上限100）");
print("阈值 threshold = " + r.threshold + "  （" + r.rule + "）");
print("时间约束 time_ok = " + r.time_ok + "  （前 " + r.before_time + " → 后 " + r.after_time + "）");
print("签名可用 sig_ok = " + r.sig_ok);
print("是否会配对 accept = " + r.accept);
if r.accept {
    print("结论：会配对");
} else {
    print("结论：不会配对");
    if !r.sig_ok {
        print("  原因：图片签名失败（解码/尺寸）");
    }
    if !r.time_ok {
        print("  原因：整改后时间未晚于整改前");
    }
    if r.sig_ok && r.time_ok && r.visual < r.threshold {
        print("  原因：画面分 " + r.visual + " < 阈值 " + r.threshold);
    }
}
print("完成");
