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
    /// clicking the permanent "⚙ Settings" card in the root box (a `Special`
    /// item) and closed via the window's close button.
    pub(super) fn render_settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        // Destructure `self` so the window builder and the body closure borrow
        // disjoint fields instead of `self` twice.
        let settings = &mut self.settings;
        let settings_open = &mut self.settings_open;
        let settings_store = &self.settings_store;
        let mut changed = false;
        // A regular floating window: the title bar makes it draggable, and its
        // position is remembered by egui across frames.
        egui::Window::new("Settings")
            .id(egui::Id::new("settings-window"))
            .open(settings_open)
            .default_size([400.0, 560.0])
            .collapsible(false)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("settings-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        Self::settings_body(ui, settings, &mut changed);
                    });
            });
        if changed {
            let _ = settings_store.save(settings);
            // Apply immediately so the switch is visible this frame; the
            // per-frame call in `HomePage::ui` keeps it in sync afterwards.
            self.apply_theme(ctx);
            ctx.request_repaint();
        }
    }

    /// The settings body (theme, preview, grid, window mode, AI). `changed` is
    /// set when any persisted field is edited.
    fn settings_body(ui: &mut egui::Ui, settings: &mut Settings, changed: &mut bool) {
        ui.heading("Settings");
        ui.add_space(6.0);
        Self::settings_theme_ui(ui, settings, changed);
        Self::settings_display_ui(ui, settings, changed);
        Self::settings_window_mode_ui(ui, settings, changed);
        Self::settings_ai_ui(ui, settings, changed);
        ui.add_space(16.0);
        ui.separator();
        ui.label(format!("FloatDea {}", env!("CARGO_PKG_VERSION")));
    }

    fn settings_theme_ui(ui: &mut egui::Ui, settings: &mut Settings, changed: &mut bool) {
        ui.label("Theme");
        let mut theme_changed = false;
        egui::ComboBox::from_id_salt("settings-theme")
            .selected_text(match settings.theme {
                ThemeSetting::System => "System",
                ThemeSetting::Light => "Light",
                ThemeSetting::Dark => "Dark",
            })
            .show_ui(ui, |ui| {
                theme_changed |= ui
                    .selectable_value(&mut settings.theme, ThemeSetting::System, "System")
                    .changed();
                theme_changed |= ui
                    .selectable_value(&mut settings.theme, ThemeSetting::Light, "Light")
                    .changed();
                theme_changed |= ui
                    .selectable_value(&mut settings.theme, ThemeSetting::Dark, "Dark")
                    .changed();
            });
        if theme_changed {
            *changed = true;
        }
        ui.add_space(10.0);
    }

    fn settings_display_ui(ui: &mut egui::Ui, settings: &mut Settings, changed: &mut bool) {
        ui.label(format!("Preview font size: {:.0} pt", settings.preview_font_size));
        if ui
            .add(egui::Slider::new(
                &mut settings.preview_font_size,
                10.0..=32.0,
            ))
            .changed()
        {
            *changed = true;
        }

        ui.add_space(10.0);
        ui.label(format!(
            "Math formula height cap: {:.2}× line",
            settings.math_cap_scale
        ));
        if ui
            .add(egui::Slider::new(&mut settings.math_cap_scale, 0.6..=2.5))
            .changed()
        {
            *changed = true;
        }

        ui.add_space(10.0);
        if ui
            .checkbox(&mut settings.snap_to_grid, "Snap dragged cards to grid")
            .on_hover_text("Cards align to the 32 pt canvas grid while dragging")
            .changed()
        {
            *changed = true;
        }
        if ui
            .checkbox(&mut settings.show_grid, "Show canvas grid")
            .on_hover_text("Draw the 32 pt dot grid on every canvas")
            .changed()
        {
            *changed = true;
        }
    }

    fn settings_window_mode_ui(ui: &mut egui::Ui, settings: &mut Settings, changed: &mut bool) {
        ui.add_space(10.0);
        ui.label("Window mode");
        let mut mode_changed = false;
        egui::ComboBox::from_id_salt("settings-window-mode")
            .selected_text(match settings.window_mode {
                WindowMode::Native => "Native windows",
                WindowMode::Floating => "Floating (single window)",
            })
            .show_ui(ui, |ui| {
                mode_changed |= ui
                    .selectable_value(
                        &mut settings.window_mode,
                        WindowMode::Native,
                        "Native windows",
                    )
                    .changed();
                mode_changed |= ui
                    .selectable_value(
                        &mut settings.window_mode,
                        WindowMode::Floating,
                        "Floating (single window)",
                    )
                    .on_hover_text(
                        "Canvas and snippet windows float inside the main window and can be dragged freely",
                    )
                    .changed();
            });
        if mode_changed {
            *changed = true;
            // Resize the main window to match: larger in full-window mode
            // to leave room for the floating windows, 640×480 otherwise.
            let size = if settings.window_mode == WindowMode::Floating {
                egui::vec2(FLOATING_MAIN_SIZE[0], FLOATING_MAIN_SIZE[1])
            } else {
                egui::vec2(ROOT_CANVAS_SIZE[0], ROOT_CANVAS_SIZE[1])
            };
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        }
    }

    /// The AI section: master switch, provider family, model, custom endpoint
    /// and the *name* of the environment variable holding the API key. The key
    /// value itself is never persisted (see plan_ai.md §10).
    fn settings_ai_ui(ui: &mut egui::Ui, settings: &mut Settings, changed: &mut bool) {
        ui.add_space(14.0);
        ui.separator();
        ui.add_space(6.0);
        ui.heading("AI");
        if ui
            .checkbox(&mut settings.ai_enabled, "Enable AI")
            .on_hover_text("When off, no model network request is ever issued; AI boxes remain usable as read-only workbenches")
            .changed()
        {
            *changed = true;
        }
        if !settings.ai_enabled {
            ui.add_space(4.0);
            ui.label("AI is off. AI boxes still work as read-only source workbenches.");
            return;
        }
        ui.add_space(8.0);
        if ui
            .checkbox(&mut settings.ai_tools_enabled, "Allow model tool calls")
            .on_hover_text("The model may call bounded built-in tools (list/read/search the bound sources, create an output proposal). Every call shows a visible receipt; proposals still require your confirmation before any snippet is created.")
            .changed()
        {
            *changed = true;
        }
        ui.add_space(8.0);
        ui.label("Provider");
        egui::ComboBox::from_id_salt("settings-ai-provider")
            .selected_text(settings.ai_provider.label())
            .show_ui(ui, |ui| {
                for kind in [
                    ProviderKind::Fake,
                    ProviderKind::OpenAiCompatible,
                    ProviderKind::Ollama,
                ] {
                    if ui
                        .selectable_value(&mut settings.ai_provider, kind, kind.label())
                        .changed()
                    {
                        *changed = true;
                    }
                }
            });
        ui.add_space(8.0);
        ui.label("Model");
        if ui
            .add(
                egui::TextEdit::singleline(&mut settings.ai_model)
                    .id(egui::Id::new("settings-ai-model"))
                    .hint_text("e.g. gpt-4o-mini, llama3.2"),
            )
            .changed()
        {
            *changed = true;
        }
        // Summarizer (auxiliary) model: a lighter/cheaper model for
        // automatic title generation and other lightweight tasks.
        ui.add_space(10.0);
        ui.label("Summarizer model (optional)");
        if ui
            .add(
                egui::TextEdit::singleline(&mut settings.summarizer_model)
                    .id(egui::Id::new("settings-summarizer-model"))
                    .hint_text("leave blank to use the main model"),
            )
            .changed()
        {
            *changed = true;
        }
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("Used for auto-generating conversation titles and other lightweight tasks.")
                .small()
                .color(ui.visuals().weak_text_color()),
        );

        if settings.ai_provider != ProviderKind::Fake {
            ui.add_space(8.0);
            ui.label("Base URL (optional)");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut settings.ai_base_url)
                        .id(egui::Id::new("settings-ai-base-url"))
                        .hint_text("e.g. https://api.example.com/v1 (blank = provider default)"),
                )
                .changed()
            {
                *changed = true;
            }
            ui.add_space(8.0);
            ui.label("API key");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut settings.ai_api_key)
                        .id(egui::Id::new("settings-ai-key"))
                        .password(true)
                        .hint_text("sk-…"),
                )
                .changed()
            {
                *changed = true;
            }
            ui.add_space(4.0);
            ui.label("Stored locally in .floatdea/settings.json. Never share this file.");
            ui.add_space(4.0);
            match settings.ai_provider {
                ProviderKind::Fake => {}
                ProviderKind::OpenAiCompatible => {
                    ui.label("Sends the conversation to the configured endpoint when you send a message.");
                }
                ProviderKind::Ollama => {
                    ui.label("Connects to a local Ollama service (default http://localhost:11434).");
                }
            }
        } else {
            ui.add_space(4.0);
            ui.label("Fake provider: deterministic local replies, no network. Good for testing the AI workbench offline.");
        }
    }
}
