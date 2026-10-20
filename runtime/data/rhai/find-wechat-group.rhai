/// 按「最后一次发图」的大致时间，找出微信群目录 ID。
/// 只扫该月的 msg/attach/<群>/YYYY-MM|YY-MM/Img/*.dat 的修改时间，不解密。
///
/// 用法：
///   脚本查找微信群目录 <微信账号根> <日期> <时间> [容差分钟]
/// 示例：
///   脚本查找微信群目录 E:\Documents\xwechat_files\账号 2026-09-21 15:40
///   脚本查找微信群目录 E:\Documents\xwechat_files\账号 2026-09-21 15:40 60
/// 默认容差 ±30 分钟。时间对不上时仍会列出最接近的群。

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

fn abs_i(n) {
    if n < 0 {
        return 0 - n;
    }
    n
}

fn fmt_delta(secs) {
    let s = abs_i(secs);
    let m = s / 60;
    let h = m / 60;
    if h > 0 {
        return h + "小时" + (m % 60) + "分";
    }
    if m > 0 {
        return m + "分" + (s % 60) + "秒";
    }
    s + "秒"
}

let usage = "用法：脚本查找微信群目录 <微信账号根> <日期> <时间> [容差分钟]\n示例：脚本查找微信群目录 E:\\Documents\\xwechat_files\\账号 2026-09-21 15:40 30";
let wx_root = slash_path(require_arg(0, usage));
let date_part = str_trim(require_arg(1, usage));
let time_part = "";
let tol_min = 30;
if date_part.contains(" ") || date_part.contains("T") {
    time_part = date_part;
    tol_min = parse_int(arg(2, "30"));
} else {
    time_part = date_part + " " + str_trim(require_arg(2, usage));
    tol_min = parse_int(arg(3, "30"));
}
if tol_min <= 0 {
    tol_min = 30;
}
let target = parse_local_epoch(time_part);
if target < 0 {
    throw "时间无效，请用 2026-09-21 15:40 或 2026-09-21 15:40:00\n" + usage;
}
if !path_exists(wx_root) {
    throw "微信账号根不存在: " + wx_root;
}
let attach = wx_root + "/msg/attach";
if !path_exists(attach) {
    throw "不是有效的微信账号根（缺少 msg/attach）: " + wx_root;
}

let ymd = time_part;
if ymd.len() >= 10 {
    ymd = ymd.sub_string(0, 10);
}
let month_full = "";
let month_short = "";
if ymd.len() >= 7 {
    month_full = ymd.sub_string(0, 7);
}
if ymd.len() >= 10 {
    month_short = ymd.sub_string(2, 2) + "-" + ymd.sub_string(5, 2);
}
let tol_secs = tol_min * 60;

print("账号根 " + wx_root);
print("目标时间 " + time_part + "  容差 ±" + tol_min + " 分钟");
print("只扫描月目录 " + month_full + " / " + month_short);

let groups = list_child_dirs(attach);
print("群目录 " + groups.len() + " 个，开始对 .dat 修改时间…");

let best = #{};
let scanned_groups = 0;
let scanned_files = 0;
let gi = 0;
while gi < groups.len() {
    let gdir = to_str(groups[gi]);
    let gid = basename(gdir);
    let img_dirs = [];
    if month_full != "" {
        img_dirs.push(gdir + "/" + month_full + "/Img");
    }
    if month_short != "" && month_short != month_full {
        img_dirs.push(gdir + "/" + month_short + "/Img");
    }
    let ii = 0;
    let group_hit = false;
    while ii < img_dirs.len() {
        let img = to_str(img_dirs[ii]);
        if !path_exists(img) {
            ii += 1;
            continue;
        }
        group_hit = true;
        let files = [];
        try {
            files = walk_files(img, "(?i)\\.dat$");
        } catch {
            files = [];
        }
        let fi = 0;
        while fi < files.len() {
            let p = to_str(files[fi]);
            let mt = file_mtime_epoch(p);
            scanned_files += 1;
            if mt > 0 {
                let delta = abs_i(mt - target);
                let take = false;
                if !best.contains(gid) {
                    take = true;
                } else if delta < best[gid].delta {
                    take = true;
                }
                if take {
                    best[gid] = #{
                        group: gid,
                        delta: delta,
                        path: p,
                        when: file_mtime_text(p),
                        img: img
                    };
                }
            }
            fi += 1;
        }
        ii += 1;
    }
    if group_hit {
        scanned_groups += 1;
    }
    gi += 1;
    if gi % 40 == 0 || gi == groups.len() {
        print("  已看 " + gi + "/" + groups.len() + " 个目录，图片文件 " + scanned_files);
    }
}

let keys = best.keys();
let ranked = [];
let i = 0;
while i < keys.len() {
    ranked.push(best[to_str(keys[i])]);
    i += 1;
}
let n = ranked.len();
let a = 0;
while a < n {
    let b = a + 1;
    while b < n {
        if ranked[b].delta < ranked[a].delta {
            let tmp = ranked[a];
            ranked[a] = ranked[b];
            ranked[b] = tmp;
        }
        b += 1;
    }
    a += 1;
}

print("════════════════════════════════════════");
print("有当月图片的群 " + scanned_groups + " 个，比对文件 " + scanned_files);
if ranked.len() == 0 {
    throw "该月没有任何 .dat。请确认账号根、以及图片是否在 " + month_full + " 目录。";
}

let in_window = 0;
let c = 0;
while c < ranked.len() {
    if ranked[c].delta <= tol_secs {
        in_window += 1;
    }
    c += 1;
}
let show = ranked.len();
if show > 8 {
    show = 8;
}
print("最接近的群（最多 8 个）：");
let i = 0;
while i < show {
    let row = ranked[i];
    let mark = "超出窗口";
    if row.delta <= tol_secs {
        mark = "命中";
    }
    print((i + 1) + ". [" + mark + "] " + row.group);
    print("    相差 " + fmt_delta(row.delta) + "  文件时间 " + row.when);
    print("    " + row.img);
    i += 1;
}
if in_window == 0 {
    print("窗口 ±" + tol_min + " 分钟内没有图片。上面第 1 条是最接近的，可把容差调大再跑。");
} else {
    print("窗口内 " + in_window + " 个群。把第 1 条的目录 ID 填进出表脚本即可。");
}
print("完成");
