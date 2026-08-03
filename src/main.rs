mod data;

use eframe::{App, egui};
use egui::TextBuffer;

use crate::data::Snippet;

const CANVAS_MARGIN: f32 = 0.0;
const CARD_WIDTH: f32 = 80.0;
const CARD_PADDING_H: f32 = 8.0;
const CARD_MARGIN_Y: f32 = 6.0;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([640., 480.]),
        ..Default::default()
    };

    eframe::run_native(
        "floatdea",
        options,
        Box::new(|cc| {
            #[cfg(not(target_arch = "wasm32"))]
            cc.egui_ctx.set_embed_viewports(false);
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::<HomePage>::default())
        }),
    )
}

#[derive(Clone, Debug)]
struct HomePage {
    items: Vec<Snippet>,
    positions: Vec<[f32; 2]>,
    card_sizes: Vec<egui::Vec2>,
    dragging: Option<usize>,
    drag_start_pos: Option<[f32; 2]>,
    drag_invalid: bool,
    views: Vec<View>,
    next_view_id: u64,
    pending_delete: Option<usize>,
    focus_request: bool,
    fps: f32,
    last_time: f64,
}

#[derive(Clone, Copy, Debug)]
struct View {
    id: u64,
    item_id: usize,
    editable: bool,
}

#[derive(Clone, Copy, Debug, Default)]
enum ViewAction {
    #[default]
    None,
    Close,
    Duplicate,
}

impl Default for HomePage {
    fn default() -> Self {
        HomePage {
            items: vec![
                Snippet {
                    title: "hello".to_owned(),
                    content: "hello, world!".to_owned(),
                },
                Snippet {
                    title: "floatdea".to_owned(),
                    content: "Welcome to floatdea!".to_owned(),
                },
                Snippet {
                    title: "help".to_owned(),
                    content: "Right-click for relevent operations.\n\nDouble-click on read-only snippet to edit.".to_owned()
                }
            ],
            positions: vec![[24.0, 24.0], [230.0, 40.0], [430.0, 90.0]],
            card_sizes: vec![egui::vec2(CARD_WIDTH, 25.0); 3],
            dragging: None,
            drag_start_pos: None,
            drag_invalid: false,
            views: Vec::new(),
            next_view_id: 0,
            pending_delete: None,
            focus_request: false,
            fps: 0.0,
            last_time: 0.0,
        }
    }
}

impl HomePage {
    fn open_view(&mut self, item_id: usize) {
        let id = self.next_view_id;
        self.next_view_id += 1;
        let editable = self.items[item_id].content.is_empty();
        self.views.push(View {
            id,
            item_id,
            editable,
        });
    }

    fn default_snippet_title(&self) -> String {
        for number in 1_u64.. {
            let candidate = if number == 1 {
                "Untitled".to_owned()
            } else {
                format!("Untitled {number}")
            };

            if self.items.iter().all(|item| item.title != candidate) {
                return candidate;
            }
        }

        unreachable!("the title counter cannot be exhausted")
    }

    fn render_home_panel(
        &mut self,
        ui: &mut egui::Ui,
        open_requests: &mut Vec<usize>,
        delete_requests: &mut Vec<usize>,
    ) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .inner_margin(egui::Margin::same(8))
                    .fill(ui.visuals().panel_fill),
            )
            .show(ui, |ui| {

                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let available = ui.available_size();
                        let mut extent = egui::Vec2::ZERO;
                        for i in 0..self.items.len() {
                            if self.items[i].title.is_empty() {
                                continue;
                            }
                            let p = self.positions[i];
                            let s = self.card_sizes[i];
                            extent.x = extent.x.max(p[0] + s.x);
                            extent.y = extent.y.max(p[1] + s.y);
                        }
                        let canvas_size =
                            available.max(extent + egui::vec2(CANVAS_MARGIN, CANVAS_MARGIN));
                        let (canvas_rect, canvas_response) =
                            ui.allocate_exact_size(canvas_size, egui::Sense::click());
                        let painter = ui.painter();

                        painter.rect_filled(canvas_rect, 0.0, ui.visuals().panel_fill);
                        Self::paint_grid(
                            &painter,
                            canvas_rect,
                            ui.visuals().weak_text_color().gamma_multiply(0.12),
                        );

                        let pointer_pos = ui.input(|i| i.pointer.interact_pos());
                        let mut pointer_over_card = false;
                        let mut drag_delta = egui::Vec2::ZERO;
                        let mut dragged_this_frame: Option<usize> = None;

                        for i in 0..self.items.len() {
                            if self.items[i].title.is_empty() {
                                continue;
                            }

                            let pos = self.positions[i];
                            let title = self.items[i].title.as_str();
                            let galley = Self::layout_title(
                                &painter,
                                title,
                                CARD_WIDTH - 2.0 * CARD_PADDING_H,
                                ui.visuals().text_color(),
                            );
                            let card_size =
                                egui::vec2(CARD_WIDTH, galley.size().y + 2.0 * CARD_MARGIN_Y);
                            self.card_sizes[i] = card_size;
                            let rect = egui::Rect::from_min_size(
                                canvas_rect.min + egui::vec2(pos[0], pos[1]),
                                card_size,
                            );

                            if let Some(p) = pointer_pos {
                                if rect.contains(p) {
                                    pointer_over_card = true;
                                }
                            }

                            let response = ui.interact(
                                rect,
                                egui::Id::new(("home-card", i)),
                                egui::Sense::click_and_drag(),
                            );

                            response.context_menu(|ui| {
                                if ui.button("delete").clicked() {
                                    delete_requests.push(i);
                                    ui.close();
                                }
                            });

                            if response.clicked() {
                                open_requests.push(i);
                            }

                            if response.drag_started() {
                                self.dragging = Some(i);
                                self.drag_start_pos = Some(self.positions[i]);
                                self.drag_invalid = false;
                            }
                            if self.dragging == Some(i) && response.dragged() {
                                drag_delta = ui.input(|input| input.pointer.delta());
                                dragged_this_frame = Some(i);
                            }

                            Self::paint_card(
                                &painter,
                                rect,
                                &galley,
                                self.dragging == Some(i),
                                ui.visuals(),
                            );
                        }

                        if let Some(i) = dragged_this_frame {
                            self.positions[i][0] += drag_delta.x;
                            self.positions[i][1] += drag_delta.y;
                            self.positions[i][0] = self.positions[i][0].max(0.);
                            self.positions[i][1] = self.positions[i][1].max(0.);

                            let pos_i = self.positions[i];
                            let size_i = self.card_sizes[i];
                            let rect_i = egui::Rect::from_min_size(
                                egui::pos2(pos_i[0], pos_i[1]),
                                size_i,
                            );
                            self.drag_invalid = (0..self.items.len()).any(|j| {
                                if j == i || self.items[j].title.is_empty() {
                                    return false;
                                }
                                let p = self.positions[j];
                                let s = self.card_sizes[j];
                                egui::Rect::from_min_size(egui::pos2(p[0], p[1]), s)
                                    .intersects(rect_i)
                            });
                        }

                        if self.dragging.is_some() && self.drag_invalid {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
                        }

                        if self.dragging.is_some() && !ui.input(|i| i.pointer.any_down()) {
                            if self.drag_invalid {
                                if let (Some(i), Some(start)) =
                                    (self.dragging, self.drag_start_pos)
                                {
                                    self.positions[i] = start;
                                }
                            }
                            self.dragging = None;
                            self.drag_start_pos = None;
                            self.drag_invalid = false;
                        }

                        if !pointer_over_card {
                            canvas_response.context_menu(|ui| {
                                if ui.button("New Snippet").clicked() {
                                    self.items.push(Snippet {
                                        title: self.default_snippet_title(),
                                        content: String::new(),
                                    });
                                    self.positions.push(self.default_position());
                                    self.card_sizes.push(egui::vec2(CARD_WIDTH, 25.0));
                                    ui.close();
                                }
                            });
                        }
                    });
            });
    }

    fn default_position(&self) -> [f32; 2] {
        for row in 0..40 {
            for col in 0..16 {
                let pos = [24.0 + col as f32 * 200.0, 24.0 + row as f32 * 130.0];
                let candidate = egui::Rect::from_min_size(
                    egui::pos2(pos[0], pos[1]),
                    egui::vec2(CARD_WIDTH, 40.0),
                );
                let occupied = (0..self.items.len()).any(|i| {
                    if self.items[i].title.is_empty() {
                        return false;
                    }
                    let p = self.positions[i];
                    let size = self.card_sizes[i];
                    egui::Rect::from_min_size(egui::pos2(p[0], p[1]), size).intersects(candidate)
                });
                if !occupied {
                    return pos;
                }
            }
        }
        [24.0, 24.0]
    }

    fn paint_grid(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
        const STEP: f32 = 32.0;
        let mut y = rect.min.y + STEP;
        while y < rect.max.y {
            let mut x = rect.min.x + STEP;
            while x < rect.max.x {
                painter.circle_filled(egui::pos2(x, y), 1.0, color);
                x += STEP;
            }
            y += STEP;
        }
    }

    fn layout_title(
        painter: &egui::Painter,
        text: &str,
        max_width: f32,
        color: egui::Color32,
    ) -> std::sync::Arc<egui::Galley> {
        let mut job = egui::text::LayoutJob::default();
        job.append(
            text,
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::proportional(14.0),
                color,
                ..Default::default()
            },
        );
        job.wrap.max_width = max_width.max(10.0);
        painter.layout_job(job)
    }

    fn paint_card(
        painter: &egui::Painter,
        rect: egui::Rect,
        galley: &std::sync::Arc<egui::Galley>,
        dragging: bool,
        visuals: &egui::Visuals,
    ) {
        let bg = if dragging {
            visuals.widgets.active.bg_fill
        } else {
            visuals.widgets.inactive.bg_fill
        };
        let stroke = if dragging {
            egui::Stroke::new(2.0, visuals.selection.stroke.color)
        } else {
            egui::Stroke::new(1.0, visuals.widgets.inactive.bg_stroke.color)
        };
        painter.rect(rect, 2.5, bg, stroke, egui::StrokeKind::Inside);

        let clip = painter.with_clip_rect(rect);
        clip.galley(
            rect.min + egui::vec2(CARD_PADDING_H, CARD_MARGIN_Y),
            galley.clone(),
            visuals.text_color(),
        );
    }

    fn render_delete_dialog(&mut self, ui: &mut egui::Ui) {
        if let Some(item_id) = self.pending_delete {
            let title = self.items[item_id].title.clone();
            let mut confirmed = false;
            let mut cancelled = false;

            egui::Window::new(())
                .id(egui::Id::new("delete-snippet-confirmation"))
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    ui.label(format!("Delete \"{title}\"?"));
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            cancelled = true;
                        }
                        let delete_button = egui::Button::new(
                            egui::RichText::new("Delete").color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(179, 38, 30));
                        if ui.add(delete_button).clicked() {
                            confirmed = true;
                        }
                    });
                });

            if confirmed {
                self.items[item_id].title.clear();
                self.views.retain(|view| view.item_id != item_id);
                self.pending_delete = None;
            } else if cancelled {
                self.pending_delete = None;
            }
        }
    }

    fn show_content_context_menu(
        ui: &mut egui::Ui,
        content: &str,
        current_selection: Option<egui::text::CCursorRange>,
        view: &mut View,
        action: &mut ViewAction,
        focus_request: &mut bool,
    ) {
        if let Some(selection) = current_selection {
            let selected = content
                .char_range(selection.as_sorted_char_range())
                .to_owned();
            if selected.is_empty() {
                if ui.button("Copy All").clicked() {
                    ui.copy_text(content.to_owned());
                    ui.close();
                }
            } else if ui.button("Copy").clicked() {
                ui.copy_text(selected);
                ui.close();
            }
        } else if ui.button("Copy All").clicked() {
            ui.copy_text(content.to_owned());
            ui.close();
        }

        if ui
            .button(if view.editable {
                "Exit Edit"
            } else {
                "Edit..."
            })
            .clicked()
        {
            view.editable = !view.editable;
            ui.close();
        }
        ui.separator();
        if ui.button("Duplicate").clicked() {
            *action = ViewAction::Duplicate;
            ui.close();
        }
        if ui.button("Focus").clicked() {
            *focus_request = true;
        }
    }

    fn render_snippet_content(
        ui: &mut egui::Ui,
        view: &mut View,
        content: &mut String,
        focus_request: &mut bool,
        action: &mut ViewAction,
    ) {
        let text_edit_id = ui.make_persistent_id(("snippet-content", view.id));
        let saved_selection = egui::TextEdit::load_state(ui.ctx(), text_edit_id)
            .and_then(|state| state.cursor.char_range());
        let secondary_pressed = ui.input(|input| input.pointer.secondary_pressed());
        let escape_pressed =
            view.editable && ui.input(|input| input.key_pressed(egui::Key::Escape));

        if escape_pressed {
            view.editable = false;
            ui.memory_mut(|memory| {
                memory.surrender_focus(text_edit_id);
            });
            ui.input_mut(|input| {
                input.consume_key(input.modifiers, egui::Key::Escape);
            });
        }

        let mut text_edit = |text: &mut dyn egui::TextBuffer| {
            egui::TextEdit::multiline(text)
                .id(text_edit_id)
                .font(egui::FontId::proportional(18.0))
                .desired_width(f32::INFINITY)
                .frame(egui::Frame::NONE)
                .show(ui)
        };

        let mut output = if view.editable {
            text_edit(content)
        } else {
            let mut read_only_content = content.as_str();
            text_edit(&mut read_only_content)
        };

        let saved_selection = saved_selection.map(|mut range| {
            range.primary = output.galley.clamp_cursor(&range.primary);
            range.secondary = output.galley.clamp_cursor(&range.secondary);
            range
        });

        if secondary_pressed && output.response.contains_pointer() {
            output.state.cursor.set_char_range(saved_selection);
            output.state.store(ui.ctx(), output.response.id);
        }

        let current_selection = if secondary_pressed && output.response.contains_pointer() {
            saved_selection
        } else {
            output.cursor_range
        };

        if output.response.changed() {
            ui.ctx().request_repaint();
        }

        if !view.editable && output.response.double_clicked() {
            view.editable = true;
            output.response.request_focus();
            ui.ctx().request_repaint();
        }

        let content_ref = content.as_str();
        output.response.context_menu(|ui| {
            Self::show_content_context_menu(
                ui,
                content_ref,
                current_selection,
                view,
                action,
                focus_request,
            );
        });
    }

    fn render_snippet_viewport(
        ui: &mut egui::Ui,
        view: &mut View,
        content: &mut String,
        title: &str,
        focus_request: &mut bool,
    ) -> ViewAction {
        ui.show_viewport_immediate(
            egui::ViewportId::from_hash_of(("snippet-view", view.id)),
            egui::ViewportBuilder::default()
                .with_title(format!("{} - FloatDea", title))
                .with_inner_size([480.0, 320.0]),
            |child_ui, _viewport_class| {
                let mut action = ViewAction::None;

                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::new()
                            .inner_margin(egui::Margin::same(16))
                            .fill(child_ui.visuals().panel_fill),
                    )
                    .show(child_ui, |ui| {
                        egui::Frame::new()
                            .inner_margin(egui::Margin::same(12))
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        Self::render_snippet_content(
                                            ui,
                                            view,
                                            content,
                                            focus_request,
                                            &mut action,
                                        );
                                    });
                            });
                    });

                if child_ui.input(|input| input.viewport().close_requested()) {
                    action = ViewAction::Close;
                }
                action
            },
        )
    }
}

impl App for HomePage {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let mut open_requests = Vec::new();
        let mut delete_requests = Vec::new();

        self.render_home_panel(ui, &mut open_requests, &mut delete_requests);

        if let Some(item_id) = delete_requests.into_iter().next() {
            self.pending_delete = Some(item_id);
        }
        self.render_delete_dialog(ui);

        for item_id in open_requests {
            self.open_view(item_id);
        }

        let mut closed_views = Vec::new();

        for view in &mut self.views {
            let item = &mut self.items[view.item_id];
            let title = item.title.as_str();
            let content = &mut item.content;
            let action =
                Self::render_snippet_viewport(ui, view, content, title, &mut self.focus_request);
            if matches!(action, ViewAction::Close) {
                closed_views.push(view.id);
            }
        }

        self.views.retain(|view| !closed_views.contains(&view.id));

        let now = ui.ctx().input(|i| i.time);
        if self.last_time > 0.0 {
            let dt = (now - self.last_time).max(1e-6) as f32;
            let instant = 1.0 / dt;
            self.fps = if self.fps == 0.0 {
                instant
            } else {
                self.fps * 0.9 + instant * 0.1
            };
        }
        self.last_time = now;
        let fps = self.fps;
        egui::Window::new("FPS")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-8.0, 8.0))
            .show(ui.ctx(), |ui| {
                ui.label(format!("{:.1} fps", fps));
            });
    }
}
