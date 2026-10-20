//! 同一视角配对：裁掉水印区后比较画面结构（差分哈希 + 色块），并用 OCR 地点词加分。
//!
//! 巡检图与保洁图水印不同、整洁程度也不同，所以不比整图，只比中间场景。
//! 硬约束：整改后水印时间必须严格晚于整改前（两边都能解析时生效）。

use image::imageops::FilterType;
use image::{GenericImageView, RgbImage};
use rhai::{Array, Dynamic, EvalAltResult, Map};
use std::collections::HashSet;
use std::path::Path;

use super::local_ocr;

/// 有共同地点词时，画面分达到此值即配对。
const VISUAL_WITH_TEXT: i64 = 46;
/// 没有共同地点词时，要求更高的画面相似度，避免只因垃圾桶外形误配。
const VISUAL_ONLY: i64 = 70;

struct SceneSig {
    dhash: u64,
    colors: Vec<u8>,
}

struct Item {
    path: String,
    text: String,
    time: String,
    datetime: String,
    sig: Option<SceneSig>,
}

pub fn pair_scene_images(before: Array, after: Array) -> Result<Map, Box<EvalAltResult>> {
    let befores = parse_items(before)?;
    let afters = parse_items(after)?;
    Ok(pair_loaded(&befores, &afters))
}

/// 调试：对单对 before/after 计算画面分、地点加分、阈值与是否会配对（不过滤低分）。
pub fn score_scene_pair(before: Map, after: Map) -> Result<Map, Box<EvalAltResult>> {
    let b = item_from_map(before).map_err(|e| {
        Box::new(EvalAltResult::ErrorRuntime(e.into(), rhai::Position::NONE))
    })?;
    let a = item_from_map(after).map_err(|e| {
        Box::new(EvalAltResult::ErrorRuntime(e.into(), rhai::Position::NONE))
    })?;
    Ok(score_loaded(&b, &a))
}

fn item_from_map(map: Map) -> Result<Item, String> {
    let path = map
        .get("path")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_default();
    if path.is_empty() {
        return Err("score_scene_pair 需要 path".into());
    }
    let text = map
        .get("text")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_default();
    let time = map
        .get("time")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_default();
    let datetime = map
        .get("datetime")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_default();
    let sig = signature(Path::new(&path)).ok();
    Ok(Item {
        path,
        text,
        time,
        datetime,
        sig,
    })
}

fn score_loaded(b: &Item, a: &Item) -> Map {
    let mut out = Map::new();
    out.insert("before".into(), Dynamic::from(b.path.clone()));
    out.insert("after".into(), Dynamic::from(a.path.clone()));

    let time_ok = after_later_than_before(b, a);
    out.insert("time_ok".into(), Dynamic::from(time_ok));
    out.insert(
        "before_time".into(),
        Dynamic::from(if b.datetime.is_empty() {
            b.time.clone()
        } else {
            b.datetime.clone()
        }),
    );
    out.insert(
        "after_time".into(),
        Dynamic::from(if a.datetime.is_empty() {
            a.time.clone()
        } else {
            a.datetime.clone()
        }),
    );

    let (visual, bonus, sig_ok) = match (&b.sig, &a.sig) {
        (Some(bs), Some(asig)) => (visual_score(bs, asig), text_bonus(&b.text, &a.text), true),
        _ => (0, text_bonus(&b.text, &a.text), false),
    };
    let threshold = if bonus > 0 {
        VISUAL_WITH_TEXT
    } else {
        VISUAL_ONLY
    };
    let accept = sig_ok && time_ok && visual >= threshold;
    let score = (visual + bonus).min(100);

    out.insert("sig_ok".into(), Dynamic::from(sig_ok));
    out.insert("visual".into(), Dynamic::from(visual));
    out.insert("bonus".into(), Dynamic::from(bonus));
    out.insert("threshold".into(), Dynamic::from(threshold));
    out.insert("score".into(), Dynamic::from(score));
    out.insert("accept".into(), Dynamic::from(accept));
    out.insert(
        "rule".into(),
        Dynamic::from(if bonus > 0 {
            format!("有地点加分，画面分>={VISUAL_WITH_TEXT}")
        } else {
            format!("无地点加分，画面分>={VISUAL_ONLY}")
        }),
    );
    out
}

fn parse_items(arr: Array) -> Result<Vec<Item>, Box<EvalAltResult>> {
    let mut out = Vec::with_capacity(arr.len());
    for d in arr {
        let meta = path_and_meta(&d).map_err(|e| {
            Box::new(EvalAltResult::ErrorRuntime(e.into(), rhai::Position::NONE))
        })?;
        if meta.path.is_empty() {
            continue;
        }
        let sig = signature(Path::new(&meta.path)).ok();
        out.push(Item {
            path: meta.path,
            text: meta.text,
            time: meta.time,
            datetime: meta.datetime,
            sig,
        });
    }
    Ok(out)
}

struct ItemMeta {
    path: String,
    text: String,
    time: String,
    datetime: String,
}

fn path_and_meta(d: &Dynamic) -> Result<ItemMeta, String> {
    if let Ok(s) = d.clone().into_string() {
        return Ok(ItemMeta {
            path: s,
            text: String::new(),
            time: String::new(),
            datetime: String::new(),
        });
    }
    let map = d
        .clone()
        .try_cast::<Map>()
        .ok_or_else(|| "配对输入须为路径或 #{ path, text }".to_string())?;
    let path = map
        .get("path")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_default();
    let text = map
        .get("text")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_default();
    let time = map
        .get("time")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_default();
    let datetime = map
        .get("datetime")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_default();
    Ok(ItemMeta {
        path,
        text,
        time,
        datetime,
    })
}

/// 两边都能解析出水印时间时，整改后必须严格晚于整改前；缺时间则不拦。
fn after_later_than_before(before: &Item, after: &Item) -> bool {
    let Some(bt) = item_instant(before) else {
        return true;
    };
    let Some(at) = item_instant(after) else {
        return true;
    };
    at > bt
}

fn item_instant(item: &Item) -> Option<chrono::NaiveDateTime> {
    if !item.datetime.trim().is_empty() {
        if let Some(dt) = local_ocr::parse_dt(&item.datetime) {
            return Some(dt);
        }
    }
    if !item.time.trim().is_empty() {
        return local_ocr::parse_dt(&item.time);
    }
    None
}

fn pair_loaded(befores: &[Item], afters: &[Item]) -> Map {
    let mut cands: Vec<(usize, usize, i64, i64, i64)> = Vec::new();
    for (bi, b) in befores.iter().enumerate() {
        for (ai, a) in afters.iter().enumerate() {
            if !after_later_than_before(b, a) {
                continue;
            }
            let (Some(bs), Some(asig)) = (&b.sig, &a.sig) else {
                continue;
            };
            let visual = visual_score(bs, asig);
            let bonus = text_bonus(&b.text, &a.text);
            let accept = if bonus > 0 {
                visual >= VISUAL_WITH_TEXT
            } else {
                visual >= VISUAL_ONLY
            };
            if !accept {
                continue;
            }
            let score = (visual + bonus).min(100);
            cands.push((bi, ai, score, visual, bonus));
        }
    }
    cands.sort_by(|a, b| b.2.cmp(&a.2).then(b.3.cmp(&a.3)));

    let mut used_b = vec![false; befores.len()];
    let mut used_a = vec![false; afters.len()];
    let mut pairs = Array::new();
    for (bi, ai, score, visual, bonus) in cands {
        if used_b[bi] || used_a[ai] {
            continue;
        }
        used_b[bi] = true;
        used_a[ai] = true;
        let mut m = Map::new();
        m.insert("before".into(), Dynamic::from(befores[bi].path.clone()));
        m.insert("after".into(), Dynamic::from(afters[ai].path.clone()));
        m.insert("score".into(), Dynamic::from(score));
        m.insert("visual".into(), Dynamic::from(visual));
        m.insert("bonus".into(), Dynamic::from(bonus));
        pairs.push(Dynamic::from(m));
    }

    let mut unmatched_before = Array::new();
    for (i, item) in befores.iter().enumerate() {
        if !used_b[i] {
            unmatched_before.push(path_map(&item.path));
        }
    }
    let mut unmatched_after = Array::new();
    for (i, item) in afters.iter().enumerate() {
        if !used_a[i] {
            unmatched_after.push(path_map(&item.path));
        }
    }

    let mut out = Map::new();
    out.insert("pairs".into(), Dynamic::from(pairs));
    out.insert("unmatched_before".into(), Dynamic::from(unmatched_before));
    out.insert("unmatched_after".into(), Dynamic::from(unmatched_after));
    out
}

fn path_map(path: &str) -> Dynamic {
    let mut m = Map::new();
    m.insert("path".into(), Dynamic::from(path.to_string()));
    Dynamic::from(m)
}

fn signature(path: &Path) -> Result<SceneSig, String> {
    let img = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| format!("解码失败: {e}"))?;
    let (w, h) = img.dimensions();
    if w < 16 || h < 16 {
        return Err("图片过小".into());
    }
    // 去掉左侧保洁水印、底部今日水印条
    let x0 = ((w as f32) * 0.26) as u32;
    let y0 = ((h as f32) * 0.04) as u32;
    let x1 = ((w as f32) * 0.98).max(x0 as f32 + 8.0) as u32;
    let y1 = ((h as f32) * 0.74).max(y0 as f32 + 8.0) as u32;
    let cw = (x1.min(w) - x0).max(8);
    let ch = (y1.min(h) - y0).max(8);
    let rgb = img.to_rgb8();
    let cropped = image::imageops::crop_imm(&rgb, x0, y0, cw, ch).to_image();
    let small = image::imageops::resize(&cropped, 72, 54, FilterType::Triangle);
    Ok(SceneSig {
        dhash: dhash8(&small),
        colors: color_grid(&small, 4, 3),
    })
}

fn dhash8(img: &RgbImage) -> u64 {
    let g = block_luma(img, 9, 8);
    let mut bits = 0u64;
    let mut k = 0u32;
    for y in 0..8 {
        for x in 0..8 {
            let left = g[(y * 9 + x) as usize];
            let right = g[(y * 9 + x + 1) as usize];
            if left > right {
                bits |= 1u64 << k;
            }
            k += 1;
        }
    }
    bits
}

fn color_grid(img: &RgbImage, cols: u32, rows: u32) -> Vec<u8> {
    let (w, h) = img.dimensions();
    let mut out = Vec::with_capacity((cols * rows * 3) as usize);
    for by in 0..rows {
        for bx in 0..cols {
            let x0 = bx * w / cols;
            let x1 = ((bx + 1) * w / cols).max(x0 + 1);
            let y0 = by * h / rows;
            let y1 = ((by + 1) * h / rows).max(y0 + 1);
            let mut sr = 0u32;
            let mut sg = 0u32;
            let mut sb = 0u32;
            let mut n = 0u32;
            for y in y0..y1.min(h) {
                for x in x0..x1.min(w) {
                    let p = img.get_pixel(x, y);
                    sr += p[0] as u32;
                    sg += p[1] as u32;
                    sb += p[2] as u32;
                    n += 1;
                }
            }
            let n = n.max(1);
            out.push((sr / n) as u8);
            out.push((sg / n) as u8);
            out.push((sb / n) as u8);
        }
    }
    out
}

fn block_luma(img: &RgbImage, cols: u32, rows: u32) -> Vec<u8> {
    let (w, h) = img.dimensions();
    let mut out = vec![0u8; (cols * rows) as usize];
    for by in 0..rows {
        for bx in 0..cols {
            let x0 = bx * w / cols;
            let x1 = ((bx + 1) * w / cols).max(x0 + 1);
            let y0 = by * h / rows;
            let y1 = ((by + 1) * h / rows).max(y0 + 1);
            let mut sum = 0u32;
            let mut n = 0u32;
            for y in y0..y1.min(h) {
                for x in x0..x1.min(w) {
                    let p = img.get_pixel(x, y);
                    sum += (p[0] as u32 * 3 + p[1] as u32 * 6 + p[2] as u32) / 10;
                    n += 1;
                }
            }
            out[(by * cols + bx) as usize] = (sum / n.max(1)) as u8;
        }
    }
    out
}

fn visual_score(a: &SceneSig, b: &SceneSig) -> i64 {
    let ham = (a.dhash ^ b.dhash).count_ones();
    let d = (64 - ham) as f32 / 64.0;
    let mut l1 = 0u32;
    let n = a.colors.len().min(b.colors.len());
    for i in 0..n {
        l1 += (a.colors[i] as i32 - b.colors[i] as i32).unsigned_abs();
    }
    let c = if n == 0 {
        0.0
    } else {
        1.0 - (l1 as f32 / (n as f32 * 255.0))
    };
    ((0.58 * d + 0.42 * c) * 100.0).round() as i64
}

fn text_bonus(a: &str, b: &str) -> i64 {
    let ta = tokens(a);
    let tb = tokens(b);
    if ta.is_empty() || tb.is_empty() {
        return 0;
    }
    let mut shared = 0i64;
    for left in &ta {
        let hit = tb.iter().any(|right| tokens_match(left, right));
        if hit {
            shared += 1;
        }
    }
    if shared == 0 {
        0
    } else {
        12 + 6 * (shared - 1).min(2)
    }
}

fn tokens_match(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let ac = a.chars().count();
    let bc = b.chars().count();
    if ac < 2 || bc < 2 {
        return false;
    }
    // 地点词常被截断：溢鑫苑 / 溢鑫苑小区
    let (short, long) = if ac <= bc { (a, b) } else { (b, a) };
    short.chars().count() >= 3 && long.contains(short)
}

fn tokens(text: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    for raw in text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '·' | '：'
                    | ':'
                    | '，'
                    | ','
                    | '。'
                    | '.'
                    | '/'
                    | '\\'
                    | '|'
                    | '、'
                    | '-'
                    | '—'
                    | '（'
                    | '）'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '【'
                    | '】'
            )
    }) {
        let t = normalize_token(raw);
        if is_useful_token(&t) {
            set.insert(t);
        }
    }
    set
}

fn normalize_token(raw: &str) -> String {
    let mut t = raw.trim().trim_matches(|c: char| c == '·' || c == '：' || c == ':');
    const PREFIXES: &[&str] = &[
        "地点",
        "时间",
        "拍摄人",
        "阳泉市城区",
        "阳泉市",
        "城区",
    ];
    loop {
        let mut stripped = false;
        for p in PREFIXES {
            if let Some(rest) = t.strip_prefix(p) {
                t = rest.trim_start_matches(|c: char| {
                    c == '·' || c == '：' || c == ':' || c.is_whitespace()
                });
                stripped = true;
                break;
            }
        }
        if !stripped {
            break;
        }
    }
    t.to_string()
}

fn is_useful_token(t: &str) -> bool {
    let n = t.chars().count();
    if n < 2 || n > 16 {
        return false;
    }
    let lower = t.to_lowercase();
    const STOP: &[&str] = &[
        "中环洁",
        "clean",
        "pro",
        "cleanpro",
        "时间",
        "地点",
        "拍摄",
        "拍摄人",
        "班组长",
        "今日",
        "水印",
        "相机",
        "真实",
        "可验",
        "可靠",
        "防伪",
        "阳泉",
        "城区",
        "星期一",
        "星期二",
        "星期三",
        "星期四",
        "星期五",
        "星期六",
        "星期日",
        "晴",
        "阴",
        "多云",
        "公司",
        "环境",
        "服务",
        "有限",
        "城市",
    ];
    if STOP.iter().any(|s| lower == *s) {
        return false;
    }
    if t.chars().all(|c| c.is_ascii_digit() || c == ':' || c == '.' || c == '-') {
        return false;
    }
    // 纯英文短词（水印品牌残片）丢掉；保留中文地点
    let cjk = t.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count();
    cjk >= 2
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn solid(path: &std::path::Path, rgb: [u8; 3]) {
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(80, 60, |_, _| Rgb(rgb));
        img.save(path).unwrap();
    }

    #[test]
    fn identical_images_pair_unmatched_when_different() {
        let dir = std::env::temp_dir().join(format!(
            "webclaw-scene-pair-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a1 = dir.join("a1.png");
        let a2 = dir.join("a2.png");
        let b1 = dir.join("b1.png");
        solid(&a1, [40, 90, 160]);
        solid(&a2, [40, 90, 160]);
        solid(&b1, [200, 30, 30]);

        let before = vec![Item {
            path: a1.display().to_string(),
            text: "地点：阳泉市城区·樱花园".into(),
            time: "09:00:00".into(),
            datetime: "2026-09-20 09:00:00".into(),
            sig: signature(&a1).ok(),
        }];
        let after = vec![
            Item {
                path: b1.display().to_string(),
                text: "地点：阳泉市城区·樱花园".into(),
                time: "10:00:00".into(),
                datetime: "2026-09-20 10:00:00".into(),
                sig: signature(&b1).ok(),
            },
            Item {
                path: a2.display().to_string(),
                text: "地点：阳泉市城区·樱花园".into(),
                time: "10:30:00".into(),
                datetime: "2026-09-20 10:30:00".into(),
                sig: signature(&a2).ok(),
            },
        ];
        let paired = pair_loaded(&before, &after);
        let pairs = paired
            .get("pairs")
            .unwrap()
            .clone()
            .try_cast::<Array>()
            .unwrap();
        assert_eq!(pairs.len(), 1, "应只配上同色图");
        let m = pairs[0].clone().try_cast::<Map>().unwrap();
        let after_path = m.get("after").unwrap().clone().into_string().unwrap();
        assert!(after_path.contains("a2.png"), "{after_path}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reject_after_not_later_than_before() {
        let dir = std::env::temp_dir().join(format!(
            "webclaw-scene-time-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let before_path = dir.join("before.png");
        let after_early = dir.join("after_early.png");
        let after_late = dir.join("after_late.png");
        solid(&before_path, [40, 90, 160]);
        solid(&after_early, [40, 90, 160]);
        solid(&after_late, [40, 90, 160]);

        let before = vec![Item {
            path: before_path.display().to_string(),
            text: "地点：溢鑫苑".into(),
            time: "12:00:00".into(),
            datetime: "2026-09-20 12:00:00".into(),
            sig: signature(&before_path).ok(),
        }];
        let after = vec![
            Item {
                path: after_early.display().to_string(),
                text: "地点：溢鑫苑".into(),
                time: "11:00:00".into(),
                datetime: "2026-09-20 11:00:00".into(),
                sig: signature(&after_early).ok(),
            },
            Item {
                path: after_late.display().to_string(),
                text: "地点：溢鑫苑".into(),
                time: "13:00:00".into(),
                datetime: "2026-09-20 13:00:00".into(),
                sig: signature(&after_late).ok(),
            },
        ];
        let paired = pair_loaded(&before, &after);
        let pairs = paired
            .get("pairs")
            .unwrap()
            .clone()
            .try_cast::<Array>()
            .unwrap();
        assert_eq!(pairs.len(), 1);
        let m = pairs[0].clone().try_cast::<Map>().unwrap();
        let after_path = m.get("after").unwrap().clone().into_string().unwrap();
        assert!(
            after_path.contains("after_late.png"),
            "应跳过更早的整改后图: {after_path}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn location_tokens_ignore_city_prefix() {
        let a = tokens("地点：阳泉市城区·溢鑫苑小区\n中环洁 CLEAN PRO");
        let b = tokens("阳泉市城区·溢鑫苑\n今日水印相机");
        assert!(a.iter().any(|t| t.contains("溢鑫苑")), "{a:?}");
        assert!(text_bonus(
            "地点：阳泉市城区·溢鑫苑小区",
            "阳泉市城区·溢鑫苑"
        ) > 0);
        assert!(b.iter().any(|t| t.contains("溢鑫苑")), "{b:?}");
    }

    #[test]
    fn score_gold_tree_sample_pair() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/samples");
        let before_path = dir.join("pair-before.jpg");
        let after_path = dir.join("pair-after.jpg");
        assert!(
            before_path.is_file() && after_path.is_file(),
            "缺少样例图: {} / {}",
            before_path.display(),
            after_path.display()
        );

        let before = Item {
            path: before_path.display().to_string(),
            text: "09:08\n2026年09月14日 星期一\n阳泉市城区 · 阳泉隆鑫商厦有限公司\n今日水印相机".into(),
            time: "09:08".into(),
            datetime: "2026-09-14 09:08".into(),
            sig: signature(&before_path).ok(),
        };
        let after = Item {
            path: after_path.display().to_string(),
            text: "中环洁 CLEAN PRO\n中环洁城市环境服务有限公司\n2026.09.14 09:09\n阳泉市城区 · 百纺购物中心\n今日水印".into(),
            time: "09:09".into(),
            datetime: "2026-09-14 09:09".into(),
            sig: signature(&after_path).ok(),
        };
        let r = score_loaded(&before, &after);
        let visual = r.get("visual").unwrap().as_int().unwrap();
        let bonus = r.get("bonus").unwrap().as_int().unwrap();
        let threshold = r.get("threshold").unwrap().as_int().unwrap();
        let accept = r.get("accept").unwrap().as_bool().unwrap();
        let time_ok = r.get("time_ok").unwrap().as_bool().unwrap();
        let before_toks = tokens(&before.text);
        let after_toks = tokens(&after.text);
        println!(
            "gold-tree pair: visual={visual} bonus={bonus} threshold={threshold} time_ok={time_ok} accept={accept}"
        );
        println!("before tokens={before_toks:?}");
        println!("after tokens={after_toks:?}");
        assert!(time_ok, "09:09 应晚于 09:08");
        assert!(before.sig.is_some() && after.sig.is_some());
        // 这对样例画面分约 71；有无地点加分都过线（有加分阈值46，无加分阈值70）
        assert!(
            visual >= 70,
            "预期画面分≥70，实际 visual={visual} bonus={bonus}"
        );
        assert!(accept, "样例对在完整水印文下应能配对");
    }
}
