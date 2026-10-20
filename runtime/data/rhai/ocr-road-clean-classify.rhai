/// 每日检查记录表 — 微信群聊按日索引 + OCR/配对出表
/// 模板：YYYY.MM.DD每日检查记录表.xlsx
///
/// 入参第二项为微信账号根目录（含 msg/attach）。每个片区对应两个群：
///   整改前群（如「上站所」）→ 只取整改前照片
///   整改后群（如「上站所回复群」）→ 只取整改后照片（忽略回复群里的对比原图）
/// 解密图与按月索引进机器级公共缓存 wechat-img-cache。
/// 历史日一次性出表；未来日拒绝；当日轮询至次日 0 点（或点停止）并覆盖刷新 Excel。
/// 整改后判定：Clean Pro 水印（clean pro / 拍摄人·班组织 / 阳泉市中环洁）；裸「中环洁」视为桶身字→整改前。
///
/// 用法：
///   脚本OCR验证道路清扫 <整改日期> <微信账号根> <片区名> <整改前群ID> <整改后群ID> [...]
/// 示例：
///   脚本OCR验证道路清扫 2026-09-14 E:\Documents\xwechat_files\账号 上站所 ab74…cc 回复群ID…

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

fn looks_like_path(s) {
    let t = str_trim(to_str(s));
    if t == "" {
        return false;
    }
    if t.len() >= 2 && t.sub_string(1, 1) == ":" {
        return true;
    }
    t.sub_string(0, 1) == "/" || t.sub_string(0, 1) == "\\"
}

fn looks_like_ymd(s) {
    let t = str_trim(to_str(s));
    if t.len() < 8 {
        return false;
    }
    let c0 = t.sub_string(0, 1);
    if c0 < "1" || c0 > "9" {
        return false;
    }
    t.contains("-") || t.contains(".") || t.contains("/")
}

fn normalize_ymd(s) {
    let t = str_trim(to_str(s));
    let out = "";
    let i = 0;
    while i < t.len() {
        let ch = t.sub_string(i, 1);
        if ch == "." || ch == "/" {
            out += "-";
        } else {
            out += ch;
        }
        i += 1;
    }
    if out.len() >= 10 {
        return out.sub_string(0, 10);
    }
    out
}

fn parse_run_args() {
    let usage = "用法：脚本OCR验证道路清扫 <整改日期> <微信账号根> <片区名> <整改前群ID> <整改后群ID> [<片区名> <整改前群ID> <整改后群ID> ...]";
    let n = arg_count();
    if n < 5 {
        throw usage;
    }
    let report_date = normalize_ymd(require_arg(0, usage));
    if !looks_like_ymd(report_date) {
        throw "整改日期无效，请用 YYYY-MM-DD 或 YYYY.MM.DD\n" + usage;
    }
    let wx_root = slash_path(require_arg(1, usage));
    if !looks_like_path(wx_root) {
        throw "微信账号根目录无效\n" + usage;
    }
    let stations = [];
    let i = 2;
    while i + 2 < n {
        let name = str_trim(arg(i));
        let before_folder = str_trim(arg(i + 1));
        let after_folder = str_trim(arg(i + 2));
        if name == "" || before_folder == "" || after_folder == "" {
            throw "片区名、整改前群ID、整改后群ID 均不能为空\n" + usage;
        }
        if looks_like_ymd(name) {
            throw "片区参数成组：名称 整改前群ID 整改后群ID，当前第 " + (i + 1) + " 个参数像日期: " + name + "\n" + usage;
        }
        stations.push(#{
            sheet: name,
            before_folder: before_folder,
            after_folder: after_folder
        });
        i += 3;
    }
    if i != n {
        throw "片区参数须成组出现（名称 + 整改前群ID + 整改后群ID）\n" + usage;
    }
    if stations.len() < 1 {
        throw "至少配置一个片区\n" + usage;
    }
    #{ report_date: report_date, wx_root: wx_root, stations: stations }
}

fn dat_quality(path) {
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

fn ymd_to_dot(ymd) {
    if ymd.len() < 10 {
        return ymd;
    }
    ymd.sub_string(0, 4) + "." + ymd.sub_string(5, 2) + "." + ymd.sub_string(8, 2)
}

fn ymd_to_month_full(ymd) {
    // 2026-09-14 → 2026-09
    if ymd.len() < 7 {
        return ymd;
    }
    ymd.sub_string(0, 7)
}

fn ymd_to_month_short(ymd) {
    // 2026-09-14 → 26-09
    if ymd.len() < 10 {
        return "";
    }
    ymd.sub_string(2, 2) + "-" + ymd.sub_string(5, 2)
}

fn resolve_month_img_dir(wx_root, group_id, report_date) {
    let base = slash_path(wx_root) + "/msg/attach/" + group_id;
    let candidates = [ymd_to_month_full(report_date), ymd_to_month_short(report_date)];
    let i = 0;
    while i < candidates.len() {
        let m = to_str(candidates[i]);
        if m == "" {
            i += 1;
            continue;
        }
        let img = base + "/" + m + "/Img";
        if path_exists(img) {
            return #{ img_dir: img, month_key: m, month_full: ymd_to_month_full(report_date) };
        }
        i += 1;
    }
    #{ img_dir: "", month_key: "", month_full: ymd_to_month_full(report_date) }
}

fn account_name(wx_root) {
    basename(slash_path(wx_root))
}

fn group_cache_dir(cache_root, account, group_id) {
    slash_path(cache_root) + "/" + account + "/" + group_id
}

fn index_path_for(cache_root, account, group_id, month_full) {
    group_cache_dir(cache_root, account, group_id) + "/index-" + month_full + ".json"
}

fn key_path_for(cache_root, account) {
    slash_path(cache_root) + "/" + account + "/wechat-image-key.json"
}

fn load_or_resolve_keys(wx_root, key_path) {
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
    let keys = wechat_resolve_image_key(wx_root);
    write_text_file(key_path, json_stringify(keys));
    print("密钥已保存 account=" + to_str(keys["account"]) + " aes=" + to_str(keys["aes_key"]) + " xor=" + to_str(keys["xor_key"]));
    keys
}

fn load_index(path) {
    if !path_exists(path) {
        return #{ month: "", items: #{} };
    }
    try {
        let obj = json_parse(read_text_file(path));
        if !obj.contains("items") {
            obj["items"] = #{};
        }
        return obj;
    } catch (e) {
        print("读取索引失败，将重建: " + e);
        return #{ month: "", items: #{} };
    }
}

fn save_index(path, index) {
    write_text_file(path, json_stringify(index));
}

fn collect_month_dats(img_dir) {
    let out = [];
    if img_dir == "" || !path_exists(img_dir) {
        return out;
    }
    try {
        let files = walk_files(img_dir, "(?i)\\.dat$");
        let i = 0;
        while i < files.len() {
            let p = to_str(files[i]);
            if dat_quality(p) >= 2 {
                out.push(p);
            }
            i += 1;
        }
    } catch (e) {
        print("扫描失败 " + img_dir + ": " + e);
    }
    prefer_best_quality(out)
}

/// 扫描月目录合并进索引。日期只来自 OCR 水印，不用文件 mtime。
fn refresh_index(index, img_dir, month_full) {
    index["month"] = month_full;
    if !index.contains("items") {
        index["items"] = #{};
    }
    let items = index["items"];
    let files = collect_month_dats(img_dir);
    let i = 0;
    while i < files.len() {
        let p = to_str(files[i]);
        let k = content_key(p);
        let q = dat_quality(p);
        let dat_size = 0;
        try {
            dat_size = file_size(p);
        } catch {
            dat_size = 0;
        }
        if !items.contains(k) {
            items[k] = #{
                dat: p,
                date: "",
                decoded: "",
                quality: q,
                dat_size: dat_size,
                ocr_done: 0
            };
        } else {
            let cur = items[k];
            let old_q = 0;
            try {
                old_q = cur.quality;
            } catch {
                old_q = 0;
            }
            let path_changed = to_str(cur.dat) != p;
            let quality_up = q > old_q;
            // 仅质量升级才作废解码/OCR；单纯 size 波动不刷缓存，避免二次运行整月重扫
            if quality_up {
                cur.dat = p;
                cur.quality = q;
                cur.dat_size = dat_size;
                cur.decoded = "";
                cur.transcode_skip = 0;
                cur.ocr_done = 0;
                cur.date = "";
                cur["ocr_date"] = "";
                cur.ocr_text = "";
                cur.ocr_hit = "";
                cur.ocr_time = "";
                cur.ocr_datetime = "";
                cur.ocr_location = "";
                cur.ocr_photographer = "";
                cur.ocr_after = 0;
                cur.problem_type = "";
                cur.problem_done = 0;
            } else {
                cur.dat = p;
                cur.quality = q;
                cur.dat_size = dat_size;
                if path_changed {
                    // 同内容换路径：只改 dat，保留 decoded/OCR
                }
            }
            items[k] = cur;
        }
        i += 1;
    }
    index["items"] = items;
    index
}

fn decoded_store_dir(group_cache) {
    slash_path(group_cache) + "/decoded"
}

fn decoded_content_key(path) {
    let name = basename(path);
    let dot = -1;
    let di = 0;
    while di < name.len() {
        if name.sub_string(di, 1) == "." {
            dot = di;
        }
        di += 1;
    }
    if dot > 0 {
        return name.sub_string(0, dot);
    }
    name
}

fn ent_ocr_ready(ent) {
    if ent == () {
        return false;
    }
    try {
        let done = ent.ocr_done;
        if (done == 1 || done == true) && str_trim(to_str(ent.ocr_text)) != "" {
            return true;
        }
    } catch {
        return false;
    }
    false
}

fn recover_decoded_path(group_cache, content_key) {
    let dir = decoded_store_dir(group_cache);
    if !path_exists(dir) {
        return "";
    }
    let exts = [".jpg", ".jpeg", ".png", ".bmp", ".webp"];
    let i = 0;
    while i < exts.len() {
        let p = dir + "/" + content_key + exts[i];
        if path_exists(p) {
            return p;
        }
        i += 1;
    }
    ""
}

/// 对本月全部 .dat 解密；已有解码图则跳过。必须用返回值回写 index（Rhai Map 按值传递）。
fn ensure_decoded_for_month(index, group_cache, keys, log_lines) {
    let items = index["items"];
    let keys_list = items.keys();
    let hit = 0;
    let fresh = 0;
    let pending = 0;
    let skipped_fail = 0;
    let i = 0;
    while i < keys_list.len() {
        let k = to_str(keys_list[i]);
        let ent = items[k];
        let decoded = to_str(ent.decoded);
        if decoded != "" && path_exists(decoded) {
            hit += 1;
            i += 1;
            continue;
        }
        let recovered = recover_decoded_path(group_cache, k);
        if recovered != "" {
            ent.decoded = recovered;
            ent.transcode_skip = 0;
            items[k] = ent;
            hit += 1;
            i += 1;
            continue;
        }
        // 微信 DLL 与 ffmpeg 都失败才跳过。旧字段 decrypt_fail 不再使用，以便新兜底能重试。
        let already_fail = false;
        try {
            already_fail = (ent.transcode_skip == 1 || ent.transcode_skip == true);
        } catch {
            already_fail = false;
        }
        if already_fail {
            skipped_fail += 1;
            i += 1;
            continue;
        }
        pending += 1;
        let dat = to_str(ent.dat);
        if dat == "" || !path_exists(dat) {
            i += 1;
            continue;
        }
        if !wait_file_stable(dat) {
            print("  文件未稳定，下轮再试: " + basename(dat));
            i += 1;
            continue;
        }
        let dest_dir = decoded_store_dir(group_cache);
        try {
            let out_path = wechat_dat_decrypt(dat, dest_dir, keys);
            ent.decoded = out_path;
            ent.transcode_skip = 0;
            items[k] = ent;
            fresh += 1;
            print("  解密[" + (hit + fresh) + "/" + keys_list.len() + "] " + basename(dat));
        } catch (e) {
            let retry_later = e.contains("未找到 ffmpeg");
            if retry_later {
                print("  暂未能转码 " + basename(dat) + "（" + e + "）");
            } else {
                ent.transcode_skip = 1;
                items[k] = ent;
                print("  跳过无法转码 " + basename(dat) + "（" + e + "）");
            }
            log_lines.push(k + "\tDECRYPT_FAIL\t" + e);
        }
        i += 1;
    }
    index["items"] = items;
    let msg = "  解密完成：缓存命中 " + hit + " · 新解密 " + fresh + " · 待稳定/缺失 " + (pending - fresh);
    if skipped_fail > 0 {
        msg = msg + " · 已跳过不可转码 " + skipped_fail;
    }
    print(msg);
    index
}

/// 对本月尚未 OCR 的条目做识别。必须用返回值回写 index。
fn ensure_ocr_for_month(index, group_cache, log_lines) {
    let items = index["items"];
    let keys_list = items.keys();
    let hit = 0;
    let fresh = 0;
    let fail = 0;
    let total = keys_list.len();
    let i = 0;
    while i < keys_list.len() {
        let k = to_str(keys_list[i]);
        let ent = items[k];
        if ent_ocr_ready(ent) {
            // 用最新规则按已有 OCR 文重判整改前/后（不必重跑 OCR）
            ent = apply_ocr_after_flags(ent);
            items[k] = ent;
            hit += 1;
            i += 1;
            continue;
        }
        let decoded = to_str(ent.decoded);
        if decoded == "" || !path_exists(decoded) {
            let recovered = recover_decoded_path(group_cache, k);
            if recovered != "" {
                ent.decoded = recovered;
                decoded = recovered;
            }
        }
        if decoded == "" || !path_exists(decoded) {
            i += 1;
            continue;
        }
        print("  OCR[" + (hit + fresh + fail + 1) + "/" + total + "] " + basename(decoded));
        let ocr_text = "";
        try {
            ocr_text = ocr_image(decoded);
        } catch (e) {
            print("    失败: " + e);
            log_lines.push(k + "\tOCR_FAIL\t" + e);
            fail += 1;
            i += 1;
            continue;
        }
        let meta = ocr_parse_meta(ocr_text);
        let watermark_date = normalize_ymd(to_str(meta.date));
        let cls = classify_ocr(ocr_text);
        ent.ocr_done = 1;
        ent.ocr_text = ocr_text;
        ent.ocr_hit = cls.hit;
        let after_flag = 0;
        if cls.after {
            after_flag = 1;
        }
        ent.ocr_after = after_flag;
        ent.ocr_time = to_str(meta.time);
        ent.ocr_datetime = to_str(meta.datetime);
        ent.ocr_location = to_str(meta.location);
        ent.ocr_photographer = to_str(meta.photographer);
        if looks_like_ymd(watermark_date) {
            ent.date = watermark_date;
            ent["ocr_date"] = watermark_date;
            let kind = "整改前";
            if cls.after {
                kind = "整改后";
            }
            print("    → 水印日 " + watermark_date + "  " + kind + "  " + preview_text(ocr_text));
        } else {
            ent.date = "";
            ent["ocr_date"] = "";
            print("    → 无水印日期  " + preview_text(ocr_text));
        }
        items[k] = ent;
        fresh += 1;
        i += 1;
    }
    index["items"] = items;
    print("  OCR 完成：缓存命中 " + hit + " · 新识别 " + fresh + " · 失败 " + fail);
    index
}

/// 仅取水印日期 == report_date 且已解密的图片
fn collect_files_for_report_date(index, report_date) {
    let out = [];
    if !index.contains("items") {
        return out;
    }
    let items = index["items"];
    let keys_list = items.keys();
    let i = 0;
    while i < keys_list.len() {
        let k = to_str(keys_list[i]);
        let ent = items[k];
        let d = to_str(ent.date);
        if to_str(ent["ocr_date"]) != "" {
            d = to_str(ent["ocr_date"]);
        }
        if d != report_date {
            i += 1;
            continue;
        }
        let decoded = to_str(ent.decoded);
        if decoded == "" || !path_exists(decoded) {
            // 不在此处猜 group_cache；由 ensure_decoded 负责找回
            i += 1;
            continue;
        }
        out.push(decoded);
        i += 1;
    }
    out
}

fn classify_ocr(text) {
    // 整改后水印常见两类：
    //   A) Clean Pro App：拍摄人 / 班组织
    //   B) 今日水印定制模板：中环洁 + CLEAN PRO logo，落款「阳泉市中环洁」
    // 桶身广告往往只有孤立的「中环洁」，没有公司落款 / CLEAN PRO / 拍摄人。
    let raw = to_str(text);
    let lower = str_to_lower(raw);
    let compact = "";
    let i = 0;
    while i < lower.len() {
        let ch = lower.sub_string(i, 1);
        if ch != " " && ch != "\t" && ch != "\n" && ch != "\r" {
            compact += ch;
        }
        i += 1;
    }
    if compact.contains("cleanpro") || lower.contains("clean pro") {
        return #{ after: true, hit: "clean pro", text: raw };
    }
    // OCR 常把 CLEAN PRO 拆开或漏字：有 CLEAN 且有中环洁，也视为品牌水印
    if raw.contains("中环洁") && (compact.contains("clean") || lower.contains("clean")) {
        return #{ after: true, hit: "中环洁+clean", text: raw };
    }
    if raw.contains("拍摄人") || raw.contains("班组织") {
        let hit = "拍摄人";
        if raw.contains("中环洁") {
            hit = "中环洁+拍摄人";
        }
        return #{ after: true, hit: hit, text: raw };
    }
    // 今日水印模板落款 / 公司全称（桶身字一般不会写成这些）
    if raw.contains("阳泉市中环洁")
        || raw.contains("阳泉中环洁")
        || raw.contains("中环洁城市环境服务")
    {
        return #{ after: true, hit: "中环洁公司水印", text: raw };
    }
    // 裸「中环洁」→ 桶身字样，算整改前
    #{ after: false, hit: "", text: raw }
}

fn apply_ocr_after_flags(ent) {
    let cls = classify_ocr(to_str(ent.ocr_text));
    let after_flag = 0;
    if cls.after {
        after_flag = 1;
    }
    ent.ocr_after = after_flag;
    ent.ocr_hit = cls.hit;
    ent
}

fn preview_text(text) {
    let preview = str_trim(to_str(text));
    if preview == "" {
        return "(无文字)";
    }
    if preview.len() > 50 {
        return preview.sub_string(0, 50) + "…";
    }
    preview
}

fn prepare_sheet(wb, sheet) {
    try {
        ws_clear_images(wb, sheet);
    } catch (e) {
        print("清空图片跳过「" + sheet + "」: " + e);
    }
    try {
        ws_clear_range(wb, sheet, 5, 200, 1, 10);
    } catch (e) {
        print("清空单元格跳过「" + sheet + "」: " + e);
    }
}

fn ensure_station_sheets(wb, stations) {
    let names = wb_sheet_names(wb);
    let base = "";
    let i = 0;
    while i < names.len() {
        let n = to_str(names[i]);
        if n != "填表说明" {
            base = n;
            break;
        }
        i += 1;
    }
    if base == "" {
        throw "模板中未找到业务工作表";
    }
    let j = 0;
    while j < stations.len() {
        let name = to_str(stations[j].sheet);
        let exists = false;
        let k = 0;
        while k < names.len() {
            if to_str(names[k]) == name {
                exists = true;
                break;
            }
            k += 1;
        }
        if !exists {
            print("复制工作表 " + base + " → " + name);
            wb_copy_sheet(wb, base, name);
            names = wb_sheet_names(wb);
        }
        prepare_sheet(wb, name);
        j += 1;
    }
}

fn fill_header_date(wb, sheet, ymd) {
    let norm = normalize_ymd(ymd);
    if str_trim(norm) == "" || norm.len() < 10 {
        return;
    }
    let y = norm.sub_string(0, 4);
    let m = norm.sub_string(5, 2);
    let d = norm.sub_string(8, 2);
    ws_set_cell(wb, sheet, 2, 1, "填表单位：阳泉中环洁城市环境服务有限公司                           " + y + "年" + m + "月" + d + "日");
}

/// OCR 分类；优先读索引缓存，结果写回（含水印日期）
fn ocr_classify_images(files, report_date, index, log_lines) {
    let befores = [];
    let afters = [];
    let items = #{};
    if index != () && index.contains("items") {
        items = index["items"];
    }
    let cache_hit = 0;
    let ocr_fresh = 0;
    let i = 0;
    while i < files.len() {
        let p = to_str(files[i]);
        let name = basename(p);
        let ck = decoded_content_key(p);
        let ent = ();
        if ck != "" && items.contains(ck) {
            ent = items[ck];
        }

        let ocr_text = "";
        let meta_date = "";
        let meta_time = "";
        let meta_datetime = "";
        let meta_location = "";
        let meta_photographer = "";
        let cls_hit = "";
        let cls_after = false;
        let problem_type = "";
        let from_cache = false;

        if ent != () && ent_ocr_ready(ent) {
            ocr_text = to_str(ent.ocr_text);
            meta_date = to_str(ent.date);
            if to_str(ent["ocr_date"]) != "" {
                meta_date = to_str(ent["ocr_date"]);
            }
            meta_time = to_str(ent.ocr_time);
            meta_datetime = to_str(ent.ocr_datetime);
            meta_location = to_str(ent.ocr_location);
            meta_photographer = to_str(ent.ocr_photographer);
            // 缓存命中也按最新规则重判，避免桶身「中环洁」旧标记残留
            ent = apply_ocr_after_flags(ent);
            items[ck] = ent;
            cls_hit = to_str(ent.ocr_hit);
            cls_after = (ent.ocr_after == 1 || ent.ocr_after == true);
            problem_type = to_str(ent.problem_type);
            from_cache = true;
            cache_hit += 1;
            print("  OCR缓存 " + name + "  日=" + meta_date);
        } else {
            print("  OCR " + name);
            try {
                ocr_text = ocr_image(p);
            } catch (e) {
                print("    失败: " + e);
                log_lines.push(name + "\tFAIL\t" + e);
                i += 1;
                continue;
            }
            let meta = ocr_parse_meta(ocr_text);
            meta_date = normalize_ymd(to_str(meta.date));
            meta_time = to_str(meta.time);
            meta_datetime = to_str(meta.datetime);
            meta_location = to_str(meta.location);
            meta_photographer = to_str(meta.photographer);
            let cls = classify_ocr(ocr_text);
            cls_hit = cls.hit;
            cls_after = cls.after;
            ocr_fresh += 1;

            if ck != "" && items.contains(ck) {
                let e2 = items[ck];
                e2.ocr_done = 1;
                e2.ocr_text = ocr_text;
                e2.ocr_hit = cls_hit;
                let after_flag = 0;
                if cls_after {
                    after_flag = 1;
                }
                e2.ocr_after = after_flag;
                e2.ocr_time = meta_time;
                e2.ocr_datetime = meta_datetime;
                e2.ocr_location = meta_location;
                e2.ocr_photographer = meta_photographer;
                if looks_like_ymd(meta_date) {
                    e2.date = meta_date;
                    e2["ocr_date"] = meta_date;
                }
                // 解码路径校正到当前文件
                e2.decoded = p;
                items[ck] = e2;
                ent = e2;
            }
        }

        let watermark_date = normalize_ymd(meta_date);
        if looks_like_ymd(watermark_date) && watermark_date != report_date {
            print("    跳过（水印日 " + watermark_date + " ≠ " + report_date + "）");
            i += 1;
            continue;
        }

        let item = #{
            path: p,
            text: ocr_text,
            hit: cls_hit,
            date: meta_date,
            time: meta_time,
            datetime: meta_datetime,
            location: meta_location,
            photographer: meta_photographer,
            problem_type: problem_type,
            content_key: ck
        };
        if cls_after {
            if !from_cache {
                print("    → 整改后 " + cls_hit + "  " + preview_text(ocr_text));
            }
            afters.push(item);
        } else {
            if !from_cache {
                print("    → 整改前  " + preview_text(ocr_text));
            }
            befores.push(item);
        }
        i += 1;
    }
    if index != () {
        index["items"] = items;
    }
    if cache_hit > 0 || ocr_fresh > 0 {
        print("  OCR 缓存命中 " + cache_hit + " · 新识别 " + ocr_fresh);
    }
    #{ befores: befores, afters: afters, index: index }
}

fn find_item_by_path(arr, path) {
    let i = 0;
    while i < arr.len() {
        if to_str(arr[i].path) == path {
            return arr[i];
        }
        i += 1;
    }
    ()
}

fn item_discover_key(item) {
    if item == () {
        return "99:99:99";
    }
    let t = to_str(item.time);
    if t == "" || t == "()" {
        return "99:99:99";
    }
    t
}

fn sort_items_by_discover(arr) {
    let n = arr.len();
    let i = 0;
    while i < n {
        let j = i + 1;
        while j < n {
            if item_discover_key(arr[j]) < item_discover_key(arr[i]) {
                let tmp = arr[i];
                arr[i] = arr[j];
                arr[j] = tmp;
            }
            j += 1;
        }
        i += 1;
    }
    arr
}

fn sort_pairs_by_discover(pairs, befores) {
    let n = pairs.len();
    let i = 0;
    while i < n {
        let j = i + 1;
        while j < n {
            let bi = find_item_by_path(befores, to_str(pairs[i].before));
            let bj = find_item_by_path(befores, to_str(pairs[j].before));
            if item_discover_key(bj) < item_discover_key(bi) {
                let tmp = pairs[i];
                pairs[i] = pairs[j];
                pairs[j] = tmp;
            }
            j += 1;
        }
        i += 1;
    }
    pairs
}

fn classify_problem_for_pairs(pairs, befores, index) {
    let items = #{};
    if index != () && index.contains("items") {
        items = index["items"];
    }
    let i = 0;
    while i < pairs.len() {
        let bp = to_str(pairs[i].before);
        let j = 0;
        while j < befores.len() {
            if to_str(befores[j].path) == bp {
                let existing = to_str(befores[j].problem_type);
                if existing != "" && existing != "()" {
                    print("  问题点位缓存 " + basename(bp) + " → " + existing);
                    break;
                }
                let ck = to_str(befores[j].content_key);
                if ck == "" {
                    ck = decoded_content_key(bp);
                }
                if ck != "" && items.contains(ck) {
                    let ent = items[ck];
                    let cached_pt = to_str(ent.problem_type);
                    let done = false;
                    try {
                        done = (ent.problem_done == true || ent.problem_done == 1);
                    } catch {
                        done = false;
                    }
                    if done && cached_pt != "" && cached_pt != "()" {
                        befores[j].problem_type = cached_pt;
                        print("  问题点位缓存 " + basename(bp) + " → " + cached_pt);
                        break;
                    }
                }
                try {
                    let vis = classify_sanitation_image(bp, to_str(befores[j].text));
                    let pt = to_str(vis.problem_type);
                    if pt != "" && pt != "()" {
                        befores[j].problem_type = pt;
                        print("  问题点位 " + basename(bp) + " → " + pt + "  conf=" + vis.confidence);
                        if ck != "" && items.contains(ck) {
                            let ent = items[ck];
                            ent.problem_type = pt;
                            ent.problem_done = 1;
                            items[ck] = ent;
                        }
                    }
                } catch (e) {
                    print("  问题点位跳过 " + basename(bp) + ": " + e);
                }
                break;
            }
            j += 1;
        }
        i += 1;
    }
    if index != () {
        index["items"] = items;
    }
    index
}

fn write_row(wb, sheet, row, seq, station, before_item, after_item) {
    ws_set_row_height(wb, sheet, row, 40.0);
    ws_set_cell(wb, sheet, row, 1, seq);
    ws_set_cell(wb, sheet, row, 2, station);

    let problem = "";
    let admin = "";
    let discover = "";
    let fix_time = "";
    let discover_dt = "";
    let fix_dt = "";
    let bp = "";
    let ap = "";

    if before_item != () {
        bp = to_str(before_item.path);
        discover = to_str(before_item.time);
        discover_dt = to_str(before_item.datetime);
        if discover_dt == "" {
            discover_dt = discover;
        }
        problem = to_str(before_item.problem_type);
    }
    if after_item != () {
        ap = to_str(after_item.path);
        fix_time = to_str(after_item.time);
        fix_dt = to_str(after_item.datetime);
        if fix_dt == "" {
            fix_dt = fix_time;
        }
        admin = to_str(after_item.photographer);
        if problem == "" || problem == "()" {
            problem = to_str(after_item.problem_type);
        }
    }
    // 落单（未找到巡检照片 / 不在保洁区域）不写问题点位
    if bp == "" || ap == "" {
        problem = "";
    } else if problem == "" || problem == "()" {
        problem = "路面散落垃圾";
    }

    if bp != "" {
        ws_set_cell(wb, sheet, row, 3, discover);
        ws_add_image(wb, sheet, row, 4, bp, 280);
    } else {
        ws_set_cell(wb, sheet, row, 4, "未找到巡检照片");
    }

    if ap != "" {
        ws_set_cell(wb, sheet, row, 5, fix_time);
        ws_add_image(wb, sheet, row, 6, ap, 280);
    } else {
        ws_set_cell(wb, sheet, row, 6, "不在保洁区域");
    }

    ws_set_cell(wb, sheet, row, 7, "");
    ws_set_cell(wb, sheet, row, 8, problem);
    ws_set_cell(wb, sheet, row, 9, admin);
    ws_set_cell(wb, sheet, row, 10, format_process_duration(discover_dt, fix_dt));
}

fn fill_station_from_classified(wb, sheet, station, befores, afters, index_before, index_after, log_lines, report_date) {
    print("════════════════════════════════════════");
    print("工作表「" + sheet + "」整改前 " + befores.len() + " · 整改后 " + afters.len());
    prepare_sheet(wb, sheet);
    fill_header_date(wb, sheet, report_date);
    if befores.len() == 0 && afters.len() == 0 {
        print("无当日可用图片，表头已写");
        return #{ before: index_before, after: index_after };
    }

    let paired = pair_scene_images(befores, afters);
    let pairs = paired.pairs;
    let unmatched_before = paired.unmatched_before;
    let unmatched_after = paired.unmatched_after;
    print("配对 " + pairs.len() + " · 落单前 " + unmatched_before.len() + " · 落单后 " + unmatched_after.len());

    index_before = classify_problem_for_pairs(pairs, befores, index_before);

    let ub_items = [];
    let i = 0;
    while i < unmatched_before.len() {
        ub_items.push(find_item_by_path(befores, to_str(unmatched_before[i].path)));
        i += 1;
    }
    ub_items = sort_items_by_discover(ub_items);

    let ua_items = [];
    let i = 0;
    while i < unmatched_after.len() {
        ua_items.push(find_item_by_path(afters, to_str(unmatched_after[i].path)));
        i += 1;
    }
    ua_items = sort_items_by_discover(ua_items);
    pairs = sort_pairs_by_discover(pairs, befores);

    let row = 5;
    let seq = 1;

    let i = 0;
    while i < ub_items.len() {
        let bi = ub_items[i];
        write_row(wb, sheet, row, seq, station, bi, ());
        print("  落单前 " + basename(to_str(bi.path)) + "  " + item_discover_key(bi));
        log_lines.push(sheet + "\t" + basename(to_str(bi.path)) + "\t整改前落单\t不在保洁区域");
        row += 1;
        seq += 1;
        i += 1;
    }

    let i = 0;
    while i < ua_items.len() {
        let ai = ua_items[i];
        write_row(wb, sheet, row, seq, station, (), ai);
        print("  落单后 " + basename(to_str(ai.path)));
        log_lines.push(sheet + "\t" + basename(to_str(ai.path)) + "\t整改后落单\t未找到巡检照片");
        row += 1;
        seq += 1;
        i += 1;
    }

    let i = 0;
    while i < pairs.len() {
        let item = pairs[i];
        let bp = to_str(item.before);
        let ap = to_str(item.after);
        let bi = find_item_by_path(befores, bp);
        let ai = find_item_by_path(afters, ap);
        write_row(wb, sheet, row, seq, station, bi, ai);
        print("  配对 " + basename(bp) + " ↔ " + basename(ap) + "  " + item_discover_key(bi) + "  score=" + item.score);
        log_lines.push(sheet + "\t" + basename(bp) + "\t↔\t" + basename(ap) + "\t" + item.score);
        row += 1;
        seq += 1;
        i += 1;
    }
    #{ before: index_before, after: index_after }
}

/// 对单个群：建/更新月索引、解密、OCR，返回当日图片列表与 index。
fn prepare_group_day(wx_root, group_id, report_date, cache_root, account, keys, log_lines, label) {
    let resolved = resolve_month_img_dir(wx_root, group_id, report_date);
    let img_dir = to_str(resolved.img_dir);
    let month_full = to_str(resolved.month_full);
    let gcache = group_cache_dir(cache_root, account, group_id);
    let ipath = index_path_for(cache_root, account, group_id, month_full);
    if img_dir == "" {
        print("  " + label + " 月目录不存在，跳过（群 " + group_id + " / " + month_full + "）");
        log_lines.push(label + "\tMISS_MONTH\t" + group_id);
        return #{
            ok: false,
            index: #{ month: month_full, items: #{} },
            ipath: ipath,
            files: []
        };
    }
    print("  " + label + " ← " + img_dir);
    let index = load_index(ipath);
    let had_items = 0;
    if index.contains("items") {
        let existing = index["items"];
        had_items = existing.keys().len();
    }
    if had_items > 0 {
        print("  " + label + " 加载月索引（已有 " + had_items + " 条）");
    } else {
        print("  " + label + " 建立月索引（首次解密+OCR，请稍候）…");
    }
    index = refresh_index(index, img_dir, month_full);
    index = ensure_decoded_for_month(index, gcache, keys, log_lines);
    save_index(ipath, index);
    index = ensure_ocr_for_month(index, gcache, log_lines);
    save_index(ipath, index);
    let files = collect_files_for_report_date(index, report_date);
    print("  " + label + " 报告日 " + report_date + " 命中 " + files.len() + " 张");
    #{ ok: true, index: index, ipath: ipath, files: files }
}

fn process_all_stations(wb, stations, wx_root, report_date, cache_root, account, keys, log_lines) {
    let i = 0;
    while i < stations.len() {
        let st = stations[i];
        let sheet = to_str(st.sheet);
        let before_id = to_str(st.before_folder);
        let after_id = to_str(st.after_folder);
        print("片区「" + sheet + "」整改前群 " + before_id + " · 整改后群 " + after_id);

        let before_prep = prepare_group_day(
            wx_root, before_id, report_date, cache_root, account, keys, log_lines, "整改前群"
        );
        let after_prep = prepare_group_day(
            wx_root, after_id, report_date, cache_root, account, keys, log_lines, "整改后群"
        );

        if !before_prep.ok && !after_prep.ok {
            prepare_sheet(wb, sheet);
            fill_header_date(wb, sheet, report_date);
            i += 1;
            continue;
        }

        // 整改前群：只要整改前；回复群里的对比原图会落在 befores，此处丢弃
        let before_cls = ocr_classify_images(before_prep.files, report_date, before_prep.index, log_lines);
        let befores = before_cls.befores;
        if before_cls.afters.len() > 0 {
            print("  整改前群忽略非整改前 " + before_cls.afters.len() + " 张");
        }
        save_index(before_prep.ipath, before_cls.index);

        // 整改后群：只要整改后；对比原图（按规则判为整改前）丢弃
        let after_cls = ocr_classify_images(after_prep.files, report_date, after_prep.index, log_lines);
        let afters = after_cls.afters;
        if after_cls.befores.len() > 0 {
            print("  整改后群忽略对比原图/非整改后 " + after_cls.befores.len() + " 张");
        }
        save_index(after_prep.ipath, after_cls.index);

        let filled = fill_station_from_classified(
            wb, sheet, sheet, befores, afters, before_cls.index, after_cls.index, log_lines, report_date
        );
        save_index(before_prep.ipath, filled.before);
        save_index(after_prep.ipath, filled.after);
        i += 1;
    }
}

fn accumulate_group_progress(group_id, wx_root, report_date, cache_root, account, acc) {
    let month_full = ymd_to_month_full(report_date);
    let ipath = index_path_for(cache_root, account, group_id, month_full);
    let resolved = resolve_month_img_dir(wx_root, group_id, report_date);
    let img_dir = to_str(resolved.img_dir);
    let index = load_index(ipath);
    if img_dir != "" {
        index = refresh_index(index, img_dir, month_full);
    }
    if index.contains("items") {
        let items = index["items"];
        let ks = items.keys();
        let j = 0;
        while j < ks.len() {
            let ent = items[to_str(ks[j])];
            acc.month_total = acc.month_total + 1;
            if ent_ocr_ready(ent) {
                acc.ocr_done = acc.ocr_done + 1;
                let d = to_str(ent.date);
                if to_str(ent["ocr_date"]) != "" {
                    d = to_str(ent["ocr_date"]);
                }
                if d == report_date {
                    acc.day_entries = acc.day_entries + 1;
                    if to_str(ent.decoded) != "" && path_exists(to_str(ent.decoded)) {
                        acc.day_decoded = acc.day_decoded + 1;
                    }
                }
            }
            j += 1;
        }
    }
    acc
}

fn count_day_progress(stations, wx_root, report_date, cache_root, account) {
    let acc = #{
        month_total: 0,
        ocr_done: 0,
        day_entries: 0,
        day_decoded: 0
    };
    let i = 0;
    while i < stations.len() {
        acc = accumulate_group_progress(
            to_str(stations[i].before_folder), wx_root, report_date, cache_root, account, acc
        );
        acc = accumulate_group_progress(
            to_str(stations[i].after_folder), wx_root, report_date, cache_root, account, acc
        );
        i += 1;
    }
    #{
        entries: acc.day_entries,
        decoded: acc.day_decoded,
        month_total: acc.month_total,
        ocr_done: acc.ocr_done,
        sig: acc.month_total * 1000000 + acc.ocr_done * 1000 + acc.day_entries
    }
}

fn count_day_decoded(stations, wx_root, report_date, cache_root, account) {
    count_day_progress(stations, wx_root, report_date, cache_root, account).decoded
}

// ─── main ───

let cfg = parse_run_args();
let wx_root = cfg.wx_root;
let stations = cfg.stations;
let report_date = cfg.report_date;
let today = today_ymd();
let cmp = ymd_cmp(report_date, today);

print("每日检查记录表（微信按月 OCR 索引 + 按水印日出表）");
print("整改日期: " + report_date + "  今天: " + today);
print("说明: 不按文件时间过滤；先对本月全部图片建索引（解密+OCR），再取水印日匹配的图片。");
print("微信账号根: " + wx_root);
let si = 0;
while si < stations.len() {
    print(
        "片区" + (si + 1) + ": " + stations[si].sheet
            + " ← 前 " + stations[si].before_folder
            + " / 后 " + stations[si].after_folder
    );
    si += 1;
}
print("────────────────────────────────────────");

if cmp > 0 {
    throw "整改日期 " + report_date + " 是未来日期（今天 " + today + "），请检查是否写错。脚本未执行。";
}

if !path_exists(wx_root) {
    throw "微信账号根目录不存在: " + wx_root;
}
if !path_exists(wx_root + "/msg/attach") {
    throw "不是有效的微信账号根（缺少 msg/attach）: " + wx_root;
}

let live = cmp == 0;
if live {
    print("模式: 当日实时（轮询至次日 0 点，可点停止）");
} else {
    print("模式: 历史日一次性处理");
}

let cache_root = wechat_img_cache_dir();
let account = account_name(wx_root);
let key_path = key_path_for(cache_root, account);
let keys = load_or_resolve_keys(wx_root, key_path);
print("公共缓存: " + cache_root + "/" + account);

let tpl = find_template_optional("^YYYY\\.MM\\.DD每日检查记录表\\.xlsx$", "");
if tpl == "" {
    tpl = find_template_optional("每日检查记录表\\.xlsx$", "");
}
if tpl == "" {
    throw "未找到模板「YYYY.MM.DD每日检查记录表.xlsx」";
}

let out_name = ymd_to_dot(report_date) + "每日检查记录表.xlsx";
// 缓存内始终可写的工作簿；产物目录放副本供用户打开
let work_xlsx = slash_path(cache_root) + "/" + account + "/reports/" + out_name;
let out_xlsx = "files/" + out_name;
let log_path = "files/ocr-daily-check-log.txt";

copy_template(tpl, work_xlsx);
let wb = open_workbook(work_xlsx);
ensure_station_sheets(wb, stations);

let poll_secs = 3;
let round = 0;
let last_sig = -1;
let artifact_synced = false;

fn sync_artifact_copy(work_xlsx, out_xlsx) {
    if try_copy_file(work_xlsx, out_xlsx) {
        track_output(out_xlsx);
        return true;
    }
    false
}

while true {
    round += 1;
    let log_lines = [];
    print("[" + now_hm() + "] 第 " + round + " 轮处理…");

    // 实时模式：无新增则跳过重算，但仍尝试把缓存表同步到产物（用户可能刚关闭 Excel）
    if live && round > 1 {
        let prog = count_day_progress(stations, wx_root, report_date, cache_root, account);
        if prog.sig == last_sig {
            if !artifact_synced {
                if sync_artifact_copy(work_xlsx, out_xlsx) {
                    artifact_synced = true;
                    print("产物已同步（文件已可写）→ " + out_xlsx);
                }
            }
            print("无新增，跳过重算");
            if today_ymd() != report_date {
                print("已过当日 24:00，结束实时监控");
                break;
            }
            sleep_secs(poll_secs);
            continue;
        }
    }

    process_all_stations(wb, stations, wx_root, report_date, cache_root, account, keys, log_lines);

    wb_save(wb, work_xlsx);
    if sync_artifact_copy(work_xlsx, out_xlsx) {
        artifact_synced = true;
        print("完成写入缓存并同步产物 → " + out_xlsx);
    } else {
        artifact_synced = false;
        print("已写入缓存: " + work_xlsx);
        print("产物文件可能正被打开，关闭后将自动同步 → " + out_xlsx);
    }

    let log_body = "根目录\t" + wx_root + "\n报告日\t" + report_date + "\n轮次\t" + round + "\n缓存表\t" + work_xlsx + "\n────────\n";
    let j = 0;
    while j < log_lines.len() {
        log_body += log_lines[j] + "\n";
        j += 1;
    }
    write_text_file(log_path, log_body);
    track_output(log_path);

    let prog = count_day_progress(stations, wx_root, report_date, cache_root, account);
    last_sig = prog.sig;
    print("当日水印命中 " + prog.entries + " · 月内已OCR " + prog.ocr_done + "/" + prog.month_total);

    if !live {
        // 历史模式结束前再试一次同步
        if !artifact_synced {
            if sync_artifact_copy(work_xlsx, out_xlsx) {
                artifact_synced = true;
                print("产物已同步 → " + out_xlsx);
            } else {
                print("警告: 产物未能写出（文件可能被占用），请关闭后从缓存复制: " + work_xlsx);
            }
        }
        break;
    }

    if today_ymd() != report_date {
        print("已过当日 24:00，结束实时监控");
        break;
    }

    print("等待新增…（" + poll_secs + "s，点停止可中断）");
    sleep_secs(poll_secs);
}

print("────────────────────────────────────────");
print("缓存表 → " + work_xlsx);
if artifact_synced {
    print("产物 → " + out_xlsx);
} else {
    print("产物尚未同步（文件可能仍被占用）→ " + out_xlsx);
}
print("日志 → " + log_path);
