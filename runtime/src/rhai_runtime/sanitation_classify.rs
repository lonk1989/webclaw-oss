//! 环卫问题点位视觉分类（本地免费）：CLIP ViT-B/32 zero-shot。
//! 首次运行下载 vision ONNX；文本向量随安装包内嵌，不走云端计费。

use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView};
use ort::session::Session;
use ort::value::Tensor;
use parking_lot::Mutex;
use rhai::{Dynamic, EvalAltResult, Map};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 与表格「问题点位」列一致（顺序须与 `clip_text_embeds_f32.bin` 行序一致）。
pub const PROBLEM_TYPES: &[&str] = &[
    "公厕便池不净",
    "垃圾桶未密闭",
    "垃圾桶满溢",
    "树坑散落垃圾",
    "垃圾点散落垃圾",
    "路面散落垃圾",
];

const EMBED_DIM: usize = 512;
const INPUT_SIZE: u32 = 224;
const VISION_ONNX: &str = "vision_model_quantized.onnx";
/// Xenova CLIP vision quantized ≈ 89MB
const VISION_MIN_BYTES: u64 = 50_000_000;

const CLIP_MEAN: [f32; 3] = [0.48145466, 0.4578275, 0.40821073];
const CLIP_STD: [f32; 3] = [0.26862954, 0.26130258, 0.27577711];

/// 预计算文本向量：6 × 512 f32 little-endian
static TEXT_EMBEDS: &[u8] = include_bytes!("../../data/classify/clip_text_embeds_f32.bin");

static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();

fn err(msg: impl Into<String>) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(
        msg.into().into(),
        rhai::Position::NONE,
    ))
}

fn text_matrix() -> [[f32; EMBED_DIM]; 6] {
    assert_eq!(
        TEXT_EMBEDS.len(),
        6 * EMBED_DIM * 4,
        "clip_text_embeds_f32.bin size mismatch"
    );
    let mut out = [[0f32; EMBED_DIM]; 6];
    for (row, chunk) in TEXT_EMBEDS.chunks_exact(EMBED_DIM * 4).enumerate() {
        for (i, w) in chunk.chunks_exact(4).enumerate() {
            out[row][i] = f32::from_le_bytes([w[0], w[1], w[2], w[3]]);
        }
    }
    out
}

fn vision_path() -> PathBuf {
    crate::paths::classify_models_dir().join(VISION_ONNX)
}

fn ensure_vision_model() -> Result<PathBuf, String> {
    let path = vision_path();
    if path.is_file() {
        let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if len >= VISION_MIN_BYTES {
            return Ok(path);
        }
    }
    let urls = [
        crate::rhai_runtime::model_cdn::cdn_url(&format!("classify-models/{VISION_ONNX}")),
        format!(
            "https://hf-mirror.com/Xenova/clip-vit-base-patch32/resolve/main/onnx/{VISION_ONNX}"
        ),
        format!(
            "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/main/onnx/{VISION_ONNX}"
        ),
    ];
    let mut last_err = String::new();
    for url in urls {
        match crate::rhai_runtime::model_cdn::download_to_file(&url, &path) {
            Ok(()) => {
                let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                if len < VISION_MIN_BYTES {
                    let _ = std::fs::remove_file(&path);
                    last_err = format!("下载文件过小 ({len} bytes): {url}");
                    continue;
                }
                return Ok(path);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                last_err = e;
            }
        }
    }
    Err(format!("下载 CLIP 视觉模型失败: {last_err}"))
}

fn with_session<R>(f: impl FnOnce(&mut Session) -> Result<R, String>) -> Result<R, String> {
    let slot = SESSION.get_or_init(|| Mutex::new(None));
    let mut guard = slot.lock();
    if guard.is_none() {
        let path = ensure_vision_model()?;
        let session = Session::builder()
            .map_err(|e| format!("ORT SessionBuilder: {e}"))?
            .commit_from_file(&path)
            .map_err(|e| format!("加载 CLIP 视觉模型失败: {e}"))?;
        *guard = Some(session);
    }
    f(guard.as_mut().unwrap())
}

/// 对现场照片做视觉分类，返回 `#{ problem_type, confidence, raw, source }`。
pub fn classify_sanitation_image(path: &Path) -> Result<Map, Box<EvalAltResult>> {
    if !path.is_file() {
        return Err(err(format!("图片不存在: {}", path.display())));
    }
    let (label, conf, scores) = classify_local(path).map_err(err)?;
    let raw = scores
        .iter()
        .zip(PROBLEM_TYPES.iter())
        .map(|(s, l)| format!("{l}:{s:.3}"))
        .collect::<Vec<_>>()
        .join(",");

    let mut m = Map::new();
    m.insert("problem_type".into(), Dynamic::from(label));
    m.insert("confidence".into(), Dynamic::from(conf));
    m.insert("raw".into(), Dynamic::from(raw));
    m.insert("source".into(), Dynamic::from("clip_local".to_string()));
    Ok(m)
}

/// 视觉优先；失败则回退 OCR 关键词分类。
pub fn classify_sanitation_with_fallback(
    path: &Path,
    ocr_text: &str,
) -> Result<Map, Box<EvalAltResult>> {
    match classify_sanitation_image(path) {
        Ok(m) => Ok(m),
        Err(e) => {
            let problem_type = crate::rhai_runtime::local_ocr::classify_problem_type(ocr_text);
            let mut m = Map::new();
            m.insert("problem_type".into(), Dynamic::from(problem_type));
            m.insert("confidence".into(), Dynamic::from(0.2_f64));
            m.insert(
                "raw".into(),
                Dynamic::from(format!("clip_fail:{e}; keyword_fallback")),
            );
            m.insert("source".into(), Dynamic::from("ocr_keyword".to_string()));
            Ok(m)
        }
    }
}

fn classify_local(path: &Path) -> Result<(String, f64, [f32; 6]), String> {
    let pixels = preprocess_image(path)?;
    let image_embeds = with_session(|session| {
        let input = Tensor::from_array((
            [1usize, 3, INPUT_SIZE as usize, INPUT_SIZE as usize],
            pixels,
        ))
        .map_err(|e| format!("构造输入张量失败: {e}"))?;
        let outputs = session
            .run(ort::inputs!["pixel_values" => input])
            .map_err(|e| format!("CLIP 推理失败: {e}"))?;
        let (_shape, data) = outputs["image_embeds"]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("提取 image_embeds 失败: {e}"))?;
        if data.len() < EMBED_DIM {
            return Err(format!("image_embeds 维度异常: {}", data.len()));
        }
        let mut v = [0f32; EMBED_DIM];
        v.copy_from_slice(&data[..EMBED_DIM]);
        Ok(v)
    })?;

    let img = l2_normalize(&image_embeds);
    let texts = text_matrix();
    let mut scores = [0f32; 6];
    let mut best_i = 0usize;
    let mut best = f32::NEG_INFINITY;
    for (i, row) in texts.iter().enumerate() {
        let s = dot(&img, row);
        scores[i] = s;
        if s > best {
            best = s;
            best_i = i;
        }
    }
    // 置信度：相对 softmax 近似（温度放大差值）
    let conf = softmax_peak(&scores, 40.0);
    Ok((PROBLEM_TYPES[best_i].to_string(), conf, scores))
}

fn preprocess_image(path: &Path) -> Result<Vec<f32>, String> {
    let img = image::ImageReader::open(path)
        .map_err(|e| format!("打开图片失败: {e}"))?
        .with_guessed_format()
        .map_err(|e| format!("识别图片格式失败: {e}"))?
        .decode()
        .map_err(|e| format!("解码图片失败: {e}"))?;
    let img = crop_watermark(img);
    let img = img.resize_exact(INPUT_SIZE, INPUT_SIZE, FilterType::CatmullRom);
    let rgb = img.to_rgb8();
    // NCHW float32, CLIP normalize
    let mut out = vec![0f32; (3 * INPUT_SIZE * INPUT_SIZE) as usize];
    let (w, h) = (INPUT_SIZE as usize, INPUT_SIZE as usize);
    for y in 0..h {
        for x in 0..w {
            let p = rgb.get_pixel(x as u32, y as u32).0;
            for c in 0..3 {
                let v = (p[c] as f32) / 255.0;
                let n = (v - CLIP_MEAN[c]) / CLIP_STD[c];
                out[c * w * h + y * w + x] = n;
            }
        }
    }
    Ok(out)
}

fn crop_watermark(img: DynamicImage) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w < 32 || h < 32 {
        return img;
    }
    let x0 = ((w as f32) * 0.02).round() as u32;
    let y0 = ((h as f32) * 0.02).round() as u32;
    let x1 = ((w as f32) * 0.98).round() as u32;
    let y1 = ((h as f32) * 0.78).round() as u32;
    if x1 <= x0 + 8 || y1 <= y0 + 8 {
        return img;
    }
    img.crop_imm(x0, y0, x1 - x0, y1 - y0)
}

fn l2_normalize(v: &[f32; EMBED_DIM]) -> [f32; EMBED_DIM] {
    let mut sum = 0f32;
    for x in v {
        sum += *x * *x;
    }
    let n = sum.sqrt().max(1e-12);
    let mut out = [0f32; EMBED_DIM];
    for i in 0..EMBED_DIM {
        out[i] = v[i] / n;
    }
    out
}

fn dot(a: &[f32; EMBED_DIM], b: &[f32; EMBED_DIM]) -> f32 {
    let mut s = 0f32;
    for i in 0..EMBED_DIM {
        s += a[i] * b[i];
    }
    s
}

fn softmax_peak(scores: &[f32; 6], temp: f32) -> f64 {
    let max = scores.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let mut exps = [0f32; 6];
    let mut sum = 0f32;
    for i in 0..6 {
        exps[i] = ((scores[i] - max) * temp).exp();
        sum += exps[i];
    }
    let best = exps.iter().cloned().fold(0f32, f32::max);
    (best / sum.max(1e-12)) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_embeds_load() {
        let m = text_matrix();
        let n0 = (m[0].iter().map(|x| x * x).sum::<f32>()).sqrt();
        assert!((n0 - 1.0).abs() < 0.05, "row0 norm={n0}");
    }

    #[test]
    #[ignore = "needs local onnx + sample images"]
    fn live_classify_sample() {
        use std::fs;
        let roots = [
            r"E:\pictures\I3HhljkRJ1S7oi09IRG74oqWtQmWPBsc\2026-09",
            r"E:\pictures\IimZLB978T2Qt68Na88XpkRQCpzcf1Bh\2026-09",
            r"E:\pictures\ZYo1M7bf7xqDktpd7wI2exVXqKqw9rqW\2026-09",
        ];
        let mut paths = Vec::new();
        for root in roots {
            let Ok(rd) = fs::read_dir(root) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if name.ends_with("51.jpeg")
                    || name.ends_with("41.jpeg")
                    || name.ends_with("43.jpeg")
                    || name == "WPS图片3.jpeg"
                    || name.ends_with("图片3.jpeg")
                {
                    paths.push(p);
                }
            }
        }
        // also pick *3.jpeg carefully
        for root in roots {
            if let Ok(rd) = fs::read_dir(root) {
                for e in rd.flatten() {
                    let p = e.path();
                    let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                    if name.ends_with("3.jpeg")
                        && !name.ends_with("13.jpeg")
                        && !name.ends_with("23.jpeg")
                        && !name.ends_with("43.jpeg")
                        && !name.ends_with("53.jpeg")
                        && !name.ends_with("63.jpeg")
                    {
                        paths.push(p);
                    }
                }
            }
        }
        paths.sort();
        paths.dedup();
        assert!(!paths.is_empty(), "no sample images found");
        for path in paths {
            let m = classify_sanitation_image(&path).expect("classify");
            let pt = m
                .get("problem_type")
                .and_then(|v| v.clone().into_string().ok())
                .unwrap_or_default();
            let conf = m
                .get("confidence")
                .and_then(|v| v.clone().try_cast::<f64>())
                .unwrap_or(0.0);
            println!(
                "{} => {} ({:.2})",
                path.file_name().unwrap().to_string_lossy(),
                pt,
                conf
            );
        }
    }
}
