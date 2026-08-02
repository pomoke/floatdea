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
    views: Vec<View>,
    next_view_id: u64,
    pending_delete: Option<usize>,
    focus_request: bool,
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
            views: Vec::new(),
            next_view_id: 0,
            pending_delete: None,
            focus_request: false,
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
        let home_background = egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .inner_margin(egui::Margin::same(16))
                    .fill(ui.visuals().panel_fill),
            )
            .show(ui, |ui| {
                // Register the background first so buttons added afterwards stay on top.
                let background_response = ui.interact(
                    ui.max_rect(),
                    ui.id().with("home-background-menu"),
                    egui::Sense::click(),
                );

                for (id, item) in self.items.iter().enumerate() {
                    if item.title.is_empty() {
                        continue;
                    }

                    let btn = ui.button(&item.title);
                    btn.context_menu(|ui| {
                        if ui.button("delete").clicked() {
                            delete_requests.push(id);
                            ui.close();
                        }
                    });
                    if btn.clicked() {
                        open_requests.push(id);
                    }
                }
                background_response
            })
            .inner;

        home_background.context_menu(|ui| {
            if ui.button("New Snippet").clicked() {
                self.items.push(Snippet {
                    title: self.default_snippet_title(),
                    content: String::new(),
                });
                ui.close();
            }
        });
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
        let escape_pressed = view.editable
            && ui.input(|input| input.key_pressed(egui::Key::Escape));

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
            let action = Self::render_snippet_viewport(
                ui,
                view,
                content,
                title,
                &mut self.focus_request,
            );
            if matches!(action, ViewAction::Close) {
                closed_views.push(view.id);
            }
        }

        self.views.retain(|view| !closed_views.contains(&view.id));
    }
}
