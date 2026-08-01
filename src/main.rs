mod data;

use eframe::{App, egui};
use egui::TextBuffer;

use crate::data::Snippet;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([640., 480.]),
        ..Default::default()
    };

    eframe::run_native(
        "floatdea",
        options,
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::<HomePage>::default())
        }),
    )
}

#[derive(Clone, Debug)]
struct HomePage {
    items: Vec<Snippet>,
    views: Vec<View>,
    next_view_id: u64,
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
                    content: "Welcome to FloatDea!".to_owned(),
                },
            ],
            views: Vec::new(),
            next_view_id: 0,
        }
    }
}

impl HomePage {
    fn open_view(&mut self, item_id: usize) {
        let id = self.next_view_id;
        self.next_view_id += 1;
        self.views.push(View {
            id,
            item_id,
            editable: false,
        });
    }
}

impl App for HomePage {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let mut open_requests = Vec::new();

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .inner_margin(egui::Margin::same(16))
                    .fill(ui.visuals().panel_fill),
            )
            .show(ui, |ui| {
                for (id, item) in self.items.iter().enumerate() {
                    let btn = ui.button(&item.title);
                    btn.context_menu(|ui| if ui.button("delete").clicked() {});
                    if btn.clicked() {
                        open_requests.push(id);
                    }
                }
            });

        for item_id in open_requests {
            self.open_view(item_id);
        }

        let mut closed_views = Vec::new();

        for view in &mut self.views {
            let item = &mut self.items[view.item_id];
            let title = item.title.as_str();
            let content = &mut item.content;
            let action = ui.show_viewport_immediate(
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
                                            let text_edit_id =
                                                ui.make_persistent_id(("snippet-content", view.id));
                                            let saved_selection =
                                                egui::TextEdit::load_state(ui.ctx(), text_edit_id)
                                                    .and_then(|state| state.cursor.char_range());
                                            let secondary_pressed =
                                                ui.input(|input| input.pointer.secondary_pressed());

                                            let mut text_edit =
                                                |text: &mut dyn egui::TextBuffer| {
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

                                            let saved_selection =
                                                saved_selection.map(|mut range| {
                                                    range.primary =
                                                        output.galley.clamp_cursor(&range.primary);
                                                    range.secondary = output
                                                        .galley
                                                        .clamp_cursor(&range.secondary);
                                                    range
                                                });

                                            if secondary_pressed
                                                && output.response.contains_pointer()
                                            {
                                                output.state.cursor.set_char_range(saved_selection);
                                                output.state.store(ui.ctx(), output.response.id);
                                            }

                                            let current_selection = if secondary_pressed
                                                && output.response.contains_pointer()
                                            {
                                                saved_selection
                                            } else {
                                                output.cursor_range
                                            };

                                            if output.response.changed() {
                                                ui.ctx().request_repaint();
                                            }

                                            output.response.context_menu(|ui| {
                                                if let Some(selection) = current_selection {
                                                    let selected = content
                                                        .char_range(
                                                            selection.as_sorted_char_range(),
                                                        )
                                                        .to_owned();
                                                    if selected.is_empty() {
                                                        if ui.button("Copy All").clicked() {
                                                            ui.copy_text(content.to_owned());
                                                            ui.close();
                                                        }
                                                    } else {
                                                        if ui.button("Copy").clicked() {
                                                            ui.copy_text(selected);
                                                            ui.close();
                                                        }
                                                    }
                                                } else {
                                                    if ui.button("Copy All").clicked() {
                                                        ui.copy_text(content.to_owned());
                                                        ui.close();
                                                    }
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
                                                    action = ViewAction::Duplicate;
                                                    ui.close();
                                                }
                                            });
                                        });
                                });
                        });

                    if child_ui.input(|input| input.viewport().close_requested()) {
                        action = ViewAction::Close;
                    }
                    action
                },
            );

            match action {
                ViewAction::None => {}
                ViewAction::Close => closed_views.push(view.id),
                ViewAction::Duplicate => {}
            }
        }

        self.views.retain(|view| !closed_views.contains(&view.id));
    }
}
