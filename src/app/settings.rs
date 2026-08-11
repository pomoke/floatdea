use super::*;

impl HomePage {
    /// Applies the persisted theme preference to the shared egui context. Every
    /// viewport uses the same context, so this affects all windows. Idempotent;
    /// called once per frame so a change in the settings window takes effect
    /// immediately.
    pub(super) fn apply_theme(&self, ctx: &egui::Context) {
        let preference = match self.settings.theme {
            ThemeSetting::System => egui::ThemePreference::System,
            ThemeSetting::Light => egui::ThemePreference::Light,
            ThemeSetting::Dark => egui::ThemePreference::Dark,
        };
        ctx.set_theme(preference);
    }

    /// Renders the system settings window in the root viewport. It is opened by
    /// clicking the permanent "⚙ 设置" card in the root box (a `Special` item)
    /// and closed via the window's close button.
    pub(super) fn render_settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut changed = false;
        // A regular floating window: the title bar makes it draggable, and its
        // position is remembered by egui across frames.
        egui::Window::new("Settings")
            .id(egui::Id::new("settings-window"))
            .open(&mut self.settings_open)
            .default_size([380.0, 300.0])
            .collapsible(false)
            .show(ctx, |ui| {
                ui.heading("Settings");
                ui.add_space(6.0);

                ui.label("Theme");
                egui::ComboBox::from_id_salt("settings-theme")
                    .selected_text(match self.settings.theme {
                        ThemeSetting::System => "System",
                        ThemeSetting::Light => "Light",
                        ThemeSetting::Dark => "Dark",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.settings.theme, ThemeSetting::System, "System");
                        ui.selectable_value(&mut self.settings.theme, ThemeSetting::Light, "Light");
                        ui.selectable_value(&mut self.settings.theme, ThemeSetting::Dark, "Dark");
                    });

                ui.add_space(10.0);
                ui.label(format!(
                    "Preview font size: {:.0} pt",
                    self.settings.preview_font_size
                ));
                if ui
                    .add(egui::Slider::new(
                        &mut self.settings.preview_font_size,
                        10.0..=32.0,
                    ))
                    .changed()
                {
                    changed = true;
                }

                ui.add_space(10.0);
                ui.label(format!(
                    "Math formula height cap: {:.2}× line",
                    self.settings.math_cap_scale
                ));
                if ui
                    .add(egui::Slider::new(&mut self.settings.math_cap_scale, 0.6..=2.5))
                    .changed()
                {
                    changed = true;
                }

                ui.add_space(10.0);
                if ui
                    .checkbox(&mut self.settings.snap_to_grid, "Snap dragged cards to grid")
                    .on_hover_text("Cards align to the 32 pt canvas grid while dragging")
                    .changed()
                {
                    changed = true;
                }
                if ui
                    .checkbox(&mut self.settings.show_grid, "Show canvas grid")
                    .on_hover_text("Draw the 32 pt dot grid on every canvas")
                    .changed()
                {
                    changed = true;
                }

                ui.add_space(10.0);
                ui.label("Window mode");
                let mut mode_changed = false;
                egui::ComboBox::from_id_salt("settings-window-mode")
                    .selected_text(match self.settings.window_mode {
                        WindowMode::Native => "Native windows",
                        WindowMode::Floating => "Floating (single window)",
                    })
                    .show_ui(ui, |ui| {
                        mode_changed |= ui
                            .selectable_value(
                                &mut self.settings.window_mode,
                                WindowMode::Native,
                                "Native windows",
                            )
                            .changed();
                        mode_changed |= ui
                            .selectable_value(
                                &mut self.settings.window_mode,
                                WindowMode::Floating,
                                "Floating (single window)",
                            )
                            .on_hover_text(
                                "Canvas and snippet windows float inside the main window and can be dragged freely",
                            )
                            .changed();
                    });
                if mode_changed {
                    changed = true;
                    // Resize the main window to match: larger in full-window mode
                    // to leave room for the floating windows, 640×480 otherwise.
                    let size = if self.settings.window_mode == WindowMode::Floating {
                        egui::vec2(FLOATING_MAIN_SIZE[0], FLOATING_MAIN_SIZE[1])
                    } else {
                        egui::vec2(ROOT_CANVAS_SIZE[0], ROOT_CANVAS_SIZE[1])
                    };
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
                }

                ui.add_space(16.0);
                ui.separator();
                ui.label(format!("FloatDea {}", env!("CARGO_PKG_VERSION")));
            });
        if changed {
            let _ = self.settings_store.save(&self.settings);
            // Apply immediately so the switch is visible this frame; the
            // per-frame call in `HomePage::ui` keeps it in sync afterwards.
            self.apply_theme(ctx);
            ctx.request_repaint();
        }
    }
}
