//! Image generation through the relay (F10, D2): `POST /proxy/v1/images/generations` goes to
//! OpenAI with the relay's key and is metered per project like a model call. With
//! `ARBOS_IMAGE_MOCK=1` the relay answers itself with a labelled placeholder PNG so the whole
//! flow runs without a key.

use std::io::Cursor;
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use image::{ImageBuffer, Rgb};
use serde_json::{json, Value};

use crate::auth::Principal;
use crate::state::AppState;

const DEFAULT_MODEL: &str = "gpt-image-1";
const MAX_SIDE: u32 = 2048;

/// USD per image for the sizes and qualities OpenAI publishes; the model's `usage` block, when
/// present, is recorded as tokens beside it.
pub fn price(model: &str, size: &str, quality: &str) -> f64 {
    let m = model.to_ascii_lowercase();
    let tall_or_wide = size != "1024x1024" && size != "auto";
    if m.contains("dall-e-2") {
        return 0.02;
    }
    if m.contains("dall-e-3") {
        return match (quality, tall_or_wide) {
            ("hd", false) => 0.08,
            ("hd", true) => 0.12,
            (_, false) => 0.04,
            (_, true) => 0.08,
        };
    }
    match (quality, tall_or_wide) {
        ("low", false) => 0.011,
        ("low", true) => 0.016,
        ("high", false) => 0.167,
        ("high", true) => 0.25,
        // `auto` and `medium`
        (_, false) => 0.042,
        (_, true) => 0.063,
    }
}

fn parse_size(size: &str) -> (u32, u32) {
    let mut it = size.split('x');
    let w = it.next().and_then(|s| s.trim().parse::<u32>().ok());
    let h = it.next().and_then(|s| s.trim().parse::<u32>().ok());
    match (w, h) {
        (Some(w), Some(h)) if (64..=MAX_SIDE).contains(&w) && (64..=MAX_SIDE).contains(&h) => {
            (w, h)
        }
        _ => (1024, 1024),
    }
}

// ---- mock renderer -------------------------------------------------------------------------

/// 5×7 glyphs, one byte per row, low five bits used. Uppercase letters, digits, a few marks.
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x0C, 0x04, 0x08],
        ':' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00],
        '?' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04],
        '/' => [0x01, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10],
        _ => [0; 7],
    }
}

fn draw_text(img: &mut ImageBuffer<Rgb<u8>, Vec<u8>>, x0: u32, y0: u32, scale: u32, text: &str) {
    let (w, h) = img.dimensions();
    for (i, c) in text.chars().enumerate() {
        let g = glyph(c);
        let cx = x0 + i as u32 * 6 * scale;
        for (row, bits) in g.iter().enumerate() {
            for col in 0..5u32 {
                if bits & (0x10 >> col) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let x = cx + col * scale + dx;
                        let y = y0 + row as u32 * scale + dy;
                        if x < w && y < h {
                            img.put_pixel(x, y, Rgb([250, 250, 250]));
                        }
                    }
                }
            }
        }
    }
}

fn wrap(text: &str, cols: usize, max_lines: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let word: String = word.chars().take(cols).collect();
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > cols {
            lines.push(std::mem::take(&mut cur));
            if lines.len() == max_lines {
                break;
            }
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(&word);
    }
    if !cur.is_empty() && lines.len() < max_lines {
        lines.push(cur);
    }
    lines
}

/// A gradient tinted from the prompt's hash, a frame, "MOCK IMAGE", the prompt, and the size.
pub fn mock_png(prompt: &str, size: &str) -> Vec<u8> {
    let (w, h) = parse_size(size);
    let seed = prompt.bytes().fold(0x811c_9dc5u32, |a, b| {
        (a ^ b as u32).wrapping_mul(0x0100_0193)
    });
    let base = [
        40 + (seed & 0x3F) as u8,
        60 + ((seed >> 8) & 0x3F) as u8,
        90 + ((seed >> 16) & 0x3F) as u8,
    ];
    let mut img = ImageBuffer::from_fn(w, h, |x, y| {
        let fx = x as f32 / w as f32;
        let fy = y as f32 / h as f32;
        Rgb([
            (base[0] as f32 + 90.0 * fx) as u8,
            (base[1] as f32 + 70.0 * fy) as u8,
            (base[2] as f32 + 60.0 * (1.0 - fx)) as u8,
        ])
    });
    let margin = (w.min(h) / 24).max(8);
    for x in margin..w.saturating_sub(margin) {
        for t in 0..3 {
            img.put_pixel(x, margin + t, Rgb([250, 250, 250]));
            img.put_pixel(x, h - margin - 1 - t, Rgb([250, 250, 250]));
        }
    }
    for y in margin..h.saturating_sub(margin) {
        for t in 0..3 {
            img.put_pixel(margin + t, y, Rgb([250, 250, 250]));
            img.put_pixel(w - margin - 1 - t, y, Rgb([250, 250, 250]));
        }
    }
    let scale = (w / 160).clamp(2, 8);
    let inner = w.saturating_sub(4 * margin);
    let cols = (inner / (6 * scale)).max(8) as usize;
    let x0 = 2 * margin;
    draw_text(&mut img, x0, 2 * margin, scale + 1, "MOCK IMAGE");
    let mut y = 2 * margin + 7 * (scale + 1) + 3 * scale;
    for line in wrap(prompt, cols, 8) {
        draw_text(&mut img, x0, y, scale, &line);
        y += 9 * scale;
    }
    draw_text(
        &mut img,
        x0,
        h.saturating_sub(2 * margin + 7 * scale),
        scale,
        &format!("{w}X{h} ARBOS_IMAGE_MOCK=1"),
    );
    let mut out = Cursor::new(Vec::new());
    let _ = img.write_to(&mut out, image::ImageFormat::Png);
    out.into_inner()
}

// ---- handler -------------------------------------------------------------------------------

fn project_of(p: &Principal) -> Option<String> {
    match p {
        Principal::Box { project_id } => Some(project_id.clone()),
        Principal::Device { .. } | Principal::Admin => None,
    }
}

/// `POST /proxy/v1/images/generations` (OpenAI Images API shape; `b64_json` responses).
pub async fn generations(
    State(state): State<AppState>,
    principal: Principal,
    Json(mut body): Json<Value>,
) -> Response {
    let project_id = project_of(&principal).unwrap_or_else(|| "relay-admin".to_owned());
    let prompt = body
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    if prompt.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": { "message": "prompt is required" } })),
        )
            .into_response();
    }
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_MODEL)
        .to_owned();
    let size = body
        .get("size")
        .and_then(Value::as_str)
        .unwrap_or("1024x1024")
        .to_owned();
    let quality = body
        .get("quality")
        .and_then(Value::as_str)
        .unwrap_or("auto")
        .to_owned();
    let n = body
        .get("n")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .clamp(1, 4);
    body["n"] = json!(n);
    body["model"] = json!(model);

    let t0 = Instant::now();
    if state.cfg.image_mock {
        let data: Vec<Value> = (0..n)
            .map(|_| {
                json!({
                    "b64_json": base64::engine::general_purpose::STANDARD.encode(mock_png(&prompt, &size)),
                    "revised_prompt": prompt,
                })
            })
            .collect();
        let _ = state.db.record_usage(
            &project_id,
            "openai-images",
            &format!("mock:{model}"),
            0,
            0,
            0,
            0,
            0.0,
            t0.elapsed().as_millis() as i64,
            None,
        );
        return Json(json!({
            "created": arbos_core::ids::now_ms() / 1000,
            "data": data,
            "size": size,
            "arbos_mock": true,
        }))
        .into_response();
    }

    let Some(key) = state.cfg.openai_key.clone() else {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": { "message": "relay has no OPENAI_API_KEY for image generation (set ARBOS_IMAGE_MOCK=1 for placeholders)" } })),
        )
            .into_response();
    };
    if let Ok(Some(p)) = state.db.project(&project_id) {
        let today = state
            .db
            .usage_since(&project_id, crate::proxy::start_of_utc_day_ms())
            .map(|u| u.usd)
            .unwrap_or(0.0);
        if today >= p.spend_cap_daily {
            return (
                StatusCode::PAYMENT_REQUIRED,
                Json(json!({ "error": { "type": "spend_cap", "message": format!("daily spend cap ${:.2} reached", p.spend_cap_daily) } })),
            )
                .into_response();
        }
    }
    // gpt-image-* models only return base64; DALL·E needs to be asked for it.
    if model.starts_with("dall-e") {
        body["response_format"] = json!("b64_json");
    }
    let url = format!(
        "{}/images/generations",
        state.cfg.openai_images_base.trim_end_matches('/')
    );
    let upstream = match state
        .http
        .post(&url)
        .bearer_auth(&key)
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": { "message": format!("upstream: {e}") } })),
            )
                .into_response()
        }
    };
    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let bytes = upstream.bytes().await.unwrap_or_default();
    if status.is_success() {
        let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let (input, output) = parsed
            .get("usage")
            .map(|u| {
                (
                    u.get("input_tokens").and_then(Value::as_i64).unwrap_or(0),
                    u.get("output_tokens").and_then(Value::as_i64).unwrap_or(0),
                )
            })
            .unwrap_or((0, 0));
        let images = parsed
            .get("data")
            .and_then(Value::as_array)
            .map(|a| a.len() as f64)
            .unwrap_or(n as f64);
        let _ = state.db.record_usage(
            &project_id,
            "openai-images",
            &model,
            input,
            output,
            0,
            0,
            price(&model, &size, &quality) * images,
            t0.elapsed().as_millis() as i64,
            None,
        );
    }
    (
        status,
        [
            ("content-type", "application/json"),
            ("x-arbos-proxied", "1"),
        ],
        bytes,
    )
        .into_response()
}
