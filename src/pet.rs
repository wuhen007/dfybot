#![cfg_attr(all(target_os = "windows", not(test)), windows_subsystem = "windows")]

use anyhow::{Context, Result, bail};
use clap::Parser;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, TextureHandle, Vec2};
use reqwest::Client;
use serde::Deserialize;
use std::{
    io::Cursor,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

const FRAME_RATE: f64 = 24.0;
const FRAME_SIZE: [usize; 2] = [216, 232];

#[derive(Parser)]
#[command(
    name = "aurabot-pet",
    about = "Optional floating desktop pet for Aurabot"
)]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:8788")]
    server: String,
    #[arg(long, default_value = "AURABOT_ADMIN_TOKEN")]
    token_env: String,
    #[arg(long, default_value = "data/admin.token")]
    token_file: PathBuf,
    /// Directory containing the six Shenshen state animation folders.
    #[arg(long)]
    assets_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
struct PetState {
    name: String,
    activity: String,
    mood: f32,
    mood_label: String,
    owner_sessions: u32,
    chat_enabled: bool,
    proactive_enabled: bool,
    connected: bool,
}

impl Default for PetState {
    fn default() -> Self {
        Self {
            name: "大肥鱼".into(),
            activity: "offline".into(),
            mood: 0.0,
            mood_label: "平静".into(),
            owner_sessions: 0,
            chat_enabled: false,
            proactive_enabled: false,
            connected: false,
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let url = reqwest::Url::parse(&args.server)?;
    if !["http", "https"].contains(&url.scheme()) {
        bail!("server URL must use HTTP or HTTPS")
    }
    let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1"));
    if url.scheme() != "https" && !local {
        bail!("remote pet connections require HTTPS")
    }
    let token = std::env::var(&args.token_env)
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| std::fs::read_to_string(&args.token_file).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| value.len() >= 24)
        .context("set the management token environment variable or provide --token-file")?;
    let state = Arc::new(RwLock::new(PetState::default()));
    let assets_dir = asset_directory(args.assets_dir)?;
    let app = PetApp::new(state.clone(), assets_dir)?;
    spawn_poll(args.server, token, state.clone())?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Aurabot Pet")
            .with_app_id("aurabot-pet")
            .with_inner_size([276.0, 320.0])
            .with_min_inner_size([240.0, 280.0])
            .with_max_inner_size([276.0, 320.0])
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_has_shadow(false)
            .with_always_on_top()
            .with_taskbar(false),
        renderer: eframe::Renderer::Glow,
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        ..Default::default()
    };
    eframe::run_native(
        "Aurabot Pet",
        options,
        Box::new(move |_cc| Ok(Box::new(app))),
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn spawn_poll(server: String, token: String, state: Arc<RwLock<PetState>>) -> Result<()> {
    let client = Client::builder()
        .timeout(Duration::from_secs(4))
        .pool_max_idle_per_host(1)
        .build()?;
    std::thread::Builder::new()
        .name("aurabot-pet-status".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(_) => return,
            };
            runtime.block_on(async move {
                let endpoint = format!("{}/api/pet/state", server.trim_end_matches('/'));
                loop {
                    let result = client
                        .get(&endpoint)
                        .bearer_auth(&token)
                        .send()
                        .await
                        .and_then(reqwest::Response::error_for_status);
                    let next = match result {
                        Ok(response) => match response.json::<PetState>().await {
                            Ok(mut data) => {
                                data.connected = true;
                                Some(data)
                            }
                            Err(_) => None,
                        },
                        Err(_) => None,
                    };
                    if let Ok(mut current) = state.write() {
                        if let Some(next) = next {
                            *current = next;
                        } else {
                            current.connected = false;
                            current.activity = "offline".into();
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            });
        })?;
    Ok(())
}

struct PetApp {
    state: Arc<RwLock<PetState>>,
    assets_dir: PathBuf,
    animation_started: Instant,
    animation: String,
    frames: Vec<Vec<u8>>,
    texture: Option<TextureHandle>,
    rendered_frame: Option<usize>,
    asset_error: bool,
    bubble_open: bool,
}

impl PetApp {
    fn new(state: Arc<RwLock<PetState>>, assets_dir: PathBuf) -> Result<Self> {
        let frames = load_frames(&assets_dir, "offline")?;
        decode_frame(&frames[0])?;
        Ok(Self {
            state,
            assets_dir,
            animation_started: Instant::now(),
            animation: "offline".into(),
            frames,
            texture: None,
            rendered_frame: None,
            asset_error: false,
            bubble_open: true,
        })
    }

    fn paint_character(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        rect: Rect,
        state: &PetState,
    ) {
        let animation = animation_for(state);
        if self.animation != animation {
            self.frames.clear();
            self.frames = match load_frames(&self.assets_dir, animation) {
                Ok(frames) => {
                    self.asset_error = false;
                    frames
                }
                Err(error) => {
                    eprintln!("Failed to load {animation}: {error:#}");
                    self.asset_error = true;
                    Vec::new()
                }
            };
            self.animation = animation.to_owned();
            self.animation_started = Instant::now();
            self.rendered_frame = None;
        }
        let index = (self.animation_started.elapsed().as_secs_f64() * FRAME_RATE) as usize
            % self.frames.len().max(1);
        let Some(frame) = self.frames.get(index) else {
            return;
        };
        if self.rendered_frame != Some(index) {
            let image = match decode_frame(frame) {
                Ok(image) => image,
                Err(error) => {
                    eprintln!("Failed to decode {animation} frame {index}: {error:#}");
                    self.asset_error = true;
                    return;
                }
            };
            if let Some(texture) = self.texture.as_mut() {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                self.texture =
                    Some(ctx.load_texture("aurabot-pet", image, egui::TextureOptions::LINEAR));
            }
            self.rendered_frame = Some(index);
        }
        if let Some(texture) = &self.texture {
            painter.image(
                texture.id(),
                Rect::from_center_size(
                    Pos2::new(rect.center().x, rect.top() + 196.0),
                    Vec2::new(FRAME_SIZE[0] as f32, FRAME_SIZE[1] as f32),
                ),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    }

    fn mood_line(state: &PetState) -> String {
        if !state.connected || state.activity == "offline" {
            return "等服务器连接".into();
        }
        if !state.chat_enabled {
            return "今天先安静陪伴".into();
        }
        match state.activity.as_str() {
            "sleeping" => "正在休息".into(),
            "thinking" => "正在陪你聊天".into(),
            _ if state.proactive_enabled => format!("{} · 会主动找你", state.mood_label),
            _ => format!("{} · 陪你待着", state.mood_label),
        }
    }
}

fn animation_for(state: &PetState) -> &'static str {
    if !state.connected {
        "offline"
    } else if !state.chat_enabled {
        "waiting"
    } else {
        match state.activity.as_str() {
            "sleeping" => "sleeping",
            "thinking" => "thinking",
            "offline" => "offline",
            _ if state.mood > 0.8 => "happy",
            _ => "idle",
        }
    }
}

fn asset_directory(requested: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(directory) = requested {
        return Ok(directory);
    }
    let executable = std::env::current_exe()?;
    let adjacent = executable
        .parent()
        .context("executable has no parent")?
        .join("assets/shenshen");
    if adjacent.is_dir() {
        return Ok(adjacent);
    }
    Ok(std::env::current_dir()?.join("assets/shenshen"))
}

fn load_frames(root: &std::path::Path, animation: &str) -> Result<Vec<Vec<u8>>> {
    #[derive(Deserialize)]
    struct Meta {
        frames: usize,
        fps: f64,
        size: [usize; 2],
    }
    let directory = root.join(animation);
    let meta: Meta = serde_json::from_slice(
        &std::fs::read(directory.join("meta.json")).with_context(|| {
            format!(
                "missing animation metadata in {}; use --assets-dir",
                directory.display()
            )
        })?,
    )?;
    if !(1..=1000).contains(&meta.frames) || meta.fps != FRAME_RATE || meta.size != FRAME_SIZE {
        bail!("unsupported animation metadata in {}", directory.display());
    }
    (1..=meta.frames)
        .map(|index| {
            let path = directory.join(format!("f_{index:04}.png"));
            std::fs::read(&path).with_context(|| format!("cannot read {}", path.display()))
        })
        .collect()
}

fn decode_frame(bytes: &[u8]) -> Result<egui::ColorImage> {
    let mut reader = png::Decoder::new(Cursor::new(bytes)).read_info()?;
    let header = reader.info();
    if [header.width as usize, header.height as usize] != FRAME_SIZE
        || header.color_type != png::ColorType::Rgba
        || header.bit_depth != png::BitDepth::Eight
    {
        bail!("expected a 216x232 RGBA8 PNG frame");
    }
    let size = reader
        .output_buffer_size()
        .context("PNG frame exceeds the supported output size")?;
    let mut buffer = vec![0; size];
    let info = reader.next_frame(&mut buffer)?;
    let size = [info.width as usize, info.height as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        size,
        &buffer[..info.buffer_size()],
    ))
}

impl eframe::App for PetApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        let state = self.state.read().map(|s| s.clone()).unwrap_or_default();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(root, |ui| {
                let (rect, response) =
                    ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
                if response.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                if response.clicked() {
                    self.bubble_open = !self.bubble_open;
                }
                response.context_menu(|ui| {
                    if ui.button("退出桌宠").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        ui.close();
                    }
                });
                let painter = ui.painter_at(rect);
                self.paint_character(&ctx, &painter, rect, &state);
                if self.bubble_open {
                    let bubble = Rect::from_min_size(
                        Pos2::new(rect.left() + 13.0, rect.top() + 16.0),
                        Vec2::new(rect.width() - 26.0, 63.0),
                    );
                    painter.rect_filled(
                        bubble,
                        15.0,
                        Color32::from_rgba_unmultiplied(255, 255, 255, 236),
                    );
                    painter.rect_stroke(
                        bubble,
                        15.0,
                        Stroke::new(1.0, Color32::from_rgb(205, 226, 220)),
                        egui::StrokeKind::Inside,
                    );
                    painter.text(
                        Pos2::new(bubble.left() + 13.0, bubble.top() + 10.0),
                        egui::Align2::LEFT_TOP,
                        &state.name,
                        egui::FontId::proportional(15.0),
                        Color32::from_rgb(38, 93, 87),
                    );
                    painter.text(
                        Pos2::new(bubble.left() + 13.0, bubble.top() + 34.0),
                        egui::Align2::LEFT_TOP,
                        if self.asset_error {
                            "素材加载失败，请检查资源目录".into()
                        } else {
                            Self::mood_line(&state)
                        },
                        egui::FontId::proportional(12.5),
                        Color32::from_rgb(89, 112, 113),
                    );
                }
                painter.text(
                    Pos2::new(rect.right() - 13.0, rect.bottom() - 16.0),
                    egui::Align2::RIGHT_BOTTOM,
                    "右键退出 · 拖动移动",
                    egui::FontId::proportional(10.0),
                    Color32::from_rgb(101, 126, 124),
                );
            });
        let interval = 1.0 / FRAME_RATE;
        let remaining = interval - self.animation_started.elapsed().as_secs_f64() % interval;
        ctx.request_repaint_after(Duration::from_secs_f64(remaining));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animation_matches_connection_and_bot_state() {
        let mut state = PetState::default();
        assert_eq!(animation_for(&state), "offline");
        state.connected = true;
        assert_eq!(animation_for(&state), "waiting");
        state.chat_enabled = true;
        state.activity = "idle".into();
        assert_eq!(animation_for(&state), "idle");
        state.activity = "sleeping".into();
        assert_eq!(animation_for(&state), "sleeping");
        state.activity = "thinking".into();
        assert_eq!(animation_for(&state), "thinking");
        state.activity = "idle".into();
        state.mood = 1.0;
        assert_eq!(animation_for(&state), "happy");
    }

    #[test]
    fn selected_animation_frames_decode() {
        for name in [
            "idle", "sleeping", "thinking", "offline", "happy", "waiting",
        ] {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/shenshen");
            let frames = load_frames(&root, name).expect("complete animation");
            for frame in frames {
                let decoded = decode_frame(&frame).expect("valid PNG");
                assert_eq!(decoded.size, FRAME_SIZE);
            }
        }
    }

    #[test]
    fn missing_frames_are_reported_instead_of_truncating_animation() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("offline");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(
            directory.join("meta.json"),
            r#"{"frames":2,"fps":24,"size":[216,232]}"#,
        )
        .unwrap();
        std::fs::write(directory.join("f_0001.png"), b"placeholder").unwrap();
        assert!(
            load_frames(root.path(), "offline")
                .unwrap_err()
                .to_string()
                .contains("f_0002.png")
        );
    }

    #[test]
    fn malformed_frames_are_rejected() {
        assert!(decode_frame(b"not a PNG").is_err());
    }
}
