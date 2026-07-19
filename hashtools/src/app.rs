#![allow(clippy::too_many_lines)]

use crate::worker::{self, Item, Msg, Summary};
use egui::{Color32, Context, Frame, ProgressBar, RichText, Stroke, Ui, Vec2};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Mode {
    #[default]
    Generate,
    Check,
}

struct Progress {
    done: usize,
    total: usize,
    current_file: String,
}

#[derive(Default)]
enum RunState {
    #[default]
    Idle,
    Running {
        rx: mpsc::Receiver<Msg>,
        cancel: Arc<AtomicBool>,
        progress: Progress,
    },
    Done {
        summary: Summary,
        items: Vec<Item>,
    },
}

#[derive(Default)]
pub struct App {
    folder: Option<PathBuf>,
    mode: Mode,
    state: RunState,
    pending_items: Vec<Item>,
}

impl App {
    fn start(&mut self, ctx: &Context) {
        let Some(dir) = self.folder.clone() else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();

        match self.mode {
            Mode::Generate => {
                worker::spawn_generate(dir, Arc::clone(&cancel), tx, ctx.clone());
            }
            Mode::Check => {
                worker::spawn_check(dir, Arc::clone(&cancel), tx, ctx.clone());
            }
        }

        self.pending_items.clear();
        self.state = RunState::Running {
            rx,
            cancel,
            progress: Progress {
                done: 0,
                total: 0,
                current_file: String::new(),
            },
        };
    }

    fn cancel(&mut self) {
        if let RunState::Running { cancel, .. } = &self.state {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    fn drain(&mut self) {
        #[allow(clippy::while_let_loop)]
        loop {
            let msg = match &self.state {
                RunState::Running { rx, .. } => match rx.try_recv() {
                    Ok(m) => m,
                    Err(_) => break,
                },
                _ => break,
            };

            match msg {
                Msg::Planned { total } => {
                    if let RunState::Running { progress, .. } = &mut self.state {
                        progress.total = total;
                    }
                }
                Msg::Progress {
                    done,
                    total,
                    filename,
                } => {
                    if let RunState::Running { progress, .. } = &mut self.state {
                        progress.done = done;
                        progress.total = total;
                        progress.current_file = filename;
                    }
                }
                Msg::Item(item) => {
                    self.pending_items.push(item);
                }
                Msg::Finished(result) => {
                    let summary = result.unwrap_or_else(|error| Summary {
                        error: Some(error),
                        hash_errors: 1,
                        ..Default::default()
                    });
                    let items = std::mem::take(&mut self.pending_items);
                    self.state = RunState::Done { summary, items };
                    break;
                }
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // Drag-and-drop
        if !matches!(self.state, RunState::Running { .. }) {
            let dropped: Option<PathBuf> = ctx.input(|i| {
                i.raw
                    .dropped_files
                    .iter()
                    .find_map(|f| f.path.as_ref().filter(|p| p.is_dir()).cloned())
            });
            if let Some(path) = dropped {
                self.folder = Some(path);
                self.state = RunState::Idle;
                self.pending_items.clear();
            }
        }

        if matches!(self.state, RunState::Running { .. }) {
            self.drain();
            ctx.request_repaint();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            draw_drop_zone(ui, ctx, self.folder.as_ref(), &self.state);
            ui.add_space(8.0);
            draw_folder_row(
                ui,
                &mut self.folder,
                &mut self.state,
                &mut self.pending_items,
            );
            ui.add_space(12.0);
            draw_mode_selector(ui, &mut self.mode, &self.state);
            ui.add_space(12.0);
            draw_controls(ui, ctx, self);

            if let RunState::Running { progress, .. } = &self.state {
                ui.add_space(8.0);
                draw_progress(ui, progress);
            }

            if let RunState::Done { summary, items } = &self.state {
                ui.add_space(12.0);
                draw_summary(ui, summary, items);
            }
        });
    }
}

fn draw_drop_zone(ui: &mut Ui, ctx: &Context, folder: Option<&PathBuf>, state: &RunState) {
    let is_hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
    let is_running = matches!(state, RunState::Running { .. });

    let border_color = if is_hovering {
        Color32::from_rgb(100, 160, 255)
    } else {
        ui.visuals().widgets.noninteractive.bg_stroke.color
    };
    let bg_color = if is_hovering {
        Color32::from_rgba_premultiplied(100, 160, 255, 20)
    } else {
        ui.visuals().extreme_bg_color
    };

    Frame::new()
        .fill(bg_color)
        .stroke(Stroke::new(1.5_f32, border_color))
        .inner_margin(16.0)
        .corner_radius(6.0)
        .show(ui, |ui| {
            ui.set_min_size(Vec2::new(ui.available_width(), 80.0));
            ui.vertical_centered(|ui| {
                if is_running {
                    ui.label(RichText::new("Running…").color(ui.visuals().weak_text_color()));
                } else if folder.is_some() {
                    ui.label(
                        RichText::new("Drop another folder here to replace")
                            .color(ui.visuals().weak_text_color()),
                    );
                } else {
                    ui.label(RichText::new("Drop folder here").size(16.0));
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("or use the Browse button below")
                            .color(ui.visuals().weak_text_color()),
                    );
                }
            });
        });
}

fn draw_folder_row(
    ui: &mut Ui,
    folder: &mut Option<PathBuf>,
    state: &mut RunState,
    pending_items: &mut Vec<Item>,
) {
    let is_running = matches!(state, RunState::Running { .. });
    ui.horizontal(|ui| {
        let label = folder.as_deref().map_or_else(
            || "No folder selected".to_string(),
            |p| p.to_string_lossy().into_owned(),
        );
        ui.label(RichText::new(&label).monospace());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(
                    !is_running,
                    egui::Button::new(RichText::new("Browse…").size(15.0))
                        .min_size(Vec2::new(0.0, 32.0)),
                )
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    *folder = Some(path);
                    *state = RunState::Idle;
                    pending_items.clear();
                }
            }
        });
    });
}

fn draw_mode_selector(ui: &mut Ui, mode: &mut Mode, state: &RunState) {
    let enabled = !matches!(state, RunState::Running { .. });
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            if ui
                .selectable_label(
                    *mode == Mode::Generate,
                    RichText::new("  Generate  ").size(15.0),
                )
                .clicked()
            {
                *mode = Mode::Generate;
            }
            if ui
                .selectable_label(*mode == Mode::Check, RichText::new("  Check  ").size(15.0))
                .clicked()
            {
                *mode = Mode::Check;
            }
        });
    });
}

fn draw_controls(ui: &mut Ui, ctx: &Context, app: &mut App) {
    const BUTTON_SIZE: Vec2 = Vec2::new(120.0, 36.0);

    ui.horizontal(|ui| {
        ui.add_space(((ui.available_width() - BUTTON_SIZE.x) / 2.0).max(0.0));

        if let RunState::Running { .. } = &app.state {
            if ui
                .add(
                    egui::Button::new(
                        RichText::new("Cancel")
                            .size(16.0)
                            .color(Color32::from_rgb(220, 80, 80)),
                    )
                    .min_size(BUTTON_SIZE),
                )
                .clicked()
            {
                app.cancel();
            }
        } else {
            let can_run = app.folder.is_some();
            if ui
                .add_enabled(
                    can_run,
                    egui::Button::new(RichText::new("Run").size(16.0)).min_size(BUTTON_SIZE),
                )
                .clicked()
            {
                app.start(ctx);
            }
        }
    });
}

fn draw_progress(ui: &mut Ui, progress: &Progress) {
    #[allow(clippy::cast_precision_loss)]
    let fraction = if progress.total == 0 {
        0.0_f32
    } else {
        progress.done as f32 / progress.total as f32
    };
    ui.add(ProgressBar::new(fraction).text(format!("{} / {}", progress.done, progress.total)));
    ui.add_space(4.0);
    ui.label(
        RichText::new(&progress.current_file)
            .monospace()
            .color(ui.visuals().weak_text_color()),
    );
}

fn draw_summary(ui: &mut Ui, summary: &Summary, items: &[Item]) {
    ui.separator();

    if let Some(error) = &summary.error {
        ui.colored_label(Color32::RED, format!("Error: {error}"));
        ui.add_space(4.0);
    }
    if summary.cancelled {
        ui.colored_label(Color32::YELLOW, "⚠ Cancelled");
        ui.add_space(4.0);
    }
    if summary.partial {
        ui.colored_label(
            Color32::YELLOW,
            format!(
                "⚠ Partial checksums.txt written — {} file(s) failed",
                summary.hash_errors
            ),
        );
        ui.add_space(4.0);
    }

    egui::Grid::new("summary_grid")
        .num_columns(2)
        .spacing([20.0, 4.0])
        .show(ui, |ui| {
            ui.label("Checked:");
            ui.label(summary.checked.to_string());
            ui.end_row();

            ui.label(RichText::new("✓ OK:").color(Color32::GREEN));
            ui.label(RichText::new(summary.ok.to_string()).color(Color32::GREEN));
            ui.end_row();

            ui.label(RichText::new("✗ FAIL:").color(if summary.fail > 0 {
                Color32::RED
            } else {
                ui.visuals().text_color()
            }));
            ui.label(summary.fail.to_string());
            ui.end_row();

            ui.label(RichText::new("? MISSING:").color(if summary.missing > 0 {
                Color32::YELLOW
            } else {
                ui.visuals().text_color()
            }));
            ui.label(summary.missing.to_string());
            ui.end_row();

            ui.label(RichText::new("+ EXTRA:").color(if summary.extra > 0 {
                Color32::YELLOW
            } else {
                ui.visuals().text_color()
            }));
            ui.label(summary.extra.to_string());
            ui.end_row();
        });

    ui.add_space(8.0);

    // Collapsible problem lists
    let fails: Vec<_> = items
        .iter()
        .filter_map(|i| {
            if let Item::Fail {
                filename,
                expected,
                got,
            } = i
            {
                Some((filename.as_str(), expected.as_str(), got.as_str()))
            } else {
                None
            }
        })
        .collect();
    let missing: Vec<_> = items
        .iter()
        .filter_map(|i| {
            if let Item::Missing { filename } = i {
                Some(filename.as_str())
            } else {
                None
            }
        })
        .collect();
    let extra: Vec<_> = items
        .iter()
        .filter_map(|i| {
            if let Item::Extra { filename } = i {
                Some(filename.as_str())
            } else {
                None
            }
        })
        .collect();
    let errors: Vec<_> = items
        .iter()
        .filter_map(|i| {
            if let Item::HashError { filename, error } = i {
                Some((filename.as_str(), error.as_str()))
            } else {
                None
            }
        })
        .collect();

    if !fails.is_empty() {
        egui::CollapsingHeader::new(
            RichText::new(format!("✗ Failed files ({})", fails.len())).color(Color32::RED),
        )
        .default_open(true)
        .show(ui, |ui| {
            for (name, exp, got) in &fails {
                ui.monospace(format!("  {name}"));
                ui.label(format!("    expected: {exp}"));
                ui.label(format!("    got:      {got}"));
            }
        });
    }

    if !missing.is_empty() {
        egui::CollapsingHeader::new(
            RichText::new(format!("? Missing files ({})", missing.len())).color(Color32::YELLOW),
        )
        .default_open(true)
        .show(ui, |ui| {
            for name in &missing {
                ui.monospace(format!("  {name}"));
            }
        });
    }

    if !extra.is_empty() {
        egui::CollapsingHeader::new(
            RichText::new(format!("+ Extra files ({})", extra.len())).color(Color32::YELLOW),
        )
        .default_open(true)
        .show(ui, |ui| {
            for name in &extra {
                ui.monospace(format!("  {name}"));
            }
        });
    }

    if !errors.is_empty() {
        egui::CollapsingHeader::new(
            RichText::new(format!("! Hash errors ({})", errors.len())).color(Color32::RED),
        )
        .default_open(true)
        .show(ui, |ui| {
            for (name, err) in &errors {
                ui.monospace(format!("  {name}: {err}"));
            }
        });
    }
}
