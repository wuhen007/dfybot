use anyhow::{Context, Result, bail};
use clap::Parser;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use reqwest::Client;
use serde::Deserialize;
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};

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
        Box::new(move |_cc| Ok(Box::new(PetApp::new(state)))),
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
    animation_time: f32,
    bubble_open: bool,
}

impl PetApp {
    fn new(state: Arc<RwLock<PetState>>) -> Self {
        Self {
            state,
            animation_time: 0.0,
            bubble_open: true,
        }
    }

    fn paint_fish(&self, painter: &egui::Painter, rect: Rect, state: &PetState) {
        let center = Pos2::new(rect.center().x, rect.top() + 207.0);
        let mood = state.mood.clamp(-5.0, 5.0);
        let body = if mood > 0.8 {
            Color32::from_rgb(69, 187, 157)
        } else if mood < -0.8 {
            Color32::from_rgb(103, 164, 177)
        } else {
            Color32::from_rgb(83, 190, 176)
        };
        let outline = Color32::from_rgb(34, 113, 112);
        let wag = self.animation_time.sin()
            * if state.activity == "thinking" {
                12.0
            } else {
                7.0
            };
        let body_rx = 83.0;
        let body_ry = 57.0;
        let tail_root = Pos2::new(center.x + 69.0, center.y + 4.0);
        let tail_tip = Pos2::new(center.x + 112.0, center.y + wag);
        let tail_upper = Pos2::new(center.x + 91.0, center.y - 29.0 + wag * 0.25);
        let tail_lower = Pos2::new(center.x + 91.0, center.y + 35.0 + wag * 0.25);
        painter.add(Shape::convex_polygon(
            vec![tail_root, tail_upper, tail_tip, tail_lower],
            Color32::from_rgb(238, 130, 115),
            Stroke::new(3.0, outline),
        ));

        let body_points: Vec<Pos2> = (0..48)
            .map(|step| {
                let angle = std::f32::consts::TAU * step as f32 / 48.0;
                Pos2::new(
                    center.x + body_rx * angle.cos(),
                    center.y + body_ry * angle.sin(),
                )
            })
            .collect();
        painter.add(Shape::convex_polygon(
            body_points,
            body,
            Stroke::new(3.0, outline),
        ));

        painter.add(Shape::convex_polygon(
            vec![
                Pos2::new(center.x - 4.0, center.y + 35.0),
                Pos2::new(center.x + 22.0, center.y + 53.0),
                Pos2::new(center.x + 41.0, center.y + 34.0),
            ],
            Color32::from_rgb(243, 197, 96),
            Stroke::new(2.0, outline),
        ));
        painter.add(Shape::convex_polygon(
            vec![
                Pos2::new(center.x - 24.0, center.y - 37.0),
                Pos2::new(center.x - 5.0, center.y - 56.0),
                Pos2::new(center.x + 14.0, center.y - 39.0),
            ],
            Color32::from_rgb(239, 151, 136),
            Stroke::new(2.0, outline),
        ));

        painter.circle_filled(
            Pos2::new(center.x - 34.0, center.y - 12.0),
            13.0,
            Color32::from_rgba_unmultiplied(255, 236, 197, 150),
        );
        painter.circle_filled(
            Pos2::new(center.x - 18.0, center.y - 5.0),
            4.8,
            Color32::WHITE,
        );
        painter.circle_filled(
            Pos2::new(center.x - 16.0, center.y - 5.0),
            2.5,
            Color32::from_rgb(38, 66, 72),
        );
        painter.circle_filled(
            Pos2::new(center.x - 29.0, center.y + 8.0),
            7.0,
            Color32::from_rgba_unmultiplied(239, 116, 126, 105),
        );
        painter.circle_filled(
            Pos2::new(center.x + 76.0, center.y - 49.0 + wag * 0.1),
            4.0,
            Color32::from_rgb(244, 197, 96),
        );
    }

    fn mood_line(state: &PetState) -> String {
        match state.activity.as_str() {
            "sleeping" => "正在休息".into(),
            "thinking" => "正在陪你聊天".into(),
            "offline" => "等服务器连接".into(),
            _ if !state.chat_enabled => "今天先安静陪伴".into(),
            _ if state.proactive_enabled => format!("{} · 会主动找你", state.mood_label),
            _ => format!("{} · 陪你待着", state.mood_label),
        }
    }
}

impl eframe::App for PetApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        let dt = ctx.input(|input| input.stable_dt).clamp(0.0, 0.1);
        self.animation_time = (self.animation_time + dt * 3.5) % std::f32::consts::TAU;
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
                        Self::mood_line(&state),
                        egui::FontId::proportional(12.5),
                        Color32::from_rgb(89, 112, 113),
                    );
                }
                self.paint_fish(&painter, rect, &state);
                painter.text(
                    Pos2::new(rect.right() - 13.0, rect.bottom() - 16.0),
                    egui::Align2::RIGHT_BOTTOM,
                    "右键退出 · 拖动移动",
                    egui::FontId::proportional(10.0),
                    Color32::from_rgb(101, 126, 124),
                );
            });
        ctx.request_repaint_after(Duration::from_millis(33));
    }
}
