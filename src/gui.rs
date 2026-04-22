use std::path::{Path, PathBuf};
use std::thread;

use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, RichText, Stroke};

use crate::cli::{CopyOpts, VerifyOpts};
use crate::progress::{LogLevel, ProgressEvent, ProgressPhase, ProgressReporter};
use crate::{copy, verify};

pub fn run() -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([600.0, 500.0])
            .with_min_inner_size([560.0, 440.0]),
        ..Default::default()
    };

    eframe::run_native(
        "SafeCopy",
        options,
        Box::new(|cc| {
            configure_fonts(&cc.egui_ctx);
            cc.egui_ctx.set_visuals(egui::Visuals::light());
            Box::<SafeCopyApp>::default()
        }),
    )
    .map_err(|e| anyhow::anyhow!("ошибка запуска GUI: {e}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppState {
    Idle,
    Preparing,
    Copying,
    Cooldown { seconds_left: u64 },
    Verifying,
    Done,
    Failed,
}

#[derive(Debug, Clone)]
struct LogLine {
    level: LogLevel,
    message: String,
}

enum GuiMessage {
    Progress(ProgressEvent),
    Done(Result<(), String>),
}

struct GuiReporter {
    tx: Sender<GuiMessage>,
    ctx: egui::Context,
}

impl ProgressReporter for GuiReporter {
    fn report(&self, event: ProgressEvent) {
        let _ = self.tx.send(GuiMessage::Progress(event));
        self.ctx.request_repaint();
    }
}

pub struct SafeCopyApp {
    state: AppState,
    source_dir: Option<PathBuf>,
    dest_dir: Option<PathBuf>,
    cooldown_secs: u64,
    max_retries: u32,
    no_manifest_on_card: bool,
    current_file: String,
    total_bytes: u64,
    completed_bytes: u64,
    logs: Vec<LogLine>,
    rx: Option<Receiver<GuiMessage>>,
}

impl Default for SafeCopyApp {
    fn default() -> Self {
        Self {
            state: AppState::Idle,
            source_dir: None,
            dest_dir: None,
            cooldown_secs: 45,
            max_retries: 3,
            no_manifest_on_card: false,
            current_file: String::new(),
            total_bytes: 0,
            completed_bytes: 0,
            logs: Vec::new(),
            rx: None,
        }
    }
}

impl eframe::App for SafeCopyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_messages();

        egui::CentralPanel::default().show(ctx, |ui| {
            self.draw_inputs(ui);
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(10.0);

            self.draw_actions(ui, ctx);
            ui.add_space(10.0);

            ui.separator();
            ui.add_space(8.0);
            self.draw_progress(ui);
        });
    }
}

impl SafeCopyApp {
    fn drain_messages(&mut self) {
        let Some(rx) = self.rx.clone() else {
            return;
        };

        while let Ok(message) = rx.try_recv() {
            match message {
                GuiMessage::Progress(event) => self.handle_progress_event(event),
                GuiMessage::Done(result) => {
                    self.rx = None;
                    self.current_file.clear();
                    match result {
                        Ok(()) => {
                            self.state = AppState::Done;
                            self.push_log(LogLevel::Success, "Операция завершена успешно");
                        }
                        Err(e) => {
                            self.state = AppState::Failed;
                            self.push_log(LogLevel::Error, format!("Операция прервана: {e}"));
                        }
                    }
                }
            }
        }
    }

    fn handle_progress_event(&mut self, event: ProgressEvent) {
        match event {
            ProgressEvent::Phase(phase) => self.set_phase(phase),
            ProgressEvent::TotalBytes(bytes) => {
                self.total_bytes = bytes;
                self.completed_bytes = 0;
            }
            ProgressEvent::BytesAdvanced(bytes) => {
                self.completed_bytes = self.completed_bytes.saturating_add(bytes);
                if self.total_bytes > 0 {
                    self.completed_bytes = self.completed_bytes.min(self.total_bytes);
                }
            }
            ProgressEvent::CurrentFile(path) => self.current_file = path,
            ProgressEvent::CooldownLeft(seconds_left) => {
                self.state = AppState::Cooldown { seconds_left };
                self.current_file.clear();
            }
            ProgressEvent::Log { level, message } => self.push_log(level, message),
        }
    }

    fn set_phase(&mut self, phase: ProgressPhase) {
        self.state = match phase {
            ProgressPhase::Sanity | ProgressPhase::Scanning => AppState::Preparing,
            ProgressPhase::Copying => AppState::Copying,
            ProgressPhase::Cooldown => AppState::Cooldown {
                seconds_left: self.cooldown_secs,
            },
            ProgressPhase::Verifying => AppState::Verifying,
            ProgressPhase::Done => AppState::Done,
        };
    }

    fn draw_inputs(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("path_grid")
            .num_columns(3)
            .spacing([8.0, 8.0])
            .striped(false)
            .show(ui, |ui| {
                ui.label("Источник:");
                draw_path_field(ui, self.source_dir.as_deref());
                if ui.button("Выбрать...").clicked() {
                    self.source_dir = rfd::FileDialog::new()
                        .set_title("Выберите исходную папку")
                        .pick_folder();
                }
                ui.end_row();

                ui.label("SD-карта:");
                draw_path_field(ui, self.dest_dir.as_deref());
                if ui.button("Выбрать...").clicked() {
                    self.dest_dir = rfd::FileDialog::new()
                        .set_title("Выберите папку на SD-карте")
                        .pick_folder();
                }
                ui.end_row();
            });

        egui::CollapsingHeader::new("Настройки")
            .default_open(false)
            .show(ui, |ui| {
                ui.add(egui::Slider::new(&mut self.cooldown_secs, 0..=120).text("Cooldown, сек"));
                ui.add(egui::Slider::new(&mut self.max_retries, 1..=10).text("Попытки на файл"));
                ui.checkbox(&mut self.no_manifest_on_card, "Без манифеста на карте");
            });
    }

    fn draw_actions(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let busy = self.is_busy();
        let can_copy = !busy && self.source_dir.is_some() && self.dest_dir.is_some();
        let can_verify = !busy && self.dest_dir.is_some();

        ui.horizontal(|ui| {
            let start_button = egui::Button::new(
                RichText::new("НАЧАТЬ КОПИРОВАНИЕ")
                    .strong()
                    .color(Color32::WHITE),
            )
            .fill(Color32::from_rgb(28, 132, 76))
            .min_size(egui::vec2(260.0, 48.0));

            if ui.add_enabled(can_copy, start_button).clicked() {
                self.start_copy_thread(ctx.clone());
            }

            let verify_button =
                egui::Button::new("Только проверка").min_size(egui::vec2(190.0, 36.0));
            if ui.add_enabled(can_verify, verify_button).clicked() {
                self.start_verify_thread(ctx.clone());
            }
        });

        if !busy && !can_copy {
            ui.add_space(6.0);
            ui.label(
                RichText::new("Выберите источник и SD-карту для копирования")
                    .color(Color32::from_gray(90)),
            );
        }
    }

    fn draw_progress(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new(self.status_text()).strong());
        if !self.current_file.is_empty() {
            ui.label(format!("Текущий файл: {}", self.current_file));
        }

        if self.total_bytes > 0 || self.is_busy() {
            let fraction = if self.total_bytes == 0 {
                0.0
            } else {
                progress_fraction(self.completed_bytes, self.total_bytes)
            };
            let progress_text = format!(
                "{} / {}",
                format_bytes(self.completed_bytes),
                format_bytes(self.total_bytes)
            );
            ui.add(egui::ProgressBar::new(fraction).text(progress_text));
        }

        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .max_height(150.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if self.logs.is_empty() {
                    ui.colored_label(Color32::from_gray(120), "Журнал появится после запуска");
                } else {
                    for line in &self.logs {
                        ui.colored_label(color_for(line.level), &line.message);
                    }
                }
            });
    }

    fn start_copy_thread(&mut self, ctx: egui::Context) {
        let (Some(source), Some(destination)) = (self.source_dir.clone(), self.dest_dir.clone())
        else {
            return;
        };

        self.prepare_run(AppState::Preparing);
        let opts = CopyOpts {
            source,
            destination,
            cooldown_secs: self.cooldown_secs,
            no_manifest_on_card: self.no_manifest_on_card,
            max_retries: self.max_retries,
        };
        self.spawn_worker(ctx, move |reporter| {
            copy::run_with_reporter(&opts, reporter)
        });
    }

    fn start_verify_thread(&mut self, ctx: egui::Context) {
        let Some(target) = self.dest_dir.clone() else {
            return;
        };

        self.prepare_run(AppState::Verifying);
        let opts = VerifyOpts { target };
        self.spawn_worker(ctx, move |reporter| {
            verify::run_with_reporter(&opts, reporter)
        });
    }

    fn spawn_worker<F>(&mut self, ctx: egui::Context, run: F)
    where
        F: FnOnce(&dyn ProgressReporter) -> anyhow::Result<()> + Send + 'static,
    {
        let (tx, rx) = unbounded();
        self.rx = Some(rx);

        let done_tx = tx.clone();
        let done_ctx = ctx.clone();
        thread::spawn(move || {
            let reporter = GuiReporter { tx, ctx };
            let result = run(&reporter).map_err(|e| e.to_string());
            let _ = done_tx.send(GuiMessage::Done(result));
            done_ctx.request_repaint();
        });
    }

    fn prepare_run(&mut self, state: AppState) {
        self.state = state;
        self.current_file.clear();
        self.total_bytes = 0;
        self.completed_bytes = 0;
        self.logs.clear();
    }

    fn push_log(&mut self, level: LogLevel, message: impl Into<String>) {
        self.logs.push(LogLine {
            level,
            message: message.into(),
        });
        if self.logs.len() > 1_000 {
            let overflow = self.logs.len() - 1_000;
            self.logs.drain(0..overflow);
        }
    }

    fn is_busy(&self) -> bool {
        matches!(
            self.state,
            AppState::Preparing
                | AppState::Copying
                | AppState::Cooldown { .. }
                | AppState::Verifying
        )
    }

    fn status_text(&self) -> String {
        match self.state {
            AppState::Idle => String::from("Стадия: ожидание выбора папок"),
            AppState::Preparing => String::from("Стадия: подготовка"),
            AppState::Copying => String::from("Стадия: копирование"),
            AppState::Cooldown { seconds_left } => {
                format!("Стадия: остывание, осталось {seconds_left} сек")
            }
            AppState::Verifying => String::from("Стадия: проверка"),
            AppState::Done => String::from("Стадия: готово"),
            AppState::Failed => String::from("Стадия: ошибка"),
        }
    }
}

fn draw_path_field(ui: &mut egui::Ui, path: Option<&Path>) {
    let text = path.map_or_else(|| String::from("Не выбрано"), |p| p.display().to_string());
    let color = if path.is_some() {
        ui.visuals().text_color()
    } else {
        Color32::from_gray(150)
    };

    egui::Frame::none()
        .fill(ui.visuals().extreme_bg_color)
        .stroke(Stroke::new(
            1.0,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
        .rounding(egui::Rounding::same(4.0))
        .inner_margin(egui::Margin::symmetric(8.0, 5.0))
        .show(ui, |ui| {
            ui.set_min_width(360.0);
            ui.set_max_width(360.0);
            ui.label(RichText::new(text).color(color).monospace());
        });
}

fn configure_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        fonts
            .font_data
            .insert("segoe-ui".to_owned(), FontData::from_owned(bytes));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, "segoe-ui".to_owned());
        }
    }
    ctx.set_fonts(fonts);
}

fn color_for(level: LogLevel) -> Color32 {
    match level {
        LogLevel::Info => Color32::from_rgb(75, 82, 92),
        LogLevel::Success => Color32::from_rgb(24, 132, 65),
        LogLevel::Warning | LogLevel::Retry => Color32::from_rgb(168, 112, 0),
        LogLevel::Error | LogLevel::Quarantine => Color32::from_rgb(190, 42, 42),
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut divisor = 1u64;
    let mut unit = 0usize;
    while bytes / divisor >= 1024 && unit + 1 < UNITS.len() {
        divisor *= 1024;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        let whole = bytes / divisor;
        let decimal = (bytes % divisor) * 10 / divisor;
        format!("{whole}.{decimal} {}", UNITS[unit])
    }
}

#[allow(clippy::cast_precision_loss)]
fn progress_fraction(completed: u64, total: u64) -> f32 {
    (completed as f32 / total as f32).clamp(0.0, 1.0)
}
