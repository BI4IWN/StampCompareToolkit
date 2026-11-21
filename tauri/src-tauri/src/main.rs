#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use axum::{routing::{get, post}, Json, Router, response::Html};
use tauri::Manager;
use base64::Engine as _;
use image::RgbaImage;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

const PORT: u16 = 18970;

mod library;

static ENGINE: std::sync::OnceLock<Option<Arc<stamp_ocr::Engine>>> = std::sync::OnceLock::new();
static INDEX: &str = include_str!("../../web/index.html");

fn decode_png(b64: &str) -> Option<RgbaImage> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    Some(img)
}

/// 只用 stamp-core 的几何能力：拟合外圈 + 去畸变，证明 Rust 内核已在进程内跑起来
fn analyze(img: &RgbaImage) -> Value {
    let t0 = Instant::now();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let alpha: Vec<u8> = img.pixels().map(|p| p.0[3]).collect();
    let ellipse = stamp_core::fit::fit_ellipse(w, h, &alpha);
    let dewarp = stamp_core::dewarp::dewarp_iterative(img);
    json!({
        "engine": "rust-geometry",
        "elapsed_ms": t0.elapsed().as_millis() as u64,
        "fit": ellipse.as_ref().map(|e| json!({
            "cx": e.cx, "cy": e.cy, "a": e.a, "b": e.b,
            "theta_deg": e.theta_deg, "ratio": e.ratio,
            "coverage": e.coverage, "inlier_ratio": e.inlier_ratio, "points": e.points,
        })),
        "dewarp": dewarp.as_ref().map(|d| json!({ "rounds": d.rounds, "ratio_after": d.after.as_ref().map(|f| f.ratio) })),
    })
}



async fn index_html() -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        index_body(),
    )
        .into_response()
}

fn index_body() -> String {
    // 调试钩子：STAMP_AUTOIMG=/path/a.png 时在页面里注入自动加载指令（用于真机复现）
    match std::env::var("STAMP_AUTOIMG") {
        Ok(p) if !p.is_empty() => {
            let tag = format!(
                "<script>window.__AUTOIMG__={:?};</script>",
                p.replace('\\', "\\\\").replace('"', "\\\"")
            );
            INDEX.replacen("</body>", &format!("{tag}</body>"), 1)
        }
        _ => INDEX.to_string(),
    }
}



// 圆章校正交给 Rust：WKWebView 与 Chromium 的 canvas 采样有差异，几何放后端结果可复现
async fn dewarp(
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let w: u32 = q.get("w").and_then(|v| v.parse().ok()).unwrap_or(0);
    let h: u32 = q.get("h").and_then(|v| v.parse().ok()).unwrap_or(0);
    if w == 0 || h == 0 || body.len() != (w * h * 4) as usize {
        return (axum::http::StatusCode::BAD_REQUEST, "bad payload").into_response();
    }
    let img = match image::RgbaImage::from_raw(w, h, body.to_vec()) {
        Some(i) => i,
        None => return (axum::http::StatusCode::BAD_REQUEST, "bad buffer").into_response(),
    };
    let force = q.get("force").map(|v| v == "1").unwrap_or(false);
    let result = if force {
        // 诊断用：跳过闸门，直接按拟合椭圆拉一次
        stamp_core::fit::fit_ellipse(img.width() as usize, img.height() as usize, &img.as_raw().chunks(4).map(|p| p[3]).collect::<Vec<u8>>())
            .map(|ell| {
                let warped = stamp_core::dewarp::dewarp_once(&img, &ell);
                stamp_core::dewarp::DewarpResult { image: warped, before: ell.clone(), after: None, rounds: 1, radius: 0.0 }
            })
    } else {
        stamp_core::dewarp::dewarp_iterative(&img)
    };
    match result {
        Some(res) => {
            let mut out = std::io::Cursor::new(Vec::new());
            if image::DynamicImage::ImageRgba8(res.image)
                .write_to(&mut out, image::ImageFormat::Png)
                .is_err()
            {
                return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "encode fail").into_response();
            }
            (
                [
                    (axum::http::header::CONTENT_TYPE, "image/png"),
                    (axum::http::header::HeaderName::from_static("x-warp-before"), "1"),
                ],
                out.into_inner(),
            )
                .into_response()
        }
        None => (axum::http::StatusCode::NO_CONTENT, "no warp").into_response(),
    }
}


// ---------- 印章库 / 设置（P3-2：把 Electron 的 IPC 搬到 Rust） ----------
async fn api_settings_get() -> Json<Value> { Json(library::settings_json()) }
async fn api_settings_set(Json(body): Json<Value>) -> Json<Value> { Json(library::update_settings(&body)) }
async fn api_settings_reset() -> Json<Value> { Json(library::reset_settings()) }
async fn api_lib_list() -> Json<Value> { Json(library::lib_list()) }
async fn api_lib_search(axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>) -> Json<Value> {
    Json(library::lib_search(q.get("q").map(|s| s.as_str()).unwrap_or("")))
}
async fn api_lib_load(axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>) -> Json<Value> {
    Json(library::lib_load(q.get("id").map(|s| s.as_str()).unwrap_or("")))
}
async fn api_lib_save(Json(body): Json<Value>) -> Json<Value> { Json(library::lib_save(&body)) }
async fn api_lib_delete(Json(body): Json<Value>) -> Json<Value> {
    Json(library::lib_delete(body.get("id").and_then(|v| v.as_str()).unwrap_or("")))
}
async fn api_lib_rename(Json(body): Json<Value>) -> Json<Value> { Json(library::lib_rename(&body)) }
async fn api_lib_reveal() -> Json<Value> { Json(library::lib_reveal()) }


// ---------- 截图（用系统 screencapture 的框选界面，等价 Electron 的覆盖层选择） ----------
async fn api_capture_start() -> Json<Value> {
    let id = format!("{}", std::process::id() as u64 * 1000 + (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_millis() as u64).unwrap_or(0)));
    let path = format!("/tmp/stamp-capture-{id}.png");
    let _ = std::fs::remove_file(&path);
    match std::process::Command::new("screencapture").arg("-i").arg("-x").arg(&path).spawn() {
        Ok(_) => Json(json!({"ok": true, "id": id, "path": path})),
        Err(e) => Json(json!({"ok": false, "error": format!("无法启动截图：{e}")})),
    }
}

async fn api_capture_poll(axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>) -> Json<Value> {
    let path = q.get("path").cloned().unwrap_or_default();
    if path.is_empty() { return Json(json!({"done": false})); }
    match std::fs::metadata(&path) {
        Ok(m) if m.len() > 1024 => match std::fs::read(&path) {
            Ok(bytes) => {
                let _ = std::fs::remove_file(&path);
                let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                Json(json!({"done": true, "png": format!("data:image/png;base64,{b64}")}))
            }
            Err(_) => Json(json!({"done": false})),
        },
        _ => Json(json!({"done": false})),
    }
}


async fn api_lib_choose_path() -> Json<Value> {
    // 用系统「选择文件夹」对话框（AppleScript），返回 {ok, path}
    let script = r#"POSIX path of (choose folder with prompt "选择印章库文件夹")"#;
    match std::process::Command::new("osascript").arg("-e").arg(script).output() {
        Ok(o) if o.status.success() => {
            let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if path.is_empty() { return Json(json!({"ok": false, "canceled": true})); }
            let mut body = serde_json::Map::new();
            body.insert("libPath".into(), json!(path));
            Json(library::update_settings(&Value::Object(body)))
        }
        Ok(_) => Json(json!({"ok": false, "canceled": true})),
        Err(e) => Json(json!({"ok": false, "error": format!("无法打开选择框：{e}")})),
    }
}

async fn diagimg() -> axum::response::Response {
    use axum::response::IntoResponse;
    match std::env::var("STAMP_AUTOIMG").ok().and_then(|p| std::fs::read(p).ok()) {
        Some(bytes) => ([(axum::http::header::CONTENT_TYPE, "image/png")], bytes).into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "no image").into_response(),
    }
}


// 极简探针通道：GET /diagp?m=<任意文本>，直接追加到 /tmp/stamp-diag.log。
// 用 <img> 或 fetch(no-cors) 都能打进来，绕开 WKWebView 上 POST JSON 的静默失败。
async fn diagp(axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>) -> &'static str {
    use std::io::Write;
    let msg = q.get("m").cloned().unwrap_or_default();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/stamp-diag.log") {
        let _ = writeln!(f, "{msg}");
    }
    "ok"
}

async fn diag(body: String) -> &'static str {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true).open("/tmp/stamp-diag.log")
    {
        let _ = writeln!(f, "{body}");
    }
    "ok"
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": "3.6.21", "ocr": true, "engine": "rust" }))
}

async fn ocr(Json(body): Json<Value>) -> Json<Value> {
    let b64 = body.get("image").and_then(|v| v.as_str()).unwrap_or("");
    let img = match decode_png(b64) {
        Some(i) => i,
        None => return Json(json!({ "error": "image decode failed" })),
    };
    let eng = ENGINE.get().cloned().flatten();
    if let Some(eng) = eng {
        let t0 = Instant::now();
        match eng.recognize(&img) {
            Ok(r) => {
                let text_for_rank = r.text.clone();
                let cands = library::rank_candidates(&text_for_rank, 3);
                let top = cands.first().and_then(|c| c.get("score")).and_then(|v| v.as_f64()).unwrap_or(0.0);
                // 库名兜底：OCR 文本与库里某条足够接近时，直接用库名（等价 Electron 的 library 覆盖）
                let cand_name = cands.first().and_then(|c| c.get("name")).and_then(|v| v.as_str()).map(|s| s.to_string());
                let (company, match_source) = match (&r.company, &cand_name, top) {
                    (_, Some(n), t) if t >= 0.50 => (Some(n.clone()), "library"),
                    (c, _, _) => (c.clone(), "ocr"),
                };
                // 前端契约：必须带 success:true，texts 是 [{text}] 对象数组
                let texts_obj: Vec<Value> = r.texts.iter().map(|t| json!({ "text": t })).collect();
                return Json(json!({
                    "success": true,
                    "engine": "rust-paddleocr-vl-rec",
                    "elapsed_ms": t0.elapsed().as_millis() as u64,
                    "stamps": [{
                        "texts": texts_obj, "full_text": r.text, "raw_text": r.text,
                        "company": company, "type": r.kind, "code": r.code, "match_source": match_source,
                        "confidence": top, "method": "rust-ring",
                        "candidates": cands, "needs_review": top < 0.72,
                        "warp_ratio": r.warp_ratio, "radius": r.radius,
                        "engine_ms": r.elapsed_ms
                    }]
                }));
            }
            Err(e) => return Json(json!({ "engine": "rust", "error": format!("{e}") })),
        }
    }
    let base = analyze(&img);
    // 结构对齐 Python /ocr，前端不会因缺字段崩掉
    Json(json!({
        "engine": "rust",
        "elapsed_ms": base.get("elapsed_ms").cloned().unwrap_or(json!(0)),
        "stamps": [ { "texts": [], "full_text": "", "company": null, "type": null, "code": null,
                      "confidence": 0, "method": "rust-geometry", "candidates": [], "needs_review": true,
                      "detail": base } ]
    }))
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            // 模型目录：依次尝试 环境变量 / resource_dir / 可执行文件同级 / 上层 Resources / cwd
            let mut cands: Vec<std::path::PathBuf> = Vec::new();
            if let Ok(v) = std::env::var("STAMP_MODELS") { cands.push(v.into()); }
            if let Ok(p) = app.path().resource_dir() { cands.push(p.join("models")); }
            if let Ok(exe) = std::env::current_exe() {
                if let Some(d) = exe.parent() {
                    cands.push(d.join("models"));
                    if let Some(up) = d.parent() {
                        cands.push(up.join("Resources").join("models"));
                        cands.push(up.join("models"));
                    }
                }
            }
            cands.push(std::path::PathBuf::from("models"));
            let dir = cands
                .iter()
                .find(|p| p.join("rec_batch1.onnx").exists())
                .cloned()
                .unwrap_or_else(|| std::path::PathBuf::from("models"));
            eprintln!("[engine] models dir = {:?} (candidates: {:?})", dir, cands);
            let loaded = match stamp_ocr::Engine::load(&dir) {
                Ok(e) => Some(Arc::new(e)),
                Err(err) => { eprintln!("[engine] load failed dir={:?}: {err}", dir); None }
            };
            let _ = ENGINE.set(loaded);
            tauri::async_runtime::spawn(async move {
                let app = Router::new()
                    .route("/", get(index_html))
                    .route("/stamp-compare.html", get(index_html))
                    .route("/health", get(health))
                    .route("/ocr", post(ocr))
                    .route("/diag", post(diag))
                    .route("/ai/test", post(api_ai_test))
                    .route("/ai/stamp", post(api_ai_stamp).layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024)))
                    .route("/ai-ocr", post(api_ai_ocr).layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024)))
                    .route("/ai/ocr", post(api_ai_ocr).layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024)))
                    .route("/diagp", get(diagp))
                    .route("/diagimg", get(diagimg))
                    .route("/settings", get(api_settings_get).post(api_settings_set))
                    .route("/settings/reset", post(api_settings_reset))
                    .route("/lib/list", get(api_lib_list))
                    .route("/lib/search", get(api_lib_search))
                    .route("/lib/load", get(api_lib_load))
                    .route("/lib/save", post(api_lib_save))
                    .route("/lib/delete", post(api_lib_delete))
                    .route("/lib/rename", post(api_lib_rename))
                    .route("/lib/reveal", post(api_lib_reveal))
                    .route("/lib/choose-path", post(api_lib_choose_path))
                    .route("/capture/start", post(api_capture_start))
                    .route("/capture/poll", get(api_capture_poll))
                    .route("/dewarp", post(dewarp).layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024)))
                    .layer(tower_http::cors::CorsLayer::permissive());
                let addr = SocketAddr::from(([127, 0, 0, 1], PORT));
                let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
                axum::serve(listener, app).await.unwrap();
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// ---------- AI 代理（OpenAI 兼容）：/ai/test、/ai/stamp、/ai-ocr ----------
fn ai_chat(cfg: &Value, prompt: &str, image: Option<&str>, max_tokens: u32) -> Result<Value, String> {
    let base = cfg.get("base_url").and_then(|v| v.as_str()).unwrap_or("").trim_end_matches('/').to_string();
    let key = cfg.get("api_key").and_then(|v| v.as_str()).unwrap_or("");
    let model = cfg.get("model").and_then(|v| v.as_str()).unwrap_or("");
    if base.is_empty() || key.is_empty() || model.is_empty() {
        return Err("AI 配置不完整：需要 API 地址、模型与 API Key".into());
    }
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err(format!("API 地址不合法：{base}"));
    }
    // 智谱/AWS 等「Anthropic 兼容」端点：/api/anthropic + /v1/messages + x-api-key
    if base.contains("/anthropic") {
        let url = format!("{base}/v1/messages");
        let mut parts = vec![json!({"type": "text", "text": prompt})];
        if let Some(img) = image {
            let (media, data) = prepare_image_raw(img);
            parts.push(json!({"type": "image", "source": {"type": "base64", "media_type": media, "data": data}}));
        }
        let body = json!({
            "model": model,
            "max_tokens": max_tokens,
            "messages": [{ "role": "user", "content": parts }]
        });
        let resp = ureq::post(&url)
            .set("x-api-key", key)
            .set("anthropic-version", "2023-06-01")
            .set("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(90))
            .send_json(body);
        return match resp {
            Ok(r) => r.into_json::<Value>().map_err(|e| format!("响应解析失败：{e}")),
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                Err(format!("HTTP {code}：{}", text.chars().take(180).collect::<String>()))
            }
            Err(e) => Err(format!("网络错误：{e}")),
        };
    }
    let url = format!("{base}/chat/completions");
    let mut content = vec![json!({"type": "text", "text": prompt})];
    if let Some(img) = image {
        content.push(json!({"type": "image_url", "image_url": {"url": prepare_image(img)}}));
    }
    // 关掉"思考模式"：deepseek-flash / glm-5.x 默认先写一大段 reasoning_content，
    // 实测同一张章：开思考 8–60s（还常被 max_tokens 截断），关思考 **0.8s** 直接出 JSON。
    // 速度/精度开关：fast（默认）关思考，precise 开思考并给更大 token 预算
    let precise = cfg.get("speed").and_then(|v| v.as_str()) == Some("precise");
    let budget = if precise { max_tokens.max(4096) } else { max_tokens };
    let mut body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": content }],
        "max_tokens": budget,
        "temperature": 0
    });
    if !precise {
        body["thinking"] = json!({ "type": "disabled" });
    }
    let send = |b: &Value| {
        ureq::post(&url)
            .set("Authorization", &format!("Bearer {key}"))
            .set("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(120))
            .send_json(b.clone())
    };
    let resp = match send(&body) {
        Ok(r) => Ok(r),
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            // 不认 thinking 字段的服务商：去掉该字段重试一次
            if code == 400 && (text.contains("thinking") || text.contains("unknown") || text.contains("Unknown")) {
                if let Some(o) = body.as_object_mut() { o.remove("thinking"); }
                send(&body)
            } else {
                return Err(format!("HTTP {code}：{}", text.chars().take(180).collect::<String>()));
            }
        }
        Err(e) => Err(e),
    };
    match resp {
        Ok(r) => r.into_json::<Value>().map_err(|e| format!("响应解析失败：{e}")),
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            Err(format!("HTTP {code}：{}", text.chars().take(180).collect::<String>()))
        }
        Err(ureq::Error::Transport(e)) => Err(format!("网络错误：{e}")),
    }
}

/// Anthropic 走原始 base64 + media_type，不带 data: 前缀
fn prepare_image_raw(b64: &str) -> (String, String) {
    use base64::Engine as _;
    let raw = b64.trim();
    let payload = if let Some(i) = raw.find("base64,") { &raw[i + 7..] } else { raw };
    let bytes = match base64::engine::general_purpose::STANDARD.decode(payload.trim()) {
        Ok(b) => b,
        Err(_) => return ("image/png".into(), payload.to_string()),
    };
    let img = match image::load_from_memory(&bytes) { Ok(i) => i, Err(_) => return ("image/png".into(), payload.to_string()) };
    let (w, h) = (img.width(), img.height());
    let scaled = if w.max(h) > 1024 {
        let s = 1024.0 / w.max(h) as f32;
        img.resize((w as f32 * s) as u32, (h as f32 * s) as u32, image::imageops::FilterType::Triangle)
    } else { img };
    let mut buf = std::io::Cursor::new(Vec::new());
    if scaled.to_rgb8().write_to(&mut buf, image::ImageFormat::Jpeg).is_err() {
        return ("image/png".into(), payload.to_string());
    }
    ("image/jpeg".into(), base64::engine::general_purpose::STANDARD.encode(buf.into_inner()))
}

fn text_of(v: &Value) -> String {
    if let Some(m) = v.get("choices").and_then(|c| c.get(0)).and_then(|c| c.get("message")) {
        // 推理型模型（deepseek-flash / glm-5.x）可能把内容全放在 reasoning_content，
        // 而 content 为空——这时绝不能当成"模型什么都没说"
        let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("").trim().to_string();
        if !content.is_empty() { return content; }
        if let Some(r) = m.get("reasoning_content").and_then(|c| c.as_str()) {
            if !r.trim().is_empty() { return r.to_string(); }
        }
    }
    // Anthropic: {"content":[{"type":"text","text":"…"}]}
    if let Some(arr) = v.get("content").and_then(|c| c.as_array()) {
        let mut out = String::new();
        for blk in arr {
            if let Some(t) = blk.get("text").and_then(|x| x.as_str()) { out.push_str(t); }
        }
        if !out.is_empty() { return out; }
    }
    String::new()
}

/// 把上游的"模型不支持图片"类 400 翻成人话，避免用户看到 Unknown variant `image_url`

/// 把每次 AI 调用的关键信息落到 /tmp/stamp-ai.log，便于定位上游失败（用户只看得到一句"提取失败"）
/// 前端传来的是裸 base64（canvasToBase64 去了 data: 前缀）；OpenAI 兼容接口要求 data URI，
/// 而且 1200×1200 PNG 动辄 2MB，直接发既慢又容易超限 —— 统一缩到 ≤1024 并转 JPEG。
fn prepare_image(b64: &str) -> String {
    use base64::Engine as _;
    let raw = b64.trim();
    let payload = if let Some(i) = raw.find("base64,") { &raw[i + 7..] } else { raw };
    let bytes = match base64::engine::general_purpose::STANDARD.decode(payload.trim()) {
        Ok(b) => b,
        Err(_) => return format!("data:image/png;base64,{payload}"),
    };
    let img = match image::load_from_memory(&bytes) { Ok(i) => i, Err(_) => return format!("data:image/png;base64,{payload}") };
    let (w, h) = (img.width(), img.height());
    let scaled = if w.max(h) > 1024 {
        let s = 1024.0 / w.max(h) as f32;
        img.resize((w as f32 * s) as u32, (h as f32 * s) as u32, image::imageops::FilterType::Triangle)
    } else { img };
    let mut buf = std::io::Cursor::new(Vec::new());
    if scaled.to_rgb8().write_to(&mut buf, image::ImageFormat::Jpeg).is_err() {
        return format!("data:image/png;base64,{payload}");
    }
    format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(buf.into_inner()))
}

fn log_ai(tag: &str, cfg: &Value, detail: &str) {
    use std::io::Write;
    let line = format!(
        "[{}] {} base={} model={} mode={} :: {}\n",
        chrono_like_now(),
        tag,
        cfg.get("base_url").and_then(|v| v.as_str()).unwrap_or("-"),
        cfg.get("model").and_then(|v| v.as_str()).unwrap_or("-"),
        cfg.get("mode").and_then(|v| v.as_str()).unwrap_or("-"),
        detail.chars().take(400).collect::<String>()
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/stamp-ai.log") {
        let _ = f.write_all(line.as_bytes());
    }
}

fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("{}", secs)
}

fn friendly_ai_error(msg: &str) -> String {
    let low = msg.to_lowercase();
    if low.contains("unknown variant `image_url`")
        || low.contains("image_url")
        || low.contains("does not support image")
        || low.contains("not support image")
        || low.contains("multimodal")
    {
        return "当前模型不支持图像理解：DeepSeek 请用 deepseek-flash（deepseek-v4-pro / deepseek-chat 仅文本），或改用智谱 glm-4v-flash / 火山方舟豆包视觉。"
            .to_string();
    }
    if low.contains("model not found") || low.contains("invalid model") || low.contains("no such model") {
        return format!("模型名无效，请检查服务商与模型名（{msg}）");
    }
    if low.contains("insufficient") || low.contains("quota") || low.contains("balance") {
        return format!("账号额度不足或欠费（{msg}）");
    }
    msg.to_string()
}

fn extract_json(s: &str) -> Option<Value> {
    // 模型常见包装：```json 围栏、前后解释文字、尾逗号、多个 JSON 片段
    let mut txt = s.trim().to_string();
    if let Some(i) = txt.find("```") {
        let rest = &txt[i + 3..];
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        if let Some(j) = rest.find("```") {
            txt = rest[..j].trim().to_string();
        } else {
            txt = rest.trim().to_string();
        }
    }
    let a = txt.find('{')?;
    let b = txt.rfind('}')?;
    let core = txt[a..=b].trim_end_matches(',').to_string();
    if let Ok(v) = serde_json::from_str::<Value>(&core) { return Some(v); }
    // 去尾逗号再试一次：{"a":1,} / [1,2,]
    let cleaned = core.replace(",}", "}").replace(",]", "]");
    serde_json::from_str::<Value>(&cleaned).ok()
}

async fn api_ai_test(Json(body): Json<Value>) -> Json<Value> {
    let cfg = body.get("config").cloned().unwrap_or(Value::Null);
    let cfg_ref = cfg.clone();
    let out = tokio::task::spawn_blocking(move || ai_chat(&cfg, "ping", None, 8)).await
        .unwrap_or_else(|e| Err(format!("线程错误：{e}")));
    match out {
        Ok(v) => Json(json!({"success": true, "model": v.get("model").cloned().unwrap_or(Value::Null), "reply": text_of(&v)})),
        Err(e) => { log_ai("test", &cfg_ref, &e); Json(json!({"success": false, "error": friendly_ai_error(&e)})) }
    }
}

async fn api_ai_stamp(Json(body): Json<Value>) -> Json<Value> {
    let cfg = body.get("config").cloned().unwrap_or(Value::Null);
    let image = body.get("image").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if image.is_empty() { return Json(json!({"success": false, "error": "缺少 image"})); }
    let cfg_ref = cfg.clone();
    let out = tokio::task::spawn_blocking(move || {
        let prompt = "图中是一枚印章（照片可能倾斜、旋转或透视变形）。第一步先判断印章朝向：沿圆周读字，若文字是倒的或横躺的，在脑中把图片旋转 0/90/180/270 到文字正立的那一个角度；第二步按这个正立视角给出印章圆心与半径（相对图片宽高的 0~1 归一化，圆心取圆环/椭圆的几何中心，不要取墨迹质心）。只返回 JSON：{\"cx\":0.5,\"cy\":0.5,\"r\":0.3}";
        ai_chat(&cfg, prompt, Some(&image), 200)
    }).await.unwrap_or_else(|e| Err(format!("线程错误：{e}")));
    match out {
        Ok(v) => {
            log_ai("stamp", &cfg_ref, &format!("ok: {}", v.to_string().chars().take(200).collect::<String>()));
            let text = text_of(&v);
            match extract_json(&text) {
                Some(j) => Json(json!({"success": true, "stamp": {
                    "cx": j.get("cx").and_then(|x| x.as_f64()).unwrap_or(0.5),
                    "cy": j.get("cy").and_then(|x| x.as_f64()).unwrap_or(0.5),
                    "r":  j.get("r").and_then(|x| x.as_f64()).unwrap_or(0.3),
                    "raw": text
                }})),
                None => {
                    log_ai("stamp", &cfg_ref, &format!("no-json raw: {}", text.chars().take(400).collect::<String>()));
                    Json(json!({"success": false, "error": "AI 未返回可解析的 JSON", "raw": text}))
                }
            }
        }
        Err(e) => { log_ai("stamp", &cfg_ref, &e); Json(json!({"success": false, "error": friendly_ai_error(&e)})) }
    }
}

async fn api_ai_ocr(Json(body): Json<Value>) -> Json<Value> {
    let cfg = body.get("config").cloned().unwrap_or(Value::Null);
    let image = body.get("image").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let text_in = body.get("text").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let mode = cfg.get("mode").and_then(|v| v.as_str()).unwrap_or("vision").to_string();
    // 文本解析模式：只把本地 OCR 的粗文字发给模型，不塞图片（纯文本模型收图会返回空 content）
    if image.is_empty() && text_in.is_empty() { return Json(json!({"success": false, "error": "缺少 image 或 text"})); }
    if mode != "vision" && !text_in.is_empty() {
        let cfg_ref = cfg.clone();
        let model_name = cfg.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let body_text = text_in.clone();
        let out = tokio::task::spawn_blocking(move || {
            let prompt = format!(
                "下面是从印章图片上 OCR 出来的粗文字（可能有错字、乱序、缺字）：\n---\n{}\n---\n请清洗并结构化：机构名称取最完整的一家（去掉零散单字、编号、日期），编码取 8~13 位纯数字（没有就空），类型从 公司/银行/机关/委员会/事务所/其他 里选一个。只返回 JSON：{{\"company\":\"\",\"code\":\"\",\"type\":\"\"}}",
                body_text
            );
            ai_chat(&cfg, &prompt, None, 2048)
        }).await.unwrap_or_else(|e| Err(format!("线程错误：{e}")));
        return match out {
            Ok(v) => {
                let text = text_of(&v);
                match extract_json(&text) {
                    Some(j) => Json(json!({
                        "success": true, "mode": "parse", "model": model_name,
                        "ai": {
                            "company": j.get("company").and_then(|x| x.as_str()).unwrap_or(""),
                            "code": j.get("code").and_then(|x| x.as_str()).unwrap_or(""),
                            "type": j.get("type").and_then(|x| x.as_str()).unwrap_or(""),
                            "raw": text
                        }
                    })),
                    None => {
                        log_ai("ocr-parse", &cfg_ref, &format!("no-json raw: {}", text.chars().take(400).collect::<String>()));
                        Json(json!({"success": false, "error": "AI 未返回可解析的 JSON", "raw": text}))
                    }
                }
            }
            Err(e) => { log_ai("ocr-parse", &cfg_ref, &e); Json(json!({"success": false, "error": friendly_ai_error(&e)})) }
        };
    }
    let cfg_ref = cfg.clone();
    let model_name = cfg.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let out = tokio::task::spawn_blocking(move || {
        let prompt = "这张图是已经做过预处理的印章章面：背景已去掉（只剩墨迹）、外圈已归一为正方形、方向已尽量摆正。请沿外圈从正上方开始顺时针读机构全称，读下缘的编码（8~13 位纯数字，没有就留空），再判断类型（公司/银行/机关/委员会/事务所/其他）；被遮挡的字按机构命名常识补全。只返回 JSON：{\"company\":\"\",\"code\":\"\",\"type\":\"\"}（不要输出任何解释）。";
        ai_chat(&cfg, prompt, Some(&image), 2048)
    }).await.unwrap_or_else(|e| Err(format!("线程错误：{e}")));
    match out {
        Ok(v) => {
            let text = text_of(&v);
            match extract_json(&text) {
                Some(j) => Json(json!({
                    "success": true, "mode": "vision", "model": model_name,
                    "ai": {
                        "company": j.get("company").and_then(|x| x.as_str()).unwrap_or(""),
                        "code": j.get("code").and_then(|x| x.as_str()).unwrap_or(""),
                        "type": j.get("type").and_then(|x| x.as_str()).unwrap_or(""),
                        "raw": text
                    }
                })),
                None => {
                    let fr = v.get("choices").and_then(|c| c.get(0)).and_then(|c| c.get("finish_reason")).and_then(|x| x.as_str()).unwrap_or("");
                    log_ai("ocr", &cfg_ref, &format!("no-json finish={} raw: {}", fr, text.chars().take(400).collect::<String>()));
                    Json(json!({"success": false, "error": "AI 未返回可解析的 JSON", "raw": text}))
                }
            }
        }
        Err(e) => { log_ai("ocr", &cfg_ref, &e); Json(json!({"success": false, "error": friendly_ai_error(&e)})) }
    }
}
