/// 网球场1/2 晚场可约监控（无限循环）
/// 仅工作日（周一至周五）20:00 / 21:00 / 22:00；含本日共 4 天窗口。
/// 每 5–10 分钟轮询一次；可约集合变化时飞书推送当前剩余可约。
/// 22:00–次日 06:00 静默（不推送、少打扰）。
///
/// 用法: 脚本网球场地
///       脚本网球场地 once   — 只跑一轮（调试）
///
/// 依赖：客户端已绑定飞书；visitor 令牌失效时请更新下方 PSPLVISITORID。

fn time_hm(dt) {
    let t = to_str(dt);
    if t.len() >= 16 {
        return t.sub_string(11, 5);
    }
    t
}

fn is_target_room(room) {
    if room.contains("匹克") || room.contains("儿童") {
        return false;
    }
    if !room.contains("网球") {
        return false;
    }
    room.contains("网球场1")
        || room.contains("网球场2")
        || room.contains("1号网球")
        || room.contains("2号网球")
        || room.contains("网球1")
        || room.contains("网球2")
}

fn is_target_begin(hm) {
    hm == "20:00" || hm == "21:00" || hm == "22:00"
}

fn is_quiet_hours() {
    let h = now_hour();
    h >= 22 || h < 6
}

fn quiet_sleep_secs() {
    // 睡到约 06:00（最多一次睡 30 分钟，便于日志可见）
    let h = now_hour();
    let m = now_minute();
    let mins_to_6;
    if h >= 22 {
        mins_to_6 = (24 - h - 1) * 60 + (60 - m) + 6 * 60;
    } else {
        mins_to_6 = (6 - h - 1) * 60 + (60 - m);
    }
    if mins_to_6 <= 0 {
        return 60;
    }
    let sec = mins_to_6 * 60;
    if sec > 1800 {
        return 1800;
    }
    sec
}

fn api_headers(store_id, visitor_id, version_info) {
    http_headers([
        "Content-Type", "application/json",
        "PSPLVISITORAUTO", "API",
        "VERSIONINFO", version_info,
        "STOREID", store_id,
        "APPTYPE", "1",
        "PSPLVISITORID", visitor_id,
        "Accept", "*/*"
    ])
}

fn fetch_day_bookable(date, api_url, user_id, project_uid, headers) {
    let body = "{\"dateTime\":\"" + date + "\",\"userId\":" + user_id + ",\"projectUid\":\"" + project_uid + "\"}";
    let raw = http_post(api_url, headers, body);
    let root = json_parse(raw);
    if !root["successed"] {
        throw "接口失败 " + date + ": status=" + to_str(root["status"]) + " errorCode=" + to_str(root["errorCode"]);
    }
    let slots = root["result"]["slots"];
    let out = [];
    if slots == () {
        return out;
    }
    let i = 0;
    while i < slots.len() {
        let slot = slots[i];
        let room = to_str(slot["classRoomName"]);
        let begin = time_hm(slot["beginDatetime"]);
        if is_target_room(room) && is_target_begin(begin) {
            let can = false;
            let appt = slot["apptInfo"];
            if appt != () {
                can = appt["canApptOrNot"];
                if can == () {
                    can = false;
                }
            }
            if can {
                out.push(#{
                    "date": date,
                    "room": room,
                    "begin": begin,
                    "end": time_hm(slot["endDatetime"]),
                    "key": date + "|" + room + "|" + begin
                });
            }
        }
        i += 1;
    }
    out
}

fn fingerprint(items) {
    let keys = [];
    let i = 0;
    while i < items.len() {
        keys.push(to_str(items[i]["key"]));
        i += 1;
    }
    // 简单插入排序，保证顺序稳定
    let a = 0;
    while a < keys.len() {
        let b = a + 1;
        while b < keys.len() {
            if keys[b] < keys[a] {
                let t = keys[a];
                keys[a] = keys[b];
                keys[b] = t;
            }
            b += 1;
        }
        a += 1;
    }
    let s = "";
    let k = 0;
    while k < keys.len() {
        if k > 0 {
            s += "\n";
        }
        s += keys[k];
        k += 1;
    }
    s
}

fn format_bookable(items) {
    if items.len() == 0 {
        return "当前无剩余可预约（网球场1/2 · 工作日 20/21/22 点）";
    }
    let by_date = #{};
    let dates = [];
    let i = 0;
    while i < items.len() {
        let it = items[i];
        let d = to_str(it["date"]);
        if !by_date.contains(d) {
            dates.push(d);
            by_date[d] = [];
        }
        let rows = by_date[d];
        rows.push(to_str(it["room"]) + "  " + to_str(it["begin"]) + "-" + to_str(it["end"]));
        by_date[d] = rows;
        i += 1;
    }
    let msg = "剩余可预约：\n";
    let di = 0;
    while di < dates.len() {
        let d = dates[di];
        msg += d + " " + ymd_weekday_cn(d) + "\n";
        let rows = by_date[d];
        let ri = 0;
        while ri < rows.len() {
            msg += "  · " + rows[ri] + "\n";
            ri += 1;
        }
        di += 1;
    }
    msg
}

fn collect_window(api_url, user_id, project_uid, headers) {
    let all = [];
    let base = today_ymd();
    let off = 0;
    while off < 4 {
        let date = ymd_add_days(base, off);
        let wd = ymd_weekday(date);
        if wd >= 1 && wd <= 5 {
            let day_items = fetch_day_bookable(date, api_url, user_id, project_uid, headers);
            let j = 0;
            while j < day_items.len() {
                all.push(day_items[j]);
                j += 1;
            }
        }
        off += 1;
    }
    all
}

print("网球场晚场监控");
print("范围: 网球场1/2 · 周一至周五 · 20:00/21:00/22:00 · 含本日共4天");
print("间隔: 5–10 分钟随机；22:00–06:00 静默");
print("────────────────────────────────────────");

let once = str_trim(arg(0)) == "once";

let api_url = "https://wxservice-stg48.pospal.cn/wxapi/AppointmentVenue/LoadValidClassRoomApptSettingV2";
let store_id = "5848810";
let user_id = 5848810;
let project_uid = "1745907191143666876";
let visitor_id = "AFNSCwE4X2gBYwNpCTAKawYzATUJNl5hUmVRMVA4UzEFNlZoWjMHNgFhBzpeZAo6VjEOOA5hXWVWZQprUWhePwA0Umw=";
let version_info = "NC|2026.03.16";
let headers = api_headers(store_id, visitor_id, version_info);

let prev_fp = "";
let primed = false;
let round = 0;

fn push_safe(text) {
    try {
        feishu_push_message(text);
        print("已推送飞书消息");
    } catch (e) {
        print("推送失败（继续监控）: " + e);
    }
}

while true {
    round += 1;

    if is_quiet_hours() {
        if once {
            print("[" + now_hm() + "] once + 静默时段：跳过查询");
            break;
        }
        let wait = quiet_sleep_secs();
        print("[" + now_hm() + "] 静默时段，休眠 " + wait + " 秒…");
        sleep_secs(wait);
        continue;
    }

    print("[" + now_hm() + "] 第 " + round + " 轮查询…");
    let items = [];
    let ok = true;
    try {
        items = collect_window(api_url, user_id, project_uid, headers);
    } catch (e) {
        ok = false;
        print("查询失败: " + e);
        if once {
            throw e;
        }
        sleep_secs(rand_int(300, 600));
        continue;
    }

    if ok {
        let fp = fingerprint(items);
        let summary = format_bookable(items);
        print(summary);

        write_text_file("files/网球场地-可约快照.txt", summary);
        track_output("files/网球场地-可约快照.txt");

        if !primed {
            primed = true;
            prev_fp = fp;
            print("首次快照已建立，推送启动通知");
            push_safe("网球场监控已启动\n" + summary);
        } else if fp != prev_fp {
            prev_fp = fp;
            print("检测到变化，推送通知");
            push_safe("网球场可约有变化\n" + summary);
        } else {
            print("无变化");
        }
    }

    if once {
        print("once 模式结束");
        break;
    }

    let wait = rand_int(300, 600);
    print("下次查询约 " + wait + " 秒后");
    sleep_secs(wait);
}
