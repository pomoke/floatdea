use egui::TextBuffer;

use super::*;

impl HomePage {
    fn show_content_context_menu(
        ui: &mut egui::Ui,
        content: &str,
        selection: Option<egui::text::CCursorRange>,
        view: &mut View,
    ) {
        if let Some(selection) = selection {
            let selected = content
                .char_range(selection.as_sorted_char_range())
                .to_owned();
            if !selected.is_empty() && ui.button("Copy").clicked() {
                ui.copy_text(selected);
                ui.close();
            } else if selected.is_empty() && ui.button("Copy All").clicked() {
                ui.copy_text(content.to_owned());
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
    }

    fn render_snippet_content(
        ui: &mut egui::Ui,
        view: &mut View,
        snippet: &mut Snippet,
        store: &SnippetStore,
    ) {
        let text_edit_id = ui.make_persistent_id(("snippet-content", view.id));
        let saved_selection = egui::TextEdit::load_state(ui.ctx(), text_edit_id)
            .and_then(|state| state.cursor.char_range());
        let secondary_pressed = ui.input(|input| input.pointer.secondary_pressed());

        if view.editable && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            view.editable = false;
            ui.memory_mut(|memory| memory.surrender_focus(text_edit_id));
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
            text_edit(&mut snippet.content)
        } else {
            let mut content = snippet.content.as_str();
            text_edit(&mut content)
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
        let selection = if secondary_pressed && output.response.contains_pointer() {
            saved_selection
        } else {
            output.cursor_range
        };

        if output.response.changed() {
            ui.ctx().request_repaint();
            let _ = store.save(snippet);
        }
        if !view.editable && output.response.double_clicked() {
            view.editable = true;
            output.response.request_focus();
            ui.ctx().request_repaint();
        }

        output.response.context_menu(|ui| {
            Self::show_content_context_menu(ui, &snippet.content, selection, view);
        });
    }

    pub(super) fn render_snippet_viewport(
        ui: &mut egui::Ui,
        view: &mut View,
        snippet: &mut Snippet,
        store: &SnippetStore,
    ) -> ViewAction {
        ui.show_viewport_immediate(
            egui::ViewportId::from_hash_of(("snippet-view", view.id)),
            egui::ViewportBuilder::default()
                .with_title(format!("{} - FloatDea", snippet.title))
                .with_inner_size([480.0, 320.0]),
            |child_ui, _| {
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
                                        Self::render_snippet_content(ui, view, snippet, store);
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
