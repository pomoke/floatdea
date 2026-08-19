//! The AI conversation window (阶段 2): a bounded chat bound to one
//! conversation's sources. The UI collects actions and applies them at the
//! frame end, mirroring the canvas command flow. Model I/O runs on the shared
//! AI worker; streaming deltas arrive through `HomePage::ai_events`.

use super::*;
use floatdea::data::ai::extract_text_from_file;

/// Maximum total characters across all sources in a single system prompt.
const MAX_TOTAL_SOURCE_CHARS: usize = 32_000;
/// Maximum characters per individual source.
const MAX_PER_SOURCE_CHARS: usize = 8_000;
/// Minimum characters per individual source (to avoid unusably tiny excerpts).
const MIN_PER_SOURCE_CHARS: usize = 1_000;

/// Returns the per-source character limit adapted to the number of sources so
/// that the total across all sources stays within [`MAX_TOTAL_SOURCE_CHARS`].
fn adaptive_source_limit(num_sources: usize) -> usize {
    if num_sources == 0 {
        return MAX_PER_SOURCE_CHARS;
    }
    MAX_TOTAL_SOURCE_CHARS
        .div_ceil(num_sources)
        .clamp(MIN_PER_SOURCE_CHARS, MAX_PER_SOURCE_CHARS)
}

/// Actions collected from the conversation window this frame, applied after the
/// window closure (the "UI collects, frame end applies" pattern).
enum AiChatAction {
    None,
    Send,
    Stop,
    Retry,
    SaveSnippet(usize),
    ApplyProposal(usize),
    RejectProposal(usize),
    OpenSource(EntityId),
    OpenFolder(ContainerId),
    OpenExternalFile(ExternalFileRef),
}

impl HomePage {
    /// Renders the open conversation window (native viewport or floating
    /// window depending on the window mode).
    pub(super) fn render_ai_conversation_window(&mut self, ui: &mut egui::Ui) {
        let Some((ai_box, conversation)) = self.ai_open.clone() else {
            return;
        };
        // If the conversation was deleted while open, close the window.
        let Some(conv) = self
            .ai_boxes
            .get(&ai_box)
            .and_then(|data| data.get(&conversation))
            .cloned()
        else {
            self.ai_open = None;
            return;
        };
        let running = self
            .ai_active_turn
            .as_ref()
            .is_some_and(|turn| turn.ai_box == ai_box && turn.conversation == conversation);
        let floating = self.settings.window_mode == WindowMode::Floating;
        let mut action = AiChatAction::None;
        let mut close = false;
        let window_id = egui::Id::new((
            "ai-conversation",
            ai_box.as_str(),
            conversation.as_str(),
        ));
        let title = format!("{} - AI - FloatDea", conv.title);
        if floating {
            let mut open = true;
            egui::Window::new(&title)
                .id(window_id)
                .open(&mut open)
                .default_size([520.0, 520.0])
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    action = Self::render_ai_chat_panel(ui, self, &ai_box, &conv, running);
                });
            if !open {
                close = true;
            }
        } else {
            ui.show_viewport_immediate(
                egui::ViewportId::from_hash_of((
                    "ai-conversation",
                    ai_box.as_str(),
                    conversation.as_str(),
                )),
                egui::ViewportBuilder::default()
                    .with_title(&title)
                    .with_inner_size([520.0, 520.0]),
                |child_ui, _| {
                    action = Self::render_ai_chat_panel(child_ui, self, &ai_box, &conv, running);
                    if child_ui.input(|input| input.viewport().close_requested()) {
                        close = true;
                    }
                },
            );
        }
        if close {
            self.ai_open = None;
        }
        self.apply_ai_chat_action(ai_box, conversation, action);
    }

    fn apply_ai_chat_action(
        &mut self,
        ai_box: ContainerId,
        _conversation: ConversationId,
        action: AiChatAction,
    ) {
        match action {
            AiChatAction::None => {}
            AiChatAction::Send => self.ai_send(),
            AiChatAction::Stop => self.ai_stop(),
            AiChatAction::Retry => self.ai_retry(),
            AiChatAction::SaveSnippet(index) => self.ai_save_as_snippet(&ai_box, index),
            AiChatAction::ApplyProposal(index) => self.apply_proposal(&ai_box, index),
            AiChatAction::RejectProposal(index) => self.reject_proposal(&ai_box, index),
            AiChatAction::OpenSource(id) => {
                self.clipboard = None;
                self.open_view(id);
            }
            AiChatAction::OpenFolder(id) => {
                self.clipboard = None;
                self.open_folder(&id);
            }
            AiChatAction::OpenExternalFile(file) => {
                let _ = open_path_externally(&file.path);
            }
        }
    }

    /// The conversation window body: header (Sources:N + provider + status),
    /// message scroll area and the IME-safe input row.
    fn render_ai_chat_panel(
        ui: &mut egui::Ui,
        page: &mut HomePage,
        ai_box: &ContainerId,
        conv: &Conversation,
        running: bool,
    ) -> AiChatAction {
        let mut action = AiChatAction::None;
        let messages = conv.messages.clone();

        // A `CentralPanel` paints the panel background (a bare viewport would
        // otherwise clear to near-black, making the content unreadable).
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .inner_margin(egui::Margin::same(10))
                    .fill(ui.visuals().panel_fill),
            )
            .show(ui, |ui| {
                // ---- Messages ----
                let message_area_height = (ui.available_height() - 90.0).max(120.0);
                egui::ScrollArea::vertical()
                    .id_salt(("ai-messages", conv.id.as_str()))
                    .max_height(message_area_height)
                    .auto_shrink([false, false])
                    .stick_to_bottom(running)
                    .show(ui, |ui| {
                        for (index, message) in messages.iter().enumerate() {
                            Self::render_chat_message(ui, page, ai_box, message, index, &mut action);
                        }
                        if running {
                            let streaming = page.ai_streaming.clone();
                            ui.add_space(4.0);
                            let text = if streaming.is_empty() { "…" } else { &streaming };
                            ui.label(egui::RichText::new(text).italics());
                        }
                    });

                // ---- Input row (model + status sit next to Send/Stop) ----
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut page.ai_input)
                            .id(egui::Id::new(("ai-chat-input", conv.id.as_str())))
                            .desired_width((ui.available_width() - 120.0).max(120.0))
                            .hint_text("Ask from sources… (Enter to send, Shift+Enter for a newline)"),
                    );
                    // The send-side column: primary action on top, model name
                    // and the generating indicator underneath.
                    ui.vertical(|ui| {
                        if running {
                            if ui.button("Stop").clicked() {
                                action = AiChatAction::Stop;
                            }
                        } else {
                            let ime_composing = ui.input(|input| {
                                input.events.iter().any(|event| {
                                    matches!(
                                        event,
                                        egui::Event::Ime(egui::ImeEvent::Preedit { text, .. })
                                            if !text.is_empty()
                                    )
                                })
                            });
                            let enter = ui.input(|input| input.key_pressed(egui::Key::Enter));
                            let shift = ui.input(|input| input.modifiers.shift);
                            // Enter sends only when the IME is not composing (a
                            // Chinese IME uses Enter to commit the composition).
                            let enter_send = enter && !shift && !ime_composing;
                            if enter_send {
                                ui.input_mut(|input| {
                                    input.consume_key(input.modifiers, egui::Key::Enter);
                                });
                            }
                            let send = ui.button("Send").clicked() || enter_send;
                            if send && !page.ai_input.trim().is_empty() {
                                action = AiChatAction::Send;
                            }
                        }
                        ui.label(
                            egui::RichText::new(page.provider_label())
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                        if running {
                            ui.label(
                                egui::RichText::new("● Generating…")
                                    .small()
                                    .color(ui.visuals().selection.stroke.color),
                            );
                        }
                    });
                });
            });

        action
    }

    /// Renders one conversation message (user question or assistant answer)
    /// with its status badge and per-answer actions (Copy / Retry / Save as
    /// Snippet via right-click on the answer).
    fn render_chat_message(
        ui: &mut egui::Ui,
        page: &mut HomePage,
        ai_box: &ContainerId,
        message: &Message,
        index: usize,
        action: &mut AiChatAction,
    ) {
        match message.role {
            MessageRole::User => {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    // A compact, left-aligned question block.
                    egui::Frame::NONE
                        .inner_margin(egui::Margin::symmetric(8, 5))
                        .corner_radius(egui::CornerRadius::same(4))
                        .fill(ui.visuals().widgets.inactive.bg_fill.gamma_multiply(0.55))
                        .show(ui, |ui| {
                            ui.label(&message.content);
                        });
                });
            }
            MessageRole::Assistant => {
                ui.add_space(6.0);
                let frame = egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(2, 4))
                    .show(ui, |ui| {
                        if message.content.is_empty() {
                            ui.label("…");
                        } else {
                            // Turn `[1]`, `[2]`, … citations into links to the
                            // sources actually used by this answer.
                            let linked = citation_linked_content(message);
                            let mut hook_urls: Vec<String> = Vec::new();
                            for source in &message.sources {
                                if let SourceTarget::Snippet(id) = &source.target {
                                    let url = format!("{}.md", id.as_str());
                                    page.ai_markdown_cache.add_link_hook(url.clone());
                                    hook_urls.push(url);
                                }
                            }
                            let _ = egui_commonmark::CommonMarkViewer::new()
                                .show(ui, &mut page.ai_markdown_cache, &linked);
                            // A clicked citation opens its source (read-only).
                            if let Some(url) = hook_urls.iter().find(|url| {
                                page.ai_markdown_cache.get_link_hook(url) == Some(true)
                            }) {
                                let id = url.trim_end_matches(".md");
                                *action = AiChatAction::OpenSource(EntityId::from_string(id));
                            }
                        }
                        // Compact footer: status badge, then the sources
                        // actually used by this answer (clickable, read-only).
                        if message.status != MessageStatus::Completed {
                            ui.add_space(4.0);
                            let (text, color) = match message.status {
                                MessageStatus::Stopped => ("STOPPED", ui.visuals().warn_fg_color),
                                MessageStatus::Failed => ("FAILED", ui.visuals().error_fg_color),
                                MessageStatus::Stale => ("SOURCE CHANGED", ui.visuals().warn_fg_color),
                                MessageStatus::Completed => unreachable!(),
                            };
                            ui.label(egui::RichText::new(text).small().color(color));
                        }
                        // Token usage reported by the provider for this answer.
                        if let Some(usage) = message.usage {
                            let mut parts = Vec::new();
                            if let Some(input) = usage.input_tokens {
                                parts.push(format!("in {input}"));
                            }
                            if let Some(output) = usage.output_tokens {
                                parts.push(format!("out {output}"));
                            }
                            if !parts.is_empty() {
                                ui.add_space(2.0);
                                ui.label(
                                    egui::RichText::new(format!("{} tokens", parts.join(" · ")))
                                        .small()
                                        .color(ui.visuals().weak_text_color()),
                                );
                            }
                        }
                        if !message.sources.is_empty() {
                            ui.add_space(2.0);
                            ui.horizontal_wrapped(|ui| {
                                for source in &message.sources {
                                    let label = egui::RichText::new(source.title.as_str())
                                        .small()
                                        .color(ui.visuals().weak_text_color());
                                    match &source.target {
                                        SourceTarget::Snippet(id) => {
                                            if ui.link(label).clicked() {
                                                *action = AiChatAction::OpenSource(id.clone());
                                            }
                                        }
                                        SourceTarget::Container(id) => {
                                            if ui.link(label).clicked() {
                                                *action = AiChatAction::OpenFolder(id.clone());
                                            }
                                        }
                                        SourceTarget::ExternalFile(_) => {
                                            if ui.link(label).clicked()
                                                && let Some(file) = page.find_external_file_ref(&source.target)
                                            {
                                                *action = AiChatAction::OpenExternalFile(file.clone());
                                            }
                                        }
                                    }
                                }
                            });
                        }
                        // Tool receipts are independent, visible events in the
                        // conversation (plan_ai.md §7.5/§9.8): the tool id and a
                        // short result summary, colored by status.
                        if !message.tools.is_empty() {
                            ui.add_space(2.0);
                            for tool in &message.tools {
                                let color = match tool.status {
                                    ToolStatus::Succeeded => ui.visuals().weak_text_color(),
                                    ToolStatus::Failed => ui.visuals().error_fg_color,
                                };
                                ui.label(
                                    egui::RichText::new(format!(
                                        "tool {}: {}",
                                        tool.tool_id, tool.summary
                                    ))
                                    .small()
                                    .color(color),
                                );
                            }
                        }
                    });
                // A model-proposed new Snippet (plan_ai.md §4.9), produced by the
                // `core.create_output_proposal` tool call: Apply/Reject card, or
                // its Saved/Rejected state after the user acted.
                if let Some(proposal) = &message.proposal {
                    Self::render_proposal_card(ui, page, ai_box, proposal, index, action);
                }
                // Actions live on the answer's own interaction region.
                frame.response.context_menu(|ui| {
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(message.content.clone());
                        ui.close();
                    }
                    if message.status != MessageStatus::Completed && ui.button("Retry").clicked() {
                        *action = AiChatAction::Retry;
                        ui.close();
                    }
                    if message.status == MessageStatus::Completed
                        && !message.content.trim().is_empty()
                        && ui.button("Save as Snippet").clicked()
                    {
                        *action = AiChatAction::SaveSnippet(index);
                        ui.close();
                    }
                });
            }
        }
    }

    /// Renders a model-proposed new Snippet (plan_ai.md §4.9): the title, a
    /// Markdown preview, the destination AI box, and Apply/Reject (or the
    /// Saved/Rejected state after the user acted).
    fn render_proposal_card(
        ui: &mut egui::Ui,
        page: &HomePage,
        ai_box: &ContainerId,
        proposal: &SnippetProposal,
        index: usize,
        action: &mut AiChatAction,
    ) {
        ui.add_space(4.0);
        egui::Frame::NONE
            .inner_margin(egui::Margin::same(8))
            .corner_radius(egui::CornerRadius::same(6))
            .stroke(egui::Stroke::new(1.0, ui.visuals().selection.stroke.color))
            .fill(ui.visuals().extreme_bg_color)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("Proposed new Snippet")
                        .small()
                        .strong()
                        .color(ui.visuals().selection.stroke.color),
                );
                ui.label(egui::RichText::new(&proposal.title).strong());
                let total = proposal.content.chars().count();
                if total > 0 {
                    let preview: String = proposal.content.chars().take(200).collect();
                    let preview = if total > 200 {
                        format!("{preview}…")
                    } else {
                        preview
                    };
                    ui.label(egui::RichText::new(preview).small());
                }
                let destination = page
                    .workspace
                    .containers
                    .get(ai_box)
                    .map(|container| container.title.as_str())
                    .unwrap_or("(deleted AI box)");
                ui.label(
                    egui::RichText::new(format!("Save to: {destination}"))
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
                ui.horizontal(|ui| {
                    if let Some(created) = &proposal.created {
                        ui.label(
                            egui::RichText::new("Saved")
                                .small()
                                .color(ui.visuals().hyperlink_color),
                        );
                        if ui.link("Open").clicked() {
                            *action = AiChatAction::OpenSource(created.clone());
                        }
                    } else if proposal.rejected {
                        ui.label(
                            egui::RichText::new("Rejected")
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    } else {
                        if ui.button("Apply").clicked() {
                            *action = AiChatAction::ApplyProposal(index);
                        }
                        if ui.button("Reject").clicked() {
                            *action = AiChatAction::RejectProposal(index);
                        }
                    }
                });
            });
    }

    // The `Sources: N` panel was removed for a simpler window header. Bound
    // sources are still used to build each turn's context and are shown per
    // answer; unbinding per-conversation sources is future work.

    // ---- chat actions ----

    fn ai_send(&mut self) {
        let Some((ai_box, conversation)) = self.ai_open.clone() else {
            return;
        };
        let text = self.ai_input.trim().to_owned();
        if text.is_empty() || self.ai_active_turn.is_some() {
            return;
        }
        let fail_identity = TurnIdentity {
            ai_box: ai_box.clone(),
            conversation: conversation.clone(),
            task: TurnTaskId::new(),
        };
        if !self.settings.ai_enabled {
            self.push_assistant(
                &fail_identity,
                "AI is disabled. Enable it in Settings first.",
                MessageStatus::Failed,
                Vec::new(),
                TokenUsage::default(),
                Vec::new(),
                None,
            );
            return;
        }
        let Some(provider) = self.provider_arc() else {
            self.push_assistant(
                &fail_identity,
                "Configure a model in Settings before sending.",
                MessageStatus::Failed,
                Vec::new(),
                TokenUsage::default(),
                Vec::new(),
                None,
            );
            return;
        };
        // Persist the user message before building the request so the turn
        // includes the question that was just asked.
        let now = now_unix();
        if let Some(data) = self.ai_boxes.get_mut(&ai_box) {
            data.push_message(&conversation, Message::user(text.clone()), now);
            let _ = self.ai_store.save_box(data);
        }
        // Filter sources by spatial proximity to the conversation card.
        let sources = self
            .ai_boxes
            .get(&ai_box)
            .and_then(|data| data.get(&conversation))
            .map(|conv| self.sources_near_conversation(&ai_box, &conversation, &conv.sources))
            .unwrap_or_default();
        let Some(request) = self.build_turn_request(&ai_box, &conversation, &sources) else {
            return;
        };
        // Capture the source snapshot at send time.
        let snapshots = self.source_snapshots(&ai_box, &sources);
        // The bounded tool scope captured at send time (only used when tools are
        // enabled; the model can never see anything outside this context).
        let (tools, tool_context) = if self.settings.ai_tools_enabled {
            (
                self.builtin_tools(),
                Some(self.tool_context_for(&ai_box, &sources)),
            )
        } else {
            (Vec::new(), None)
        };
        let identity = TurnIdentity {
            ai_box: ai_box.clone(),
            conversation: conversation.clone(),
            task: TurnTaskId::new(),
        };
        if self
            .ai_worker
            .submit(TurnRequest {
                identity: identity.clone(),
                request,
                provider,
                tools,
                tool_context,
            })
            .is_err()
        {
            self.push_assistant(
                &identity,
                "The AI worker is busy or shutting down.",
                MessageStatus::Failed,
                snapshots,
                TokenUsage::default(),
                Vec::new(),
                None,
            );
            return;
        }
        self.ai_input.clear();
        self.ai_active_turn = Some(identity);
        self.ai_streaming = String::new();
        self.ai_snapshots = snapshots;
    }

    fn ai_stop(&mut self) {
        let Some(active) = self.ai_active_turn.take() else {
            return;
        };
        self.ai_worker.cancel(&active.task);
        let partial = std::mem::take(&mut self.ai_streaming);
        let snapshots = std::mem::take(&mut self.ai_snapshots);
        let content = if partial.is_empty() { "(stopped)".to_owned() } else { partial };
        self.push_assistant(
            &active,
            &content,
            MessageStatus::Stopped,
            snapshots,
            TokenUsage::default(),
            Vec::new(),
            None,
        );
    }

    /// Retries the last turn: removes the failed/stopped assistant answer and
    /// re-sends the last user message as a fresh turn.
    fn ai_retry(&mut self) {
        let Some((ai_box, conversation)) = self.ai_open.clone() else {
            return;
        };
        if self.ai_active_turn.is_some() {
            return;
        }
        let mut last_user: Option<String> = None;
        if let Some(data) = self.ai_boxes.get_mut(&ai_box)
            && let Some(conv) = data.get_mut(&conversation)
        {
            while let Some(last) = conv.messages.last() {
                match last.role {
                    MessageRole::Assistant if last.status != MessageStatus::Completed => {
                        conv.messages.pop();
                    }
                    MessageRole::User => {
                        last_user = Some(last.content.clone());
                        break;
                    }
                    _ => break,
                }
            }
            let _ = self.ai_store.save_box(data);
        }
        if let Some(user) = last_user {
            self.ai_input = user;
            self.ai_send();
        }
    }

    /// Saves a completed assistant answer as a new ordinary snippet (`Output`).
    fn ai_save_as_snippet(&mut self, ai_box: &ContainerId, message_index: usize) {
        let Some((_, conversation)) = self.ai_open.clone() else {
            return;
        };
        let Some(message) = self
            .ai_boxes
            .get(ai_box)
            .and_then(|data| data.get(&conversation))
            .and_then(|conv| conv.messages.get(message_index))
            .cloned()
        else {
            return;
        };
        let content = message.content.trim();
        if content.is_empty() {
            return;
        }
        let title = content
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(|line| line.chars().take(40).collect::<String>())
            .unwrap_or_else(|| "AI Output".to_owned());
        let body = Self::output_content_with_sources(content, &message.sources);
        let _ = self.create_output_snippet(ai_box, title, body);
    }

    /// Builds the Markdown body saved with an assistant answer: the answer text
    /// followed by a `## Sources` section linking the sources it used. Snippet
    /// sources link with their stable `{id}.md`; container sources are listed by
    /// title (a container is not a page).
    fn output_content_with_sources(content: &str, sources: &[SourceRef]) -> String {
        let mut output = content.to_owned();
        if !sources.is_empty() {
            output.push_str("\n\n## Sources\n\n");
            for (index, source) in sources.iter().enumerate() {
                match &source.target {
                    SourceTarget::Snippet(id) => output.push_str(&format!(
                        "{}. [{}]({}.md)\n",
                        index + 1,
                        source.title,
                        id.as_str()
                    )),
                    SourceTarget::Container(_) => {
                        output.push_str(&format!("{}. {}\n", index + 1, source.title));
                    }
                    SourceTarget::ExternalFile(_) => {
                        output.push_str(&format!("{}. {} (external file)\n", index + 1, source.title));
                    }
                }
            }
        }
        output
    }

    /// Creates a new ordinary Snippet from an answer/proposal, saves it, adds an
    /// `Output` reference inside `ai_box` and places a card on that box's
    /// canvas. Runs on the UI thread (the unified command path, plan_ai.md
    /// §8.5); the `EntityId` is generated locally. Returns the new id.
    fn create_output_snippet(
        &mut self,
        ai_box: &ContainerId,
        title: String,
        content: String,
    ) -> Option<EntityId> {
        let snippet = Snippet {
            id: EntityId::new(),
            title,
            content,
        };
        let _ = self.store.save(&snippet);
        let snippet_id = snippet.id.clone();
        self.all_snippets.insert(snippet_id.clone(), snippet);
        // Create the `Output` reference inside the AI box.
        let Ok(reference_id) = self.workspace.add_output_reference(ai_box, snippet_id.clone())
        else {
            return None;
        };
        let _ = self.workspace_store.save(&self.workspace);
        let position = self
            .canvas_for(ai_box)
            .map(|canvas| {
                canvas::default_position_for(
                    ai_box,
                    &canvas.items,
                    &self.all_snippets,
                    &self.workspace,
                    &self.ai_boxes,
                    &canvas::approx_text_rects(&canvas.texts),
                )
            })
            .unwrap_or_else(|| default_card_position(0));
        let target_canvas = if ai_box == &self.root.container_id {
            Some(&mut self.root)
        } else {
            self.folder_views.get_mut(ai_box)
        };
        if let Some(canvas) = target_canvas {
            canvas.items.push(CanvasItem {
                reference_id: reference_id.clone(),
                target: ReferenceTarget::Snippet(snippet_id.clone()),
                role: MemberRole::Output,
                position,
                size: egui::vec2(CARD_WIDTH, 25.0),
            });
            canvas.layout.items.insert(
                reference_id,
                CardLayout {
                    position,
                    color: None,
                },
            );
            canvas.save_layout(&self.workspace_store);
        }
        Some(snippet_id)
    }

    /// Applies a model-proposed Snippet (plan_ai.md §4.9): commits the proposal
    /// through the unified output-creation path, then records the resulting
    /// `EntityId` on the message (the creation record). Idempotent: a proposal
    /// whose `created` is set is never committed twice.
    fn apply_proposal(&mut self, ai_box: &ContainerId, message_index: usize) {
        let Some((_, conversation)) = self.ai_open.clone() else {
            return;
        };
        let (title, content) = {
            let Some(data) = self.ai_boxes.get(ai_box) else {
                return;
            };
            let Some(conv) = data.get(&conversation) else {
                return;
            };
            let Some(message) = conv.messages.get(message_index) else {
                return;
            };
            let Some(proposal) = &message.proposal else {
                return;
            };
            if proposal.created.is_some() || proposal.rejected {
                return;
            }
            (proposal.title.clone(), proposal.content.clone())
        };
        let Some(created) = self.create_output_snippet(ai_box, title, content) else {
            return;
        };
        if let Some(data) = self.ai_boxes.get_mut(ai_box) {
            if let Some(conv) = data.get_mut(&conversation)
                && let Some(message) = conv.messages.get_mut(message_index)
                && let Some(proposal) = &mut message.proposal
            {
                proposal.created = Some(created);
            }
            let _ = self.ai_store.save_box(data);
        }
    }

    /// Rejects a model-proposed Snippet: marks the proposal as rejected so it no
    /// longer offers Apply. Never deletes snippets (an applied proposal is not
    /// un-created by a later reject).
    fn reject_proposal(&mut self, ai_box: &ContainerId, message_index: usize) {
        let Some((_, conversation)) = self.ai_open.clone() else {
            return;
        };
        if let Some(data) = self.ai_boxes.get_mut(ai_box) {
            if let Some(conv) = data.get_mut(&conversation)
                && let Some(message) = conv.messages.get_mut(message_index)
                && let Some(proposal) = &mut message.proposal
            {
                proposal.rejected = true;
            }
            let _ = self.ai_store.save_box(data);
        }
    }

    /// Appends an assistant message to the conversation sidecar, including the
    /// provider-reported token usage, every visible tool receipt and an
    /// optional model-proposed snippet (from `core.create_output_proposal`).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn push_assistant(
        &mut self,
        identity: &TurnIdentity,
        content: &str,
        status: MessageStatus,
        sources: Vec<SourceRef>,
        usage: TokenUsage,
        tools: Vec<ToolRecord>,
        proposal: Option<SnippetProposal>,
    ) {
        let now = now_unix();
        if let Some(data) = self.ai_boxes.get_mut(&identity.ai_box) {
            let mut message = Message::assistant(content.to_owned(), identity.task.clone());
            message.status = status;
            message.sources = sources;
            message.tools = tools;
            if usage.input_tokens.is_some() || usage.output_tokens.is_some() {
                message.usage = Some(usage);
            }
            message.proposal = proposal;
            data.push_message(&identity.conversation, message, now);
            let _ = self.ai_store.save_box(data);
        }
    }

    // ---- spatial source filtering ----

    /// Filters sources by spatial proximity to the conversation card on the AI
    /// box canvas. Returns the top 32 closest sources (by Euclidean distance).
    /// Falls back to the full list when the layout is not spatial, the
    /// conversation card is missing from the layout, or there are ≤ 32 sources.
    fn sources_near_conversation(
        &self,
        ai_box: &ContainerId,
        conversation: &ConversationId,
        sources: &[SourceTarget],
    ) -> Vec<SourceTarget> {
        if sources.len() <= 32 {
            return sources.to_vec();
        }
        let Ok(layout) = self.workspace_store.load_layout(ai_box) else {
            return sources.to_vec();
        };
        let has_arranged = layout.items.values().any(|card| card.position != [0.0, 0.0]);
        if !has_arranged {
            return sources.to_vec();
        }
        let Some(container) = self.workspace.containers.get(ai_box) else {
            return sources.to_vec();
        };
        let conversation_ref = container.members.iter().find(|member| {
            matches!(&member.target, ReferenceTarget::Conversation(id) if id == conversation)
                && member.role == MemberRole::Conversation
        });
        let Some(conversation_ref) = conversation_ref else {
            return sources.to_vec();
        };
        let Some(&conv_pos) = layout.items.get(&conversation_ref.id).map(|c| &c.position) else {
            return sources.to_vec();
        };
        let mut positioned: Vec<(SourceTarget, [f32; 2])> = Vec::new();
        for source in sources {
            let pos = container.members.iter().find_map(|member| {
                if member.role != MemberRole::Source {
                    return None;
                }
                let matched = match (source, &member.target) {
                    (SourceTarget::Snippet(id), ReferenceTarget::Snippet(mid)) => id == mid,
                    (SourceTarget::Container(id), ReferenceTarget::Container(mid)) => id == mid,
                    (SourceTarget::ExternalFile(id), ReferenceTarget::ExternalFile(file)) => {
                        id == &file.id
                    }
                    _ => false,
                };
                if matched {
                    layout.items.get(&member.id).map(|c| c.position)
                } else {
                    None
                }
            });
            positioned.push((source.clone(), pos.unwrap_or([0.0, 0.0])));
        }
        positioned.sort_by(|a, b| {
            let da = (a.1[0] - conv_pos[0]).powi(2) + (a.1[1] - conv_pos[1]).powi(2);
            let db = (b.1[0] - conv_pos[0]).powi(2) + (b.1[1] - conv_pos[1]).powi(2);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        });
        positioned.into_iter().take(32).map(|(s, _)| s).collect()
    }

    // ---- context building ----

    /// Builds the bounded chat request for one turn: the conversation's recent
    /// history, the given sources embedded in the system prompt and (when
    /// enabled) the built-in tool definitions.
    fn build_turn_request(
        &self,
        ai_box: &ContainerId,
        conversation: &ConversationId,
        sources: &[SourceTarget],
    ) -> Option<ChatRequest> {
        let conv = self.ai_boxes.get(ai_box)?.get(conversation)?;
        let mut messages = Vec::new();
        let history: Vec<&Message> = conv.messages.iter().rev().take(20).collect();
        for message in history.iter().rev() {
            match message.role {
                MessageRole::User => messages.push(ChatMessage::user(message.content.clone())),
                MessageRole::Assistant if !message.content.trim().is_empty() => {
                    messages.push(ChatMessage::assistant(message.content.clone()))
                }
                MessageRole::Assistant => {}
            }
        }
        let system = self.build_system_prompt(ai_box, sources);
        let tools = if self.settings.ai_tools_enabled {
            self.builtin_tools()
        } else {
            Vec::new()
        };
        let mut request = ChatRequest::new(system, messages).with_tools(tools);
        if sources.len() < conv.sources.len() {
            request = request.with_system_note(format!(
                "(Selected {} of {} sources based on spatial proximity to this conversation.)",
                sources.len(),
                conv.sources.len(),
            ));
        }
        Some(request)
    }

    fn build_system_prompt(
        &self,
        ai_box: &ContainerId,
        sources: &[SourceTarget],
    ) -> Option<String> {
        let mut parts = vec![
            "You are a knowledge assistant in FloatDea, a local-first notes app.".to_owned(),
            "Answer using ONLY the read-only sources below. Cite sources as [1], [2], … in your answer.".to_owned(),
            "Do not invent facts or sources. If the sources do not contain the answer, say so.".to_owned(),
            "When your answer is a self-contained deliverable (a new summary or a ready-to-use note), call the core.create_output_proposal tool with a title and the full Markdown body. Never embed proposal JSON in your reply.".to_owned(),
            "Never modify, append to, replace or delete any existing snippet; core.create_output_proposal only creates a brand-new Snippet proposal that the user must confirm before it is saved.".to_owned(),
            String::new(),
        ];
        let mut count = 0;
        let total = sources.len();
        let max_chars = adaptive_source_limit(total);
        for target in sources {
            if let Some((title, text)) = self.resolve_source_text(target, ai_box, max_chars) {
                count += 1;
                parts.push(format!("[{count}] {title}\n{text}"));
            }
        }
        log::info!("built system prompt: {count}/{total} sources resolved");
        if count == 0 {
            parts.push("(No sources are bound to this conversation yet.)".to_owned());
        }
        Some(parts.join("\n"))
    }

    /// Resolves a bound source to its display title and content. Containers
    /// expand to their direct snippet members (non-recursive, bounded).
    /// `max_chars` is the adaptive per-source limit calculated from the total
    /// number of sources (see [`adaptive_source_limit`]).
    fn resolve_source_text(&self, target: &SourceTarget, _ai_box: &ContainerId, max_chars: usize) -> Option<(String, String)> {
        match target {
            SourceTarget::Snippet(id) => {
                let snippet = self.all_snippets.get(id)?;
                let text: String = snippet.content.chars().take(max_chars).collect();
                Some((snippet.title.clone(), text))
            }
            SourceTarget::Container(id) => {
                let container = self.workspace.containers.get(id)?;
                let mut parts = Vec::new();
                for member in &container.members {
                    if let ReferenceTarget::Snippet(member_id) = &member.target
                        && let Some(snippet) = self.all_snippets.get(member_id)
                    {
                        parts.push(format!("- {}:\n{}", snippet.title, snippet.content));
                    }
                }
                let text: String = parts.join("\n\n").chars().take(max_chars).collect();
                Some((format!("Folder {}", container.title), text))
            }
            SourceTarget::ExternalFile(id) => {
                let Some(file) = self.find_external_file(id) else {
                    log::warn!("ExternalFile source not found in workspace: {}", id.as_str());
                    return None;
                };
                // Use the cache if available; otherwise extract synchronously
                // and cache the result. The cache is pre-populated in the
                // background after conversation creation (see
                // `spawn_content_extraction`).
                let text = if let Some(cached) = self.file_content_cache.lock().ok().and_then(|c| c.get(id).cloned()) {
                    cached
                } else {
                    match extract_text_from_file(&file.path) {
                        Some(text) => {
                            if let Ok(mut cache) = self.file_content_cache.lock() {
                                cache.insert(id.clone(), text.clone());
                            }
                            text
                        }
                        None => {
                            log::warn!("could not extract content from external file: {} ({})", file.title, file.path);
                            return Some((file.title.clone(), "[content unavailable: file could not be read]".to_owned()));
                        }
                    }
                };
                let text: String = text.chars().take(max_chars).collect();
                Some((file.title.clone(), text))
            }
        }
    }

    /// Captures the actual sources (target, title, content hash) at send time.
    fn source_snapshots(&self, ai_box: &ContainerId, sources: &[SourceTarget]) -> Vec<SourceRef> {
        let mut snapshots = Vec::new();
        let max_chars = adaptive_source_limit(sources.len());
        for target in sources {
            if let Some((title, text)) = self.resolve_source_text(target, ai_box, max_chars) {
                snapshots.push(SourceRef {
                    target: target.clone(),
                    title,
                    content_hash: Some(content_hash(&text)),
                });
            }
        }
        snapshots
    }

    /// The built-in tool definitions the model may call (plan_ai.md §9.8).
    fn builtin_tools(&self) -> Vec<ToolDef> {
        ToolRegistry::builtins().definitions().to_vec()
    }

    /// Captures the bounded tool context at send time: only the sources given,
    /// with their title, content and content hash. Tools can never read anything
    /// outside this list.
    fn tool_context_for(&self, ai_box: &ContainerId, sources: &[SourceTarget]) -> ToolContext {
        let mut context = Vec::new();
        let max_chars = adaptive_source_limit(sources.len());
        let mut index = 0u32;
        for target in sources {
            if let Some((title, text)) = self.resolve_source_text(target, ai_box, max_chars) {
                index += 1;
                let content_hash = content_hash(&text);
                let file_path = match target {
                    SourceTarget::ExternalFile(id) => {
                        self.find_external_file(id).map(|f| f.path.clone())
                    }
                    _ => None,
                };
                context.push(BoundSource {
                    index,
                    target: target.clone(),
                    title,
                    content: text,
                    content_hash,
                    file_path,
                });
            }
        }
        ToolContext { sources: context }
    }

    /// The model name shown in the conversation header (provider type stays in
    /// Settings).
    fn provider_label(&self) -> String {
        if self.settings.ai_enabled {
            self.settings.ai_model.clone()
        } else {
            "AI off".to_owned()
        }
    }

    /// Builds the configured provider (fake for tests/offline, genai for
    /// remote/local services). Tests may override it with a scripted provider.
    fn provider_arc(&self) -> Option<Arc<dyn ChatProvider>> {
        if let Some(provider) = &self.ai_provider_override {
            return Some(provider.clone());
        }
        let config = self.settings.ai_provider_config();
        build_provider(&config).ok().map(Arc::from)
    }
}

/// Rewrites `[n]` citation markers in an assistant answer into links to the
/// `n`-th source actually used by that answer (`[{n}]({id}.md)`). Container
/// sources and out-of-range numbers are left as plain text. Non-citation
/// bracket pairs (markdown links, code) are untouched.
fn citation_linked_content(message: &Message) -> String {
    let sources = &message.sources;
    if sources.is_empty() {
        return message.content.clone();
    }
    let mut out = String::new();
    let chars: Vec<char> = message.content.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            // Scan a closing `]` within a few characters.
            let mut close = None;
            let mut j = i + 1;
            while j < chars.len() && j <= i + 6 {
                if chars[j] == ']' {
                    close = Some(j);
                    break;
                }
                if !chars[j].is_ascii_digit() {
                    break;
                }
                j += 1;
            }
            if let Some(close_index) = close {
                let number: String = chars[i + 1..close_index].iter().collect();
                if let Ok(n) = number.parse::<usize>()
                    && n >= 1
                    && let Some(source) = sources.get(n - 1)
                    && let SourceTarget::Snippet(id) = &source.target
                {
                    out.push_str(&format!("[{n}]({}.md)", id.as_str()));
                    i = close_index + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

use super::*;
    use floatdea::data::ai::provider::FakeProvider;

    struct TestFolder(PathBuf);

    impl TestFolder {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "floatdea-ai-chat-{}-{nonce}",
                std::process::id()
            )))
        }
    }

    impl Drop for TestFolder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Creates an AI box + a fresh conversation bound to one snippet, and opens
    /// the conversation.
    fn open_fake_conversation(page: &mut HomePage) -> (ContainerId, ConversationId, EntityId) {
        let root = page.root.container_id.clone();
        page.process_canvas_commands(
            vec![CanvasCommand::NewAiBox {
                owner: root,
                position: None,
            }],
            egui::ViewportId::ROOT,
        );
        let ai_box = page
            .workspace
            .containers
            .values()
            .find(|container| container.kind == ContainerKind::AiWorkspace)
            .expect("AI box exists")
            .id
            .clone();
        let entity = page
            .root
            .items
            .iter()
            .find_map(|item| match &item.target {
                ReferenceTarget::Snippet(id) => Some(id.clone()),
                _ => None,
            })
            .expect("a snippet card exists");
        page.process_canvas_commands(
            vec![CanvasCommand::LinkAiSource {
                ai_box: ai_box.clone(),
                target: ReferenceTarget::Snippet(entity.clone()),
                position: None,
            }],
            egui::ViewportId::ROOT,
        );
        page.process_canvas_commands(
            vec![CanvasCommand::NewConversation {
                ai_box: ai_box.clone(),
                position: None,
            }],
            egui::ViewportId::ROOT,
        );
        let conversation = page.ai_boxes[&ai_box]
            .conversations
            .keys()
            .next()
            .cloned()
            .expect("conversation created");
        page.ai_open = Some((ai_box.clone(), conversation.clone()));
        (ai_box, conversation, entity)
    }

    #[test]
    fn send_turn_streams_and_persists_messages_with_fake_provider() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        page.settings.ai_enabled = true; // default provider is the offline Fake
        let (ai_box, conversation, _) = open_fake_conversation(&mut page);

        page.ai_input = "Hello from the test".to_owned();
        page.ai_send();
        assert!(page.ai_active_turn.is_some());
        assert!(page.ai_input.is_empty());

        // Drain worker events until the turn completes (offline fake provider).
        let deadline = Instant::now() + Duration::from_secs(10);
        while page.ai_active_turn.is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
            page.drain_ai_events();
        }
        assert!(
            page.ai_active_turn.is_none(),
            "the fake turn completes within the deadline"
        );

        let conversation_data = page.ai_boxes[&ai_box].get(&conversation).unwrap();
        assert_eq!(conversation_data.messages.len(), 2);
        assert_eq!(conversation_data.messages[0].role, MessageRole::User);
        assert_eq!(conversation_data.messages[0].content, "Hello from the test");
        assert_eq!(conversation_data.messages[1].role, MessageRole::Assistant);
        assert!(
            conversation_data.messages[1].content.contains("Hello from the test"),
            "assistant content was: {:?}",
            conversation_data.messages[1].content
        );
        // The fake provider reports usage; it is persisted on the message.
        let usage = conversation_data.messages[1]
            .usage
            .expect("assistant message records token usage");
        assert_eq!(usage.input_tokens, Some(12));
        assert!(usage.output_tokens.is_some());

        // Ordinary conversations survive a restart.
        drop(page);
        let reloaded = HomePage::new(&folder.0);
        assert_eq!(
            reloaded.ai_boxes[&ai_box].get(&conversation).unwrap().messages.len(),
            2
        );
    }

    #[test]
    fn save_snippet_creates_an_output_reference_in_the_ai_box() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let (ai_box, conversation, entity) = open_fake_conversation(&mut page);

        let mut message = Message::assistant("Summary line\n\nDetails", TurnTaskId::new());
        message.sources = vec![SourceRef {
            target: SourceTarget::Snippet(entity),
            title: "hello".to_owned(),
            content_hash: Some("abc".to_owned()),
        }];
        page.ai_boxes
            .get_mut(&ai_box)
            .unwrap()
            .push_message(&conversation, message, now_unix());

        page.ai_save_as_snippet(&ai_box, 0);

        let outputs: Vec<&Snippet> = page
            .all_snippets
            .values()
            .filter(|snippet| snippet.content.contains("## Sources"))
            .collect();
        assert_eq!(outputs.len(), 1, "a new output snippet was created");
        let output = outputs[0];
        assert!(output.content.contains("[hello]("), "output links its sources");
        assert!(output.content.contains(".md)"));
        assert!(
            page.workspace.containers[&ai_box]
                .members
                .iter()
                .any(|reference| {
                    reference.role == MemberRole::Output
                        && matches!(&reference.target, ReferenceTarget::Snippet(id) if id == &output.id)
                }),
            "the AI box holds an Output reference to the new snippet"
        );
    }

    #[test]
    fn retry_removes_the_failed_answer_and_resends_the_question() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        page.settings.ai_enabled = true;
        let (ai_box, conversation, _) = open_fake_conversation(&mut page);

        // A failed assistant answer following the user question.
        {
            let data = page.ai_boxes.get_mut(&ai_box).unwrap();
            let now = now_unix();
            data.push_message(&conversation, Message::user("question"), now);
            let mut failed = Message::assistant("oops", TurnTaskId::new());
            failed.status = MessageStatus::Failed;
            data.push_message(&conversation, failed, now);
        }

        page.ai_retry();

        // The failed answer was replaced by a fresh completed one; the question
        // is re-sent as a new turn.
        let deadline = Instant::now() + Duration::from_secs(10);
        while page.ai_active_turn.is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
            page.drain_ai_events();
        }
        let conversation_data = page.ai_boxes[&ai_box].get(&conversation).unwrap();
        assert_eq!(conversation_data.messages.len(), 3);
        assert_eq!(conversation_data.messages[0].role, MessageRole::User);
        assert_eq!(conversation_data.messages[1].role, MessageRole::User);
        assert_eq!(conversation_data.messages[2].role, MessageRole::Assistant);
        assert_eq!(conversation_data.messages[2].status, MessageStatus::Completed);
    }

    #[test]
    fn citation_links_are_built_from_this_answers_sources() {
        let entity = EntityId::new();
        let mut message = Message::assistant(
            "See [1] and [2]; [3] is out of range and [text](https://example.com) is a link.",
            TurnTaskId::new(),
        );
        message.sources = vec![
            SourceRef {
                target: SourceTarget::Snippet(entity.clone()),
                title: "one".to_owned(),
                content_hash: None,
            },
            SourceRef {
                target: SourceTarget::Container(ContainerId::new()),
                title: "two".to_owned(),
                content_hash: None,
            },
        ];

        let linked = citation_linked_content(&message);

        assert!(
            linked.contains(&format!("[1]({}.md)", entity.as_str())),
            "snippet citations become links: {linked}"
        );
        assert!(
            linked.contains("[2]"),
            "container citations stay as plain text"
        );
        assert!(linked.contains("[3] is out of range"));
        assert!(linked.contains("[text](https://example.com)"));
    }

    #[test]
    fn linking_a_folder_brings_its_members_into_context() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let root = page.root.container_id.clone();
        page.process_canvas_commands(
            vec![CanvasCommand::NewAiBox {
                owner: root.clone(),
                position: None,
            }],
            egui::ViewportId::ROOT,
        );
        let ai_box = page
            .workspace
            .containers
            .values()
            .find(|container| container.kind == ContainerKind::AiWorkspace)
            .expect("AI box exists")
            .id
            .clone();
        let knowledge = page.workspace.create_container("Knowledge");
        let member = first_snippet_id(&page);
        page.workspace
            .add_snippet_reference(&knowledge, member.clone())
            .unwrap();

        // Link the folder itself as a Source.
        page.process_canvas_commands(
            vec![CanvasCommand::LinkAiSource {
                ai_box: ai_box.clone(),
                target: ReferenceTarget::Container(knowledge.clone()),
                position: None,
            }],
            egui::ViewportId::ROOT,
        );
        assert!(page.workspace.containers[&ai_box].members.iter().any(|reference| {
            reference.role == MemberRole::Source
                && matches!(&reference.target, ReferenceTarget::Container(id) if id == &knowledge)
        }));

        // A new conversation binds the folder source; the turn request embeds
        // the folder's direct snippet members into the system prompt.
        page.process_canvas_commands(
            vec![CanvasCommand::NewConversation {
                ai_box: ai_box.clone(),
                position: None,
            }],
            egui::ViewportId::ROOT,
        );
        let conversation = page.ai_boxes[&ai_box]
            .conversations
            .keys()
            .next()
            .cloned()
            .unwrap();
        let sources = &page.ai_boxes[&ai_box].get(&conversation).unwrap().sources;
        let request = page
            .build_turn_request(&ai_box, &conversation, sources)
            .expect("a turn request builds");
        let system = request.system.expect("system prompt");
        assert!(system.contains("[1] Folder Knowledge"));
        assert!(system.contains(&page.all_snippets[&member].content));

        let snapshots = page.source_snapshots(&ai_box, sources);
        assert_eq!(snapshots.len(), 1);
        assert!(matches!(snapshots[0].target, SourceTarget::Container(_)));
    }

    #[test]
    fn tool_loop_creates_a_proposal_and_records_the_tool_receipt() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        page.settings.ai_enabled = true;
        // Script the fake provider: round 1 requests `core.create_output_proposal`,
        // round 2 streams the final answer.
        page.ai_provider_override = Some(Arc::from(FakeProvider::tool_proposal(
            "Draft Note",
            "# Draft Note\n\nDraft body",
            "Here is the final summary.",
        )));
        let (ai_box, conversation, _) = open_fake_conversation(&mut page);

        page.ai_input = "Summarize the sources".to_owned();
        page.ai_send();
        let deadline = Instant::now() + Duration::from_secs(10);
        while page.ai_active_turn.is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
            page.drain_ai_events();
        }
        assert!(page.ai_active_turn.is_none(), "the tool turn completes");

        let data = page.ai_boxes[&ai_box].get(&conversation).unwrap();
        assert_eq!(data.messages.len(), 2);
        let answer = &data.messages[1];
        assert!(
            answer.content.contains("final summary"),
            "the final answer comes from the continuation round: {:?}",
            answer.content
        );
        // The tool receipt is a visible, persisted event.
        assert_eq!(answer.tools.len(), 1);
        assert_eq!(answer.tools[0].tool_id, "core.create_output_proposal");
        assert_eq!(answer.tools[0].status, ToolStatus::Succeeded);
        assert!(
            answer.tools[0].summary.contains("Draft Note"),
            "receipt summarises the proposal: {}",
            answer.tools[0].summary
        );
        // The proposal card data is attached to the answer.
        let proposal = answer.proposal.as_ref().expect("a proposal is stored");
        assert_eq!(proposal.title, "Draft Note");
        assert!(proposal.content.contains("# Draft Note"));
        assert!(proposal.created.is_none());
        // No raw proposal JSON leaks into the visible answer.
        assert!(!answer.content.contains("proposal"));
    }

    #[test]
    fn tool_loop_is_skipped_when_tools_are_disabled() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        page.settings.ai_enabled = true;
        page.settings.ai_tools_enabled = false;
        page.ai_provider_override = Some(Arc::from(FakeProvider::tool_proposal(
            "Draft Note",
            "Body",
            "Answer without tools",
        )));
        let (ai_box, conversation, _) = open_fake_conversation(&mut page);

        page.ai_input = "Hi".to_owned();
        page.ai_send();
        let deadline = Instant::now() + Duration::from_secs(10);
        while page.ai_active_turn.is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
            page.drain_ai_events();
        }

        let data = page.ai_boxes[&ai_box].get(&conversation).unwrap();
        let answer = &data.messages[1];
        // With tools disabled the request carries no tool definitions, so the
        // scripted fake behaves like a plain provider (no tool call, no loop).
        assert!(answer.tools.is_empty());
        assert!(answer.proposal.is_none());
        assert!(
            answer.content.contains("Fake provider"),
            "the plain fake reply is used when tools are off: {:?}",
            answer.content
        );
    }

    #[test]
    fn apply_proposal_creates_an_output_snippet_and_records_creation() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let (ai_box, conversation, _) = open_fake_conversation(&mut page);

        let mut message = Message::assistant("Proposal text", TurnTaskId::new());
        message.proposal = Some(SnippetProposal::new("Draft Note", "# Draft Note\n\nDraft body"));
        page.ai_boxes
            .get_mut(&ai_box)
            .unwrap()
            .push_message(&conversation, message, now_unix());

        page.apply_proposal(&ai_box, 0);

        let data = page.ai_boxes[&ai_box].get(&conversation).unwrap();
        let proposal = data.messages[0]
            .proposal
            .as_ref()
            .expect("the proposal is retained on the message");
        let created = proposal.created.clone().expect("the creation is recorded");
        assert_eq!(page.all_snippets[&created].title, "Draft Note");
        assert!(page.all_snippets[&created].content.contains("# Draft Note"));
        assert!(
            page.workspace.containers[&ai_box]
                .members
                .iter()
                .any(|reference| {
                    reference.role == MemberRole::Output
                        && matches!(&reference.target, ReferenceTarget::Snippet(id) if id == &created)
                }),
            "the AI box holds an Output reference to the committed snippet"
        );
    }

    #[test]
    fn apply_proposal_is_idempotent() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let (ai_box, conversation, _) = open_fake_conversation(&mut page);

        let mut message = Message::assistant("Proposal text", TurnTaskId::new());
        message.proposal = Some(SnippetProposal::new("Draft Note", "Body"));
        page.ai_boxes
            .get_mut(&ai_box)
            .unwrap()
            .push_message(&conversation, message, now_unix());

        page.apply_proposal(&ai_box, 0);
        page.apply_proposal(&ai_box, 0);

        let created = page.ai_boxes[&ai_box]
            .get(&conversation)
            .unwrap()
            .messages[0]
            .proposal
            .as_ref()
            .unwrap()
            .created
            .clone()
            .expect("the creation is recorded");
        let drafts: Vec<&Snippet> = page
            .all_snippets
            .values()
            .filter(|snippet| snippet.id == created)
            .collect();
        assert_eq!(
            drafts.len(),
            1,
            "double apply must not create a second snippet"
        );
    }

    #[test]
    fn reject_proposal_creates_nothing() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let (ai_box, conversation, _) = open_fake_conversation(&mut page);

        let mut message = Message::assistant("Proposal text", TurnTaskId::new());
        message.proposal = Some(SnippetProposal::new("Draft Note", "Body"));
        page.ai_boxes
            .get_mut(&ai_box)
            .unwrap()
            .push_message(&conversation, message, now_unix());

        page.reject_proposal(&ai_box, 0);

        let data = page.ai_boxes[&ai_box].get(&conversation).unwrap();
        let proposal = data.messages[0].proposal.as_ref().unwrap();
        assert!(proposal.rejected);
        assert!(proposal.created.is_none());
        assert!(
            page.all_snippets
                .values()
                .all(|snippet| snippet.title != "Draft Note"),
            "rejecting must not create a snippet"
        );
        assert!(
            !page.workspace.containers[&ai_box]
                .members
                .iter()
                .any(|reference| reference.role == MemberRole::Output),
            "rejecting must not add an Output reference"
        );
    }

    #[test]
    fn applied_proposal_and_created_snippet_survive_restart() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let (ai_box, conversation, _) = open_fake_conversation(&mut page);

        let mut message = Message::assistant("Proposal text", TurnTaskId::new());
        message.proposal = Some(SnippetProposal::new("Draft Note", "Body"));
        page.ai_boxes
            .get_mut(&ai_box)
            .unwrap()
            .push_message(&conversation, message, now_unix());
        page.apply_proposal(&ai_box, 0);
        let created = page.ai_boxes[&ai_box]
            .get(&conversation)
            .unwrap()
            .messages[0]
            .proposal
            .as_ref()
            .unwrap()
            .created
            .clone()
            .unwrap();

        drop(page);
        let reloaded = HomePage::new(&folder.0);
        let data = reloaded.ai_boxes[&ai_box].get(&conversation).unwrap();
        let proposal = data.messages[0].proposal.as_ref().unwrap();
        assert_eq!(proposal.created.as_ref(), Some(&created));
        assert!(
            reloaded.all_snippets.contains_key(&created),
            "the committed snippet is an ordinary Markdown entity after restart"
        );
        assert!(
            reloaded.workspace.containers[&ai_box]
                .members
                .iter()
                .any(|reference| {
                    reference.role == MemberRole::Output
                        && matches!(&reference.target, ReferenceTarget::Snippet(id) if id == &created)
                }),
            "the Output reference survives the restart"
        );
    }

    fn first_snippet_id(page: &HomePage) -> EntityId {
        page.root
            .items
            .iter()
            .find_map(|item| match &item.target {
                ReferenceTarget::Snippet(id) => Some(id.clone()),
                _ => None,
            })
            .expect("root has a snippet card")
    }

    #[test]
    fn spatial_filter_selects_sources_near_conversation() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let root = page.root.container_id.clone();

        page.process_canvas_commands(
            vec![CanvasCommand::NewAiBox {
                owner: root,
                position: None,
            }],
            egui::ViewportId::ROOT,
        );
        let ai_box = page
            .workspace
            .containers
            .values()
            .find(|container| container.kind == ContainerKind::AiWorkspace)
            .expect("AI box exists")
            .id
            .clone();

        // Open the AI box canvas so card positions are tracked.
        page.open_folder(&ai_box);

        // Create 33 snippets and link them all as sources.
        let mut source_ids = Vec::new();
        for i in 0..33 {
            let snippet = Snippet {
                id: EntityId::new(),
                title: format!("source_{i}"),
                content: format!("content_{i}"),
            };
            let sid = snippet.id.clone();
            let _ = page.store.save(&snippet);
            page.all_snippets.insert(sid.clone(), snippet);
            page.workspace.add_snippet_to_root(sid.clone());
            page.process_canvas_commands(
                vec![CanvasCommand::LinkAiSource {
                    ai_box: ai_box.clone(),
                    target: ReferenceTarget::Snippet(sid.clone()),
                    position: None,
                }],
                egui::ViewportId::ROOT,
            );
            source_ids.push(sid);
        }

        // Create the conversation (captures all 33 sources).
        page.process_canvas_commands(
            vec![CanvasCommand::NewConversation {
                ai_box: ai_box.clone(),
                position: None,
            }],
            egui::ViewportId::ROOT,
        );
        let conversation = page.ai_boxes[&ai_box]
            .conversations
            .keys()
            .next()
            .cloned()
            .expect("conversation created");

        // Set positions on the AI box canvas: conversation at (0,0), first 16
        // sources nearby (position.y < 200), last 17 far away (position.y > 1000).
        let canvas = page.folder_views.get_mut(&ai_box).expect("AI box canvas");
        for (i, item) in canvas.items.iter_mut().enumerate() {
            if item.role == MemberRole::Conversation {
                item.position = [0.0, 0.0];
            } else if i < 16 + 1 {
                // Skip the conversation card at index 0; sources 1..17 are near.
                item.position = [0.0, 50.0 * i as f32];
            } else {
                item.position = [0.0, 1000.0 + 50.0 * i as f32];
            }
        }
        canvas.save_layout(&page.workspace_store);

        let near_ids: std::collections::HashSet<EntityId> =
            source_ids.iter().take(16).cloned().collect();
        let sources: Vec<SourceTarget> = source_ids
            .into_iter()
            .map(SourceTarget::Snippet)
            .collect();
        let filtered = page.sources_near_conversation(&ai_box, &conversation, &sources);

        assert_eq!(filtered.len(), 32, "filters to 32 sources");
        // The first 16 sources (indices 0..15) are positioned near the
        // conversation (y < 800); the remaining 17 are far (y > 1800).
        // The 32 closest should include all 16 near ones.
        for source in filtered.iter().take(16) {
            let SourceTarget::Snippet(id) = source else {
                panic!("expected snippet source");
            };
            assert!(
                near_ids.contains(id),
                "filtered source should be among the near ones: got {}",
                id.as_str()
            );
        }
    }

    #[test]
    fn spatial_filter_falls_back_when_positions_are_all_zero() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let root = page.root.container_id.clone();

        page.process_canvas_commands(
            vec![CanvasCommand::NewAiBox {
                owner: root,
                position: None,
            }],
            egui::ViewportId::ROOT,
        );
        let ai_box = page
            .workspace
            .containers
            .values()
            .find(|container| container.kind == ContainerKind::AiWorkspace)
            .expect("AI box exists")
            .id
            .clone();

        page.open_folder(&ai_box);

        // Create 33 sources but never set any position (all default to 0,0).
        let mut sources = Vec::new();
        for i in 0..33 {
            let snippet = Snippet {
                id: EntityId::new(),
                title: format!("s{i}"),
                content: format!("c{i}"),
            };
            let sid = snippet.id.clone();
            let _ = page.store.save(&snippet);
            page.all_snippets.insert(sid.clone(), snippet);
            page.workspace.add_snippet_to_root(sid.clone());
            page.process_canvas_commands(
                vec![CanvasCommand::LinkAiSource {
                    ai_box: ai_box.clone(),
                    target: ReferenceTarget::Snippet(sid.clone()),
                    position: None,
                }],
                egui::ViewportId::ROOT,
            );
            sources.push(SourceTarget::Snippet(sid));
        }

        page.process_canvas_commands(
            vec![CanvasCommand::NewConversation {
                ai_box: ai_box.clone(),
                position: None,
            }],
            egui::ViewportId::ROOT,
        );
        let conversation = page.ai_boxes[&ai_box]
            .conversations
            .keys()
            .next()
            .cloned()
            .expect("conversation created");

        // Force all layout positions to (0,0).
        if let Some(canvas) = page.folder_views.get_mut(&ai_box) {
            for item in &mut canvas.items {
                item.position = [0.0, 0.0];
            }
            canvas.save_layout(&page.workspace_store);
        }

        let filtered = page.sources_near_conversation(&ai_box, &conversation, &sources);
        assert_eq!(filtered.len(), 33, "all sources returned when positions are all zero");
    }

    #[test]
    fn spatial_filter_falls_back_when_32_or_fewer_sources() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let (ai_box, conversation, _) = open_fake_conversation(&mut page);

        let sources = page.ai_boxes[&ai_box]
            .get(&conversation)
            .unwrap()
            .sources
            .clone();
        // Only 1 source, so no filtering.
        let filtered = page.sources_near_conversation(&ai_box, &conversation, &sources);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0], sources[0]);
    }
}
