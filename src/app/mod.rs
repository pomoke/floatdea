use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::{App, egui};
use tokio::sync::mpsc;

use floatdea::data::{
    ContainerId, ContainerKind, ConversationId, EntityId, ExternalFileId, ReferenceId, Snippet,
    TextId, TurnTaskId,
    ai::{
        AiBoxData, AiErrorKind, AiStore, AiWorker, BoundSource, ChatMessage, ChatProvider,
        ChatRequest, Conversation, Message, MessageRole, MessageStatus, ProviderKind,
        SnippetProposal, SourceRef, SourceTarget, TokenUsage, ToolContext, ToolDef, ToolRecord,
        ToolRegistry, ToolStatus, TurnEvent, TurnIdentity, TurnRequest, build_provider,
        content_hash, now_unix,
    },
    settings::{Settings, SettingsStore, ThemeSetting, WindowMode},
    storage::SnippetStore,
    workspace::{
        CanvasText, CardLayout, Container, ContainerLayout, ExternalFileRef, MemberRole,
        Reference, ReferenceTarget, SpecialKind, Workspace, WorkspaceStore,
    },
};

mod ai_chat;
mod canvas;
mod math;
mod settings;
mod snippet;

use math::MathRenderer;

const CANVAS_MARGIN: f32 = 0.0;
const CARD_WIDTH: f32 = 150.0;
const CARD_PADDING_H: f32 = 8.0;
const CARD_MARGIN_Y: f32 = 6.0;

/// The fixed size of the root canvas; also the main-window inner size in native
/// multi-window mode.
pub(crate) const ROOT_CANVAS_SIZE: [f32; 2] = [640.0, 480.0];
/// The larger main-window inner size used in full-window mode to leave room for
/// the floating snippet/folder windows.
pub(crate) const FLOATING_MAIN_SIZE: [f32; 2] = [1280.0, 800.0];
/// How long the "could not open external file" toast stays visible (frames).
const EXTERNAL_OPEN_ERROR_FRAMES: u32 = 180;

pub(crate) struct HomePage {
    all_snippets: BTreeMap<EntityId, Snippet>,
    store: SnippetStore,
    workspace: Workspace,
    workspace_store: WorkspaceStore,
    root: ContainerCanvas,
    folder_views: BTreeMap<ContainerId, ContainerCanvas>,
    views: Vec<View>,
    next_view_id: u64,
    pending_delete: Option<PendingDelete>,
    rename_dialog: RenameDialogState,
    /// Global search over all snippets (opened by `Ctrl+F`, `/`, or the box
    /// context menu).
    search: SearchState,
    clipboard: Option<ClipboardEntry>,
    /// Shared local TeX-to-SVG renderer for previews and document embeds.
    math_renderer: MathRenderer,
    /// Reference drops consumed by `DropOnCanvas` commands this frame; used by
    /// [`HomePage::finalize_drops`] to decide whether a dangling drag should
    /// bounce back to its start position.
    consumed_drops: BTreeSet<(ContainerId, ReferenceId)>,
    /// Persisted user settings (theme, preview font size, math cap, grid).
    settings: Settings,
    settings_store: SettingsStore,
    /// Whether the system settings window is open.
    settings_open: bool,
    /// Full-window mode: the root window's close button was clicked and the
    /// "Exit?" confirmation is awaiting a decision.
    root_exit_pending: bool,
    /// AI sidecar store (conversations, messages, source snapshots).
    ai_store: AiStore,
    /// In-memory cache of per-AI-box sidecar data, kept in sync with
    /// [`HomePage::ai_store`].
    ai_boxes: BTreeMap<ContainerId, AiBoxData>,
    /// The single shared AI worker and its event stream (one per app).
    ai_worker: AiWorker,
    ai_events: mpsc::Receiver<TurnEvent>,
    /// The currently open conversation window: `(ai_box, conversation)`.
    ai_open: Option<(ContainerId, ConversationId)>,
    /// Input buffer of the open conversation.
    ai_input: String,
    /// The running turn (matches streaming events; one per conversation).
    ai_active_turn: Option<TurnIdentity>,
    /// Transient streaming text of the active turn.
    ai_streaming: String,
    /// Source snapshots captured when the active turn was sent.
    ai_snapshots: Vec<SourceRef>,
    /// CommonMark cache for the open conversation's assistant answers.
    ai_markdown_cache: egui_commonmark::CommonMarkCache,
    /// Test hook: overrides the provider built from settings so tests can script
    /// the fake provider (tool loops, failures). Always `None` in production.
    ai_provider_override: Option<Arc<dyn ChatProvider>>,
    /// Transient "could not open external file" toast: message, originating
    /// viewport, and remaining frames (auto-dismissed after a short while).
    external_open_error: Option<(String, egui::ViewportId, u32)>,
    /// Per-frame guard so an OS-level file drop (a single frame-global
    /// `dropped_files` event shared by every canvas of the viewport) is
    /// consumed by exactly one canvas. Reset at the start of each frame.
    os_file_drop_consumed: bool,
    /// Pending file picker result: when the user clicks "Insert External File…",
    /// a thread is spawned to run the blocking `rfd` file dialog. The result is
    /// received here on the next frame and processed immediately.
    pending_file_picker: Option<ExternalFilePending>,
    /// Cache of extracted text content for external files, keyed by
    /// `ExternalFileId`. Populated in the background after a conversation is
    /// created, so PDF extraction never blocks the UI thread. Uses `Mutex` for
    /// interior mutability so the background thread can write into it.
    file_content_cache: Arc<Mutex<HashMap<ExternalFileId, String>>>,
    /// Hover-to-preview state for canvas cards.
    hover_preview: HoverPreview,
}

/// State of an in-flight native file picker dialog. The blocking `rfd` call
/// runs in a separate thread so the main event loop stays responsive.
struct ExternalFilePending {
    container: ContainerId,
    rx: std::sync::mpsc::Receiver<Option<String>>,
}

/// State of the global search window. Like the rename/delete dialogs, the
/// window renders **only on the viewport that opened it** (root or a folder
/// window), so the search box appears where the user asked for it instead of
/// always floating on the main window.
struct SearchState {
    open: bool,
    filter: String,
    /// Focus the filter field only on the first frame after opening (IME-safe).
    focus_requested: bool,
    /// The viewport that opened the search; the window is drawn only there.
    origin: egui::ViewportId,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            open: false,
            filter: String::new(),
            focus_requested: false,
            origin: egui::ViewportId::ROOT,
        }
    }
}

/// Delay before showing a hover preview popup (milliseconds).
const PREVIEW_DELAY_MS: u64 = 800;

/// The kind of item being hover-previewed on a canvas.
#[derive(Clone, Debug, PartialEq)]
enum PreviewTarget {
    Snippet(EntityId),
    Folder(ContainerId),
    AiBox(ContainerId),
    Conversation(ConversationId),
    ExternalFile(ExternalFileId),
}

/// State of the hover-to-preview feature: when the user hovers over a card
/// for ~800ms, a floating popup shows the card's content.
#[derive(Clone, Debug, Default)]
struct HoverPreview {
    target: Option<PreviewTarget>,
    /// The container that owns the hovered card (used to check if the preview
    /// belongs to the current canvas).
    canvas_id: Option<ContainerId>,
    /// When the hover started (monotonic clock).
    hover_start: Option<Instant>,
    /// Whether the preview is currently visible (timer elapsed).
    visible: bool,
    /// The rect of the hovered card in viewport coordinates. The popup is
    /// anchored to this rect so it stays next to the card while the pointer
    /// moves inside it, instead of sliding around following the cursor.
    anchor_rect: Option<egui::Rect>,
    /// The rect of the popup (screen coordinates), used for hit-testing to
    /// keep the popup open while the pointer is over it.
    popup_rect: Option<egui::Rect>,
}

/// Display mode of a snippet viewport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ViewMode {
    /// Raw markdown editing (single pane).
    Source,
    /// Rendered CommonMark preview (read-only).
    #[default]
    Preview,
}

impl ViewMode {
    /// Whether the viewport is currently showing the raw-markdown editor.
    fn is_editing(self) -> bool {
        self == ViewMode::Source
    }
}

/// State of the "Insert Link…" picker for a snippet viewport.
#[derive(Clone, Debug)]
struct LinkPicker {
    /// Char index in the note body where the link is inserted.
    cursor: usize,
    /// Substring filter over snippet titles.
    filter: String,
    /// Focus the filter field only once when the picker opens (IME-safe).
    focus_requested: bool,
    /// Insert an inline embed (`![title]({id}.md)`) instead of a plain link.
    embed: bool,
}

#[derive(Debug)]
struct View {
    id: u64,
    entity_id: EntityId,
    mode: ViewMode,
    /// Read-only "Linked Source" preview opened from an AI box: the editor is
    /// unreachable and the mode menu offers no `Source` option.
    read_only: bool,
    /// Per-view CommonMark cache shared by every preview pane.
    markdown_cache: egui_commonmark::CommonMarkCache,
    /// Transient: request focus for the source editor next frame (set when
    /// entering `Source` from a preview action).
    focus_edit: bool,
    /// Transient: pointer position where the right-click view-mode menu should
    /// open (the preview pane, or the empty area below the editor), if any.
    mode_menu: Option<egui::Pos2>,
    /// Transient: open "Insert Link…" picker, if any.
    link_picker: Option<LinkPicker>,
    /// Transient: last broken-link click error (message + remaining frames),
    /// auto-dismissed after a short while.
    link_error: Option<(String, u32)>,
}

#[derive(Debug, Clone, PartialEq)]
enum RenameTarget {
    Snippet {
        id: EntityId,
        origin: egui::ViewportId,
    },
    Folder {
        id: ContainerId,
        origin: egui::ViewportId,
    },
    Conversation {
        ai_box: ContainerId,
        id: ConversationId,
        origin: egui::ViewportId,
    },
    /// Rename the display title of an external-file card (the file on disk is
    /// never renamed).
    ExternalFile {
        id: ExternalFileId,
        origin: egui::ViewportId,
    },
}

impl RenameTarget {
    fn origin(&self) -> egui::ViewportId {
        match self {
            RenameTarget::Snippet { origin, .. }
            | RenameTarget::Folder { origin, .. }
            | RenameTarget::Conversation { origin, .. }
            | RenameTarget::ExternalFile { origin, .. } => *origin,
        }
    }
}

/// State of the single in-flight rename dialog. The dialog is rendered inside
/// the viewport stored in [`RenameTarget::origin`] so that it appears in the
/// window that initiated the rename.
#[derive(Default)]
struct RenameDialogState {
    pending: Option<RenameTarget>,
    buffer: String,
    focus_requested: bool,
}

/// Semantics chosen when a card's reference is picked up via its context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClipboardSemantics {
    /// Create a new reference in the target container; the source stays.
    Link,
    /// Move the reference; it is removed from the source container.
    Move,
}

/// A reference picked from a card context menu (`Link` / `Move`), ready to be
/// pasted into any open canvas via its `Paste` context-menu entry.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ClipboardEntry {
    source_container: ContainerId,
    reference_id: ReferenceId,
    target: ReferenceTarget,
    semantics: ClipboardSemantics,
    /// The viewport where the reference was picked up; the clipboard status
    /// indicator is shown in this window.
    origin: egui::ViewportId,
}

/// A card drag published to egui's drag-and-drop payload. Any canvas can be a
/// drop target: dropping on an empty canvas creates/moves a reference in that
/// container, dropping on a folder card targets that folder's container.
/// Must be `Send + Sync` for [`egui::DragAndDrop::set_payload`].
#[derive(Clone, Debug, PartialEq, Eq)]
enum DragPayload {
    Reference {
        source_container: ContainerId,
        reference_id: ReferenceId,
        target: ReferenceTarget,
    },
}

/// A pending "delete last reference" confirmation. Deleting the final link to
/// a snippet or folder permanently removes the underlying entity/container, so
/// a confirmation dialog is shown first.
#[derive(Clone, Debug, PartialEq)]
struct PendingDelete {
    owner: ContainerId,
    reference: ReferenceId,
    target: ReferenceTarget,
    /// The viewport that initiated the delete; the confirmation dialog is
    /// shown in this window.
    origin: egui::ViewportId,
}

#[derive(Clone, Debug, Default)]
enum ViewAction {
    #[default]
    None,
    Close,
    /// Open another snippet, requested by clicking a local markdown link in a
    /// preview pane.
    OpenSnippet(EntityId),
}

#[derive(Debug)]
struct CanvasItem {
    reference_id: ReferenceId,
    target: ReferenceTarget,
    /// The AI member role of this reference. Inside an AI box this drives the
    /// card visuals and the context menu (read-only Source vs. Conversation vs.
    /// saved Output); ordinary containers keep `Normal`.
    role: MemberRole,
    position: [f32; 2],
    size: egui::Vec2,
}

#[derive(Clone, Debug)]
struct DragState {
    index: usize,
    start_position: [f32; 2],
    invalid: bool,
    /// Identity of the dragged reference; used to match the egui
    /// drag-and-drop payload and to resolve the card safely at frame end.
    reference_id: ReferenceId,
    /// Pointer offset from the card's top-left at grab time. The card follows
    /// the cursor by this offset, so dragging feels glued to the pointer
    /// (smooth in both free and grid-snap modes).
    grab_offset: egui::Vec2,
}

#[derive(Debug)]
struct ContainerCanvas {
    container_id: ContainerId,
    items: Vec<CanvasItem>,
    /// Canvas-local text annotations. Texts are not references: they cannot
    /// be linked, pasted, or moved across container boundaries.
    texts: Vec<CanvasText>,
    layout: ContainerLayout,
    dragging: Option<DragState>,
    /// Transient: index of the text being dragged, its start position, and
    /// whether the current position overlaps another element.
    dragging_text: Option<(usize, [f32; 2], bool)>,
    /// Transient: the text currently being edited in place, if any.
    editing_text: Option<TextId>,
    /// Transient: focus is requested only on the first frame of editing, so
    /// that IME input is not broken by repeated focus requests.
    edit_focus_requested: bool,
    /// Transient: canvas-local position of the right-click that opened the
    /// empty-canvas context menu; new items are created here. Stored so the
    /// position survives while the menu stays open across frames.
    menu_anchor: Option<[f32; 2]>,
}

#[derive(Debug)]
enum CanvasCommand {
    OpenSnippet(EntityId),
    OpenFolder(ContainerId),
    /// Open the system page for a built-in special item (e.g. Settings).
    OpenSpecial(SpecialKind),
    /// Open the global search window.
    OpenSearch,
    /// Open the "Insert External File" dialog for the given container.
    OpenFileInsertDialog(ContainerId),
    /// Remove a reference from `owner`. If it is the last link to its target,
    /// a confirmation dialog is shown before the entity/container is deleted.
    DeleteReference {
        owner: ContainerId,
        reference: ReferenceId,
        target: ReferenceTarget,
    },
    CloseFolder(ContainerId),
    RenameSnippet(EntityId),
    RenameFolder(ContainerId),
    /// Internal: apply the rename confirmed in a viewport's dialog.
    ApplyRename(RenameTarget),
    /// Internal: apply the delete confirmed in a viewport's dialog.
    ConfirmDelete(PendingDelete),
    /// Paste the clipboard reference into the given container.
    PasteClipboard {
        container: ContainerId,
        entry: ClipboardEntry,
    },
    /// Drop a dragged card onto this canvas: create (default) or move (Shift)
    /// the reference into `container`.
    DropOnCanvas {
        container: ContainerId,
        /// Drop position in the target canvas's local coordinates; `None`
        /// picks a free default slot (used when dropping onto a folder card).
        position: Option<[f32; 2]>,
        payload: DragPayload,
        move_semantics: bool,
    },
    /// Open the conversation window for a `Conversation` card inside an AI box.
    /// Wired to the AI conversation layer.
    OpenConversation {
        ai_box: ContainerId,
        conversation: ConversationId,
    },
    /// Create a new AI box container and place its card in `owner`.
    NewAiBox {
        owner: ContainerId,
        position: Option<[f32; 2]>,
    },
    /// Create a new (initially empty) conversation inside an AI box.
    NewConversation {
        ai_box: ContainerId,
        position: Option<[f32; 2]>,
    },
    /// Link an existing snippet or folder as a read-only `Source` of an AI box.
    LinkAiSource {
        ai_box: ContainerId,
        target: ReferenceTarget,
        position: Option<[f32; 2]>,
    },
    /// Remove a `Source` card from an AI box (unlink only; never deletes the
    /// source entity).
    RemoveAiSource {
        ai_box: ContainerId,
        reference: ReferenceId,
    },
    /// Delete a conversation (sidecar state + card) from an AI box.
    DeleteConversation {
        ai_box: ContainerId,
        conversation: ConversationId,
    },
    /// Open a snippet in the read-only "Linked Source" preview.
    OpenSnippetReadOnly(EntityId),
    /// Open the rename dialog for a conversation card.
    RenameConversation {
        ai_box: ContainerId,
        conversation: ConversationId,
        origin: egui::ViewportId,
    },
    /// Insert an external-file card into `container` at `position` (or a free
    /// default slot when `None`).
    InsertExternalFile {
        container: ContainerId,
        file: ExternalFileRef,
        position: Option<[f32; 2]>,
    },
    /// Open an external file with the system's default application.
    OpenExternalFile(ExternalFileRef),
    /// Open the rename dialog for an external-file card's display title.
    RenameExternalFile(ExternalFileId),
}

impl ContainerCanvas {
    fn save_layout(&mut self, store: &WorkspaceStore) {
        for item in &self.items {
            self.layout.items.insert(
                item.reference_id.clone(),
                CardLayout {
                    position: item.position,
                    color: None,
                },
            );
        }
        self.layout.texts = self.texts.clone();
        let _ = store.save_layout(&self.layout);
    }

    /// Repositions every card into an organized column-major grid: cards fill
    /// the first column top-to-bottom, then wrap to the next column. The number
    /// of rows per column is chosen so a column fits within `viewport_height`
    /// (the visible canvas height); when there are more cards than that, the
    /// layout overflows horizontally into the scroll area.
    ///
    /// The layout **follows the `snap_to_grid` setting**: when snapped, cards
    /// are packed densely **without gaps** (column pitch = card width, row
    /// pitch = the tallest card); otherwise they are aligned to the 32 pt
    /// canvas grid with breathing room.
    fn organize(&mut self, store: &WorkspaceStore, viewport_height: f32, snap_to_grid: bool) {
        const GRID: f32 = 32.0;
        // On-grid origin: every card corner lands on a grid point.
        const MARGIN: f32 = GRID;
        const GAP_Y: f32 = 12.0;
        const MIN_CARD_HEIGHT: f32 = 25.0;

        let max_height = self
            .items
            .iter()
            .map(|item| item.size.y)
            .fold(MIN_CARD_HEIGHT, f32::max)
            .max(MIN_CARD_HEIGHT);
        let (step_x, step_y) = if snap_to_grid {
            // Dense: cards touch; no gaps at all.
            (CARD_WIDTH, max_height)
        } else {
            // Grid-aligned: fixed on-grid column pitch; round the row pitch up
            // to whole grid cells so every row sits on-grid, with gaps.
            let step_x = 6.0 * GRID;
            let step_y = ((max_height + GAP_Y) / GRID).ceil() * GRID;
            (step_x, step_y)
        };
        let rows_per_column = if viewport_height <= MARGIN {
            1
        } else {
            (((viewport_height - MARGIN) / step_y).floor() as usize).max(1)
        };
        for (index, item) in self.items.iter_mut().enumerate() {
            let column = index / rows_per_column;
            let row = index % rows_per_column;
            item.position = [
                MARGIN + column as f32 * step_x,
                MARGIN + row as f32 * step_y,
            ];
        }
        self.save_layout(store);
    }

    fn remove_reference(&mut self, reference_id: &ReferenceId, store: &WorkspaceStore) {
        self.items.retain(|item| &item.reference_id != reference_id);
        self.layout.items.remove(reference_id);
        let _ = store.save_layout(&self.layout);
    }

    fn remove_entity(&mut self, entity_id: &EntityId, store: &WorkspaceStore) {
        let removed: Vec<_> = self
            .items
            .iter()
            .filter_map(|item| match &item.target {
                ReferenceTarget::Snippet(id) if id == entity_id => Some(item.reference_id.clone()),
                _ => None,
            })
            .collect();
        self.items.retain(
            |item| !matches!(&item.target, ReferenceTarget::Snippet(id) if id == entity_id),
        );
        for reference_id in removed {
            self.layout.items.remove(&reference_id);
        }
        let _ = store.save_layout(&self.layout);
    }
}

impl HomePage {
    pub(crate) fn new(workspace: impl Into<PathBuf>) -> Self {
        let workspace_path = workspace.into();
        let store = SnippetStore::open(&workspace_path).expect("failed to open snippet store");
        let mut snippets = store.load_all().unwrap_or_default();
        if snippets.is_empty() {
            for (title, content) in [
                ("hello", "hello, world!"),
                ("floatdea", "Welcome to floatdea!"),
                (
                    "help",
                    "Right-click for relevant operations.\n\nDouble-click on a card to open it. Drag cards to move them.",
                ),
            ] {
                let snippet = Snippet {
                    id: EntityId::new(),
                    title: title.to_owned(),
                    content: content.to_owned(),
                };
                let _ = store.save(&snippet);
                snippets.push(snippet);
            }
        }

        let workspace_store =
            WorkspaceStore::open(&workspace_path).expect("failed to open workspace metadata");
        let workspace = workspace_store
            .load_or_initialize(&snippets)
            .expect("failed to load workspace metadata");
        let settings_store =
            SettingsStore::open(&workspace_path).expect("failed to open settings store");
        let settings = settings_store.load();
        let ai_store = AiStore::open(&workspace_path).expect("failed to open AI sidecar store");
        // Load sidecar state for every existing AI box so conversation titles
        // and message history are available without touching the store each
        // frame.
        let ai_boxes: BTreeMap<ContainerId, AiBoxData> = workspace
            .containers
            .values()
            .filter(|container| container.kind == ContainerKind::AiWorkspace)
            .map(|container| (container.id.clone(), ai_store.load_box(&container.id)))
            .collect();
        let all_snippets = snippets
            .into_iter()
            .map(|snippet| (snippet.id.clone(), snippet))
            .collect();
        let root = Self::load_container_canvas(
            &workspace,
            &workspace_store,
            &all_snippets,
            workspace.root.clone(),
        );

        let (ai_worker, ai_events) = AiWorker::spawn();
        Self {
            all_snippets,
            store,
            workspace,
            workspace_store,
            root,
            folder_views: BTreeMap::new(),
            views: Vec::new(),
            next_view_id: 0,
            pending_delete: None,
            rename_dialog: RenameDialogState::default(),
            clipboard: None,
            math_renderer: MathRenderer::default(),
            consumed_drops: BTreeSet::new(),
            settings,
            settings_store,
            settings_open: false,
            root_exit_pending: false,
            search: SearchState::default(),
            ai_store,
            ai_boxes,
            ai_worker,
            ai_events,
            ai_open: None,
            ai_input: String::new(),
            ai_active_turn: None,
            ai_streaming: String::new(),
            ai_snapshots: Vec::new(),
            ai_markdown_cache: egui_commonmark::CommonMarkCache::default(),
            ai_provider_override: None,
            external_open_error: None,
            os_file_drop_consumed: false,
            pending_file_picker: None,
            file_content_cache: Arc::new(Mutex::new(HashMap::new())),
            hover_preview: HoverPreview::default(),
        }
    }

    fn load_container_canvas(
        workspace: &Workspace,
        store: &WorkspaceStore,
        snippets: &BTreeMap<EntityId, Snippet>,
        container_id: ContainerId,
    ) -> ContainerCanvas {
        let layout = store
            .load_layout(&container_id)
            .unwrap_or_else(|_| ContainerLayout::empty(container_id.clone()));
        let items = workspace
            .containers
            .get(&container_id)
            .into_iter()
            .flat_map(|container| &container.members)
            .filter(|reference| match &reference.target {
                ReferenceTarget::Snippet(id) => snippets.contains_key(id),
                ReferenceTarget::Container(id) => workspace.containers.contains_key(id),
                // Special items, AI conversation cards, and external file cards
                // always resolve (the conversation sidecar may be absent, in
                // which case the card simply shows a placeholder title; the
                // file itself may be missing, the card still opens it on click).
                ReferenceTarget::Special(_)
                | ReferenceTarget::Conversation(_)
                | ReferenceTarget::ExternalFile(_) => true,
            })
            .enumerate()
            .map(|(index, reference)| CanvasItem {
                reference_id: reference.id.clone(),
                target: reference.target.clone(),
                role: reference.role,
                position: layout
                    .items
                    .get(&reference.id)
                    .map_or_else(|| default_card_position(index), |item| item.position),
                size: egui::vec2(CARD_WIDTH, 25.0),
            })
            .collect();

        ContainerCanvas {
            container_id,
            items,
            texts: layout.texts.clone(),
            layout,
            dragging: None,
            dragging_text: None,
            editing_text: None,
            edit_focus_requested: false,
            menu_anchor: None,
        }
    }

    fn open_view(&mut self, entity_id: EntityId) {
        // Opening a snippet is a navigation action; drop any pending clipboard
        // reference so a stale paste cannot leak into another window.
        self.clipboard = None;
        let id = self.next_view_id;
        self.next_view_id += 1;
        // Empty snippets open straight into the editor so they can be typed
        // into immediately; notes with content open as a rendered preview.
        let mode = if self
            .all_snippets
            .get(&entity_id)
            .is_some_and(|snippet| snippet.content.is_empty())
        {
            ViewMode::Source
        } else {
            ViewMode::Preview
        };
        self.views.push(View {
            id,
            entity_id,
            mode,
            read_only: false,
            markdown_cache: egui_commonmark::CommonMarkCache::default(),
            focus_edit: false,
            mode_menu: None,
            link_picker: None,
            link_error: None,
        });
    }

    /// Opens a snippet as a read-only "Linked Source · Read-only" preview
    /// (opened from an AI box source card). The editor is unreachable; use
    /// `open_view` for the normal, writable window.
    fn open_read_only_view(&mut self, entity_id: EntityId) {
        if !self.all_snippets.contains_key(&entity_id) {
            return;
        }
        self.clipboard = None;
        let id = self.next_view_id;
        self.next_view_id += 1;
        self.views.push(View {
            id,
            entity_id,
            mode: ViewMode::Preview,
            read_only: true,
            markdown_cache: egui_commonmark::CommonMarkCache::default(),
            focus_edit: false,
            mode_menu: None,
            link_picker: None,
            link_error: None,
        });
    }

    /// Whether snippet/folder windows are presented as floating windows inside
    /// the single main window (full-window mode) instead of native OS windows.
    fn floating_windows(&self) -> bool {
        self.settings.window_mode == WindowMode::Floating
    }

    /// Full-window mode: the "Exit?" confirmation shown when the user presses
    /// the root window's close button. Confirming exits the app; cancelling
    /// keeps the root window (and everything else) open.
    fn render_root_exit_dialog(&mut self, ctx: &egui::Context) {
        if !self.root_exit_pending {
            return;
        }
        let mut exit = false;
        let mut cancel = false;
        egui::Window::new("Exit?")
            .id(egui::Id::new("root-exit-dialog"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Exit FloatDea?");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    let exit_button =
                        egui::Button::new(egui::RichText::new("Exit").color(egui::Color32::WHITE))
                            .fill(egui::Color32::from_rgb(179, 38, 30));
                    if ui.add(exit_button).clicked() {
                        exit = true;
                    }
                });
            });
        if cancel {
            self.root_exit_pending = false;
        }
        if exit {
            self.root_exit_pending = false;
            // Closing the root viewport terminates the app.
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// In floating-window mode every dialog carries the root viewport origin,
    /// so closing a folder window must clear dialogs by their target id.
    fn clear_dialog_for_folder(&mut self, container_id: &ContainerId) {
        if self.rename_dialog.pending.as_ref().is_some_and(
            |target| matches!(target, RenameTarget::Folder { id, .. } if id == container_id),
        ) {
            self.rename_dialog.pending = None;
        }
        if self.pending_delete.as_ref().is_some_and(|pending| {
            matches!(&pending.target, ReferenceTarget::Container(id) if id == container_id)
        }) {
            self.pending_delete = None;
        }
    }

    fn process_canvas_commands(&mut self, commands: Vec<CanvasCommand>, origin: egui::ViewportId) {
        for command in commands {
            match command {
                CanvasCommand::OpenSnippet(id) => self.open_view(id),
                CanvasCommand::OpenFolder(id) => self.open_folder(&id),
                CanvasCommand::OpenSpecial(kind) => match kind {
                    SpecialKind::Settings => self.settings_open = true,
                },
                CanvasCommand::OpenSearch => {
                    self.search.open = true;
                    // The search window renders only on the viewport that opened
                    // it (root or a folder window), mirroring the rename dialog.
                    self.search.origin = origin;
                    self.search.focus_requested = true;
                }
                CanvasCommand::OpenFileInsertDialog(container) => {
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let selected = rfd::FileDialog::new().pick_file();
                        let _ = tx.send(selected.map(|p| p.display().to_string()));
                    });
                    self.pending_file_picker = Some(ExternalFilePending { container, rx });
                }
                CanvasCommand::DeleteReference {
                    owner,
                    reference,
                    target,
                } => {
                    if matches!(target, ReferenceTarget::ExternalFile(_)) {
                        // An external-file card is a plain reference: removing it
                        // never touches the file on disk, so no confirmation is
                        // needed even for the last link.
                        self.remove_reference_only(&owner, &reference);
                    } else if reference_count(&self.workspace, &target) == 1 {
                        self.pending_delete = Some(PendingDelete {
                            owner,
                            reference,
                            target,
                            origin,
                        });
                    } else {
                        self.remove_reference_only(&owner, &reference);
                    }
                }
                CanvasCommand::CloseFolder(id) => {
                    let viewport = egui::ViewportId::from_hash_of(("folder-view", id.as_str()));
                    self.clear_rename_for_viewport(viewport);
                    self.clear_search_for_viewport(viewport);
                    
                    if self.floating_windows() {
                        self.clear_dialog_for_folder(&id);
                    }
                    if self
                        .pending_delete
                        .as_ref()
                        .is_some_and(|pending| pending.origin == viewport)
                    {
                        self.pending_delete = None;
                    }
                    self.folder_views.remove(&id);
                }
                CanvasCommand::RenameSnippet(id) => {
                    if let Some(snippet) = self.all_snippets.get(&id) {
                        self.rename_dialog.buffer = snippet.title.clone();
                        self.rename_dialog.pending = Some(RenameTarget::Snippet { id, origin });
                        self.rename_dialog.focus_requested = false;
                    }
                }
                CanvasCommand::RenameFolder(id) => {
                    if let Some(container) = self.workspace.containers.get(&id) {
                        self.rename_dialog.buffer = container.title.clone();
                        self.rename_dialog.pending = Some(RenameTarget::Folder { id, origin });
                        self.rename_dialog.focus_requested = false;
                    }
                }
                CanvasCommand::ApplyRename(target) => {
                    if self.rename_dialog.pending.as_ref() != Some(&target) {
                        continue;
                    }
                    let new_title = self.rename_dialog.buffer.trim().to_owned();
                    let ok = match &target {
                        RenameTarget::Snippet { id, .. } => self.rename_snippet(id, new_title),
                        RenameTarget::Folder { id, .. } => self.rename_folder(id, new_title),
                        RenameTarget::Conversation { ai_box, id, .. } => {
                            self.rename_conversation(ai_box, id, new_title)
                        }
                        RenameTarget::ExternalFile { id, .. } => {
                            self.rename_external_file(id, new_title)
                        }
                    };
                    if ok {
                        self.rename_dialog.pending = None;
                    }
                }
                CanvasCommand::ConfirmDelete(pending) => {
                    if self.pending_delete.as_ref() == Some(&pending) {
                        self.confirm_delete(pending);
                    }
                }
                CanvasCommand::PasteClipboard { container, entry } => {
                    if self.paste_clipboard(&container, &entry) {
                        // The clipboard is single-use: clear it after a
                        // successful paste (Link or Move).
                        self.clipboard = None;
                    }
                }
                CanvasCommand::DropOnCanvas {
                    container,
                    position,
                    payload,
                    move_semantics,
                } => {
                    if self.apply_drop(&container, position, &payload, move_semantics)
                        && let DragPayload::Reference {
                            source_container,
                            reference_id,
                            ..
                        } = &payload
                    {
                        // Record the consumption so `finalize_drops` knows the
                        // drag ended in a successful drop (no bounce-back).
                        self.consumed_drops
                            .insert((source_container.clone(), reference_id.clone()));
                    }
                }
                CanvasCommand::OpenConversation {
                    ai_box,
                    conversation,
                } => self.ai_open = Some((ai_box, conversation)),
                CanvasCommand::NewAiBox { owner, position } => self.create_ai_box(&owner, position),
                CanvasCommand::NewConversation { ai_box, position } => {
                    self.create_conversation(&ai_box, position)
                }
                CanvasCommand::LinkAiSource {
                    ai_box,
                    target,
                    position,
                } => self.link_ai_source(&ai_box, target, position),
                CanvasCommand::RemoveAiSource { ai_box, reference } => {
                    self.remove_ai_source(&ai_box, &reference)
                }
                CanvasCommand::DeleteConversation {
                    ai_box,
                    conversation,
                } => self.delete_conversation(&ai_box, &conversation),
                CanvasCommand::OpenSnippetReadOnly(id) => self.open_read_only_view(id),
                CanvasCommand::RenameConversation {
                    ai_box,
                    conversation,
                    origin,
                } => {
                    let title = self
                        .ai_boxes
                        .get(&ai_box)
                        .and_then(|data| data.get(&conversation))
                        .map(|conversation| conversation.title.clone());
                    if let Some(title) = title {
                        self.rename_dialog.buffer = title;
                        self.rename_dialog.pending = Some(RenameTarget::Conversation {
                            ai_box,
                            id: conversation,
                            origin,
                        });
                        self.rename_dialog.focus_requested = false;
                    }
                }
                CanvasCommand::InsertExternalFile {
                    container,
                    file,
                    position,
                } => self.insert_external_file(&container, file, position),
                CanvasCommand::OpenExternalFile(file) => {
                    if let Err(error) = open_path_externally(&file.path) {
                        self.external_open_error = Some((
                            format!("Could not open \"{}\": {error}", file.title),
                            origin,
                            EXTERNAL_OPEN_ERROR_FRAMES,
                        ));
                    }
                }
                CanvasCommand::RenameExternalFile(id) => {
                    let title = self
                        .workspace
                        .containers
                        .values()
                        .flat_map(|container| &container.members)
                        .find_map(|reference| match &reference.target {
                            ReferenceTarget::ExternalFile(file) if file.id == id => {
                                Some(file.title.clone())
                            }
                            _ => None,
                        });
                    if let Some(title) = title {
                        self.rename_dialog.buffer = title;
                        self.rename_dialog.pending = Some(RenameTarget::ExternalFile { id, origin });
                        self.rename_dialog.focus_requested = false;
                    }
                }
            }
        }
    }

    fn clear_rename_for_viewport(&mut self, viewport_id: egui::ViewportId) {
        if self
            .rename_dialog
            .pending
            .as_ref()
            .is_some_and(|target| target.origin() == viewport_id)
        {
            self.rename_dialog.pending = None;
        }
    }

    /// Closes the global search window when the viewport that opened it is gone
    /// (e.g. the folder window that initiated the search was closed), so the
    /// window never gets stuck in an invisible "open" state.
    fn clear_search_for_viewport(&mut self, viewport_id: egui::ViewportId) {
        if self.search.open && self.search.origin == viewport_id {
            self.search.open = false;
        }
    }

    

    /// Removes a single reference from `owner` without touching the underlying
    /// entity/container (used when the deleted link is not the last one).
    fn remove_reference_only(&mut self, owner: &ContainerId, reference: &ReferenceId) {
        let _ = self.workspace.remove_reference(owner, reference);
        let _ = self.workspace_store.save(&self.workspace);
        if owner == &self.root.container_id {
            self.root.remove_reference(reference, &self.workspace_store);
        } else if let Some(canvas) = self.folder_views.get_mut(owner) {
            canvas.remove_reference(reference, &self.workspace_store);
        }
    }

    /// Confirms a "delete last reference" action: removes the link and deletes
    /// the underlying snippet file or folder container permanently.
    fn confirm_delete(&mut self, pending: PendingDelete) {
        match &pending.target {
            ReferenceTarget::Snippet(entity_id) => {
                let entity_id = entity_id.clone();
                if let Some(snippet) = self.all_snippets.get(&entity_id) {
                    let _ = self.store.remove(snippet);
                }
                self.workspace.remove_entity_references(&entity_id);
                let _ = self.workspace_store.save(&self.workspace);
                self.root.remove_entity(&entity_id, &self.workspace_store);
                for canvas in self.folder_views.values_mut() {
                    canvas.remove_entity(&entity_id, &self.workspace_store);
                }
                self.all_snippets.remove(&entity_id);
                self.views.retain(|view| view.entity_id != entity_id);
            }
            ReferenceTarget::Container(container_id) => {
                let container_id = container_id.clone();
                let was_ai_box = self.workspace.is_ai_box(&container_id);
                let _ = self
                    .workspace
                    .remove_reference(&pending.owner, &pending.reference);
                self.workspace.containers.remove(&container_id);
                let _ = self.workspace_store.save(&self.workspace);
                // Deleting an AI box only cleans its own sidecar state; source
                // entities and saved outputs are never touched.
                if was_ai_box {
                    self.ai_boxes.remove(&container_id);
                    let _ = self.ai_store.remove_box(&container_id);
                }
                if pending.owner == self.root.container_id {
                    self.root
                        .remove_reference(&pending.reference, &self.workspace_store);
                } else if let Some(canvas) = self.folder_views.get_mut(&pending.owner) {
                    canvas.remove_reference(&pending.reference, &self.workspace_store);
                }
                self.clear_rename_for_viewport(egui::ViewportId::from_hash_of((
                    "folder-view",
                    container_id.as_str(),
                )));
                self.clear_search_for_viewport(egui::ViewportId::from_hash_of((
                    "folder-view",
                    container_id.as_str(),
                )));
                self.folder_views.remove(&container_id);
            }
            // Special items cannot be deleted; this arm is unreachable.
            ReferenceTarget::Special(_) => {}
            // Conversation cards are never deleted through the entity-delete
            // path; the AI conversation layer handles their removal.
            ReferenceTarget::Conversation(_) => {}
            // External-file cards are deleted without confirmation (see the
            // `DeleteReference` command), so the dialog never opens for them.
            ReferenceTarget::ExternalFile(_) => {}
        }
        self.pending_delete = None;
    }

    fn canvas_for(&self, container: &ContainerId) -> Option<&ContainerCanvas> {
        if container == &self.root.container_id {
            Some(&self.root)
        } else {
            self.folder_views.get(container)
        }
    }

    // ---- External file references ----

    /// Inserts an external-file card into `container` at `position` (or a free
    /// default slot when `None`), persisting the reference and the card layout.
    /// The file on disk is untouched: FloatDea only records its path.
    fn insert_external_file(
        &mut self,
        container: &ContainerId,
        file: ExternalFileRef,
        position: Option<[f32; 2]>,
    ) {
        let is_ai_box = self.workspace.is_ai_box(container);
        let result = if is_ai_box {
            self.workspace.add_source_reference(container, ReferenceTarget::ExternalFile(file.clone()))
        } else {
            self.workspace.add_external_file_reference(container, file.clone())
        };
        let Ok(reference_id) = result else {
            return;
        };
        let _ = self.workspace_store.save(&self.workspace);
        let position = position.unwrap_or_else(|| {
            self.canvas_for(container)
                .map(|canvas| {
                    canvas::default_position_for(
                        container,
                        &canvas.items,
                        &self.all_snippets,
                        &self.workspace,
                        &self.ai_boxes,
                        &canvas::approx_text_rects(&canvas.texts),
                    )
                })
                .unwrap_or_else(|| default_card_position(0))
        });
        // Direct field borrow so the layout save below can also borrow
        // `self.workspace_store` (a method like `canvas_for_mut` would borrow
        // all of `self`).
        let target_canvas = if container == &self.root.container_id {
            Some(&mut self.root)
        } else {
            self.folder_views.get_mut(container)
        };
        if let Some(canvas) = target_canvas {
            let role = if is_ai_box { MemberRole::Source } else { MemberRole::Normal };
            canvas.items.push(CanvasItem {
                reference_id: reference_id.clone(),
                target: ReferenceTarget::ExternalFile(file),
                role,
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
    }

    /// Renames the **display title** of an external-file card. The file on disk
    /// is never renamed: only the recorded `ExternalFileRef.title` changes, in
    /// every workspace reference and every open canvas that shares the id.
    fn rename_external_file(&mut self, id: &ExternalFileId, new_title: String) -> bool {
        let new_title = new_title.trim().to_owned();
        if new_title.is_empty() {
            return false;
        }
        let mut found = false;
        for container in self.workspace.containers.values_mut() {
            for reference in &mut container.members {
                if let ReferenceTarget::ExternalFile(file) = &mut reference.target
                    && &file.id == id
                {
                    file.title = new_title.clone();
                    found = true;
                }
            }
        }
        // Open canvases cache a clone of the reference target; keep them in
        // sync so the card label updates without a reload.
        for item in &mut self.root.items {
            if let ReferenceTarget::ExternalFile(file) = &mut item.target
                && &file.id == id
            {
                file.title = new_title.clone();
                found = true;
            }
        }
        for canvas in self.folder_views.values_mut() {
            for item in &mut canvas.items {
                if let ReferenceTarget::ExternalFile(file) = &mut item.target
                    && &file.id == id
                {
                    file.title = new_title.clone();
                    found = true;
                }
            }
        }
        if !found {
            return false;
        }
        let _ = self.workspace_store.save(&self.workspace);
        true
    }

    /// Finds an `ExternalFileRef` by its `ExternalFileId` across all containers
    /// in the workspace. Returns `None` if no matching reference exists.
    fn find_external_file(&self, id: &ExternalFileId) -> Option<&ExternalFileRef> {
        self.workspace.containers.values().flat_map(|container| &container.members).find_map(|reference| {
            if let ReferenceTarget::ExternalFile(file) = &reference.target
                && &file.id == id
            {
                Some(file)
            } else {
                None
            }
        })
    }

    /// Like [`Self::find_external_file`] but resolves the `ExternalFileId` from
    /// a `SourceTarget::ExternalFile`. Returns `None` for non-ExternalFile
    /// targets or when the id is not found.
    pub(super) fn find_external_file_ref(&self, source: &SourceTarget) -> Option<&ExternalFileRef> {
        match source {
            SourceTarget::ExternalFile(id) => self.find_external_file(id),
            _ => None,
        }
    }

    // ---- AI workbench operations (阶段 1: no-model workbench) ----

    /// Creates a new AI box container and places its card in `owner`'s canvas.
    fn create_ai_box(&mut self, owner: &ContainerId, position: Option<[f32; 2]>) {
        let container_id = self.workspace.create_ai_box("AI Box");
        let Ok(reference_id) = self
            .workspace
            .add_container_reference(owner, container_id.clone())
        else {
            return;
        };
        let _ = self.workspace_store.save(&self.workspace);
        let position = position.unwrap_or_else(|| {
            self.canvas_for(owner)
                .map(|canvas| {
                    canvas::default_position_for(
                        owner,
                        &canvas.items,
                        &self.all_snippets,
                        &self.workspace,
                        &self.ai_boxes,
                        &canvas::approx_text_rects(&canvas.texts),
                    )
                })
                .unwrap_or_else(|| default_card_position(0))
        });
        // Direct field borrow so the layout save below can also borrow
        // `self.workspace_store` (a method like `canvas_for_mut` would borrow
        // all of `self`).
        let target_canvas = if owner == &self.root.container_id {
            Some(&mut self.root)
        } else {
            self.folder_views.get_mut(owner)
        };
        if let Some(canvas) = target_canvas {
            canvas.items.push(CanvasItem {
                reference_id: reference_id.clone(),
                target: ReferenceTarget::Container(container_id),
                role: MemberRole::Normal,
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
    }

    /// The initial source bindings of a new conversation: every direct
    /// `Source`-role snippet/container reference of the AI box, de-duplicated
    /// by stable id. Per plan_ai.md §3.2, unbound cards never join the scope.
    fn initial_conversation_sources(&self, ai_box: &ContainerId) -> Vec<SourceTarget> {
        self.workspace
            .containers
            .get(ai_box)
            .map(|container| {
                container
                    .members
                    .iter()
                    .filter(|reference| reference.role == MemberRole::Source)
                    .filter_map(|reference| match &reference.target {
                        ReferenceTarget::Snippet(id) => {
                            Some(SourceTarget::Snippet(id.clone()))
                        }
                        ReferenceTarget::Container(id) => {
                            Some(SourceTarget::Container(id.clone()))
                        }
                        ReferenceTarget::ExternalFile(file) => {
                            Some(SourceTarget::ExternalFile(file.id.clone()))
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Spawns background threads to extract text content from all ExternalFile
    /// sources in `sources`, storing results in `file_content_cache`. This way
    /// PDF extraction never blocks the UI thread when the user sends a message.
    fn spawn_content_extraction(&self, sources: &[SourceTarget]) {
        for target in sources {
            let SourceTarget::ExternalFile(id) = target else {
                continue;
            };
            let Some(file) = self.find_external_file(id) else {
                continue;
            };
            let path = file.path.clone();
            let fid = file.id.clone();
            let cache = Arc::clone(&self.file_content_cache);
            std::thread::spawn(move || {
                let text = floatdea::data::ai::extract::extract_text_from_file(&path);
                if let Some(text) = text
                    && let Ok(mut cache) = cache.lock()
                {
                    cache.insert(fid, text);
                }
            });
        }
    }

    /// Creates a conversation (sidecar state + card) inside an AI box. Initial
    /// sources are the AI box's direct `Source` references.
    fn create_conversation(&mut self, ai_box: &ContainerId, position: Option<[f32; 2]>) {
        let sources = self.initial_conversation_sources(ai_box);
        self.spawn_content_extraction(&sources);
        let conversation = ConversationId::new();
        let data = self
            .ai_boxes
            .entry(ai_box.clone())
            .or_insert_with(|| self.ai_store.load_box(ai_box));
        if !data.create_conversation(
            conversation.clone(),
            "New Conversation",
            false,
            sources,
            now_unix(),
        ) {
            return;
        }
        let _ = self.ai_store.save_box(data);
        let Ok(reference_id) = self
            .workspace
            .add_conversation_card(ai_box, conversation.clone())
        else {
            return;
        };
        let _ = self.workspace_store.save(&self.workspace);
        let position = position.unwrap_or_else(|| {
            self.canvas_for(ai_box)
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
                .unwrap_or_else(|| default_card_position(0))
        });
        let target_canvas = if ai_box == &self.root.container_id {
            Some(&mut self.root)
        } else {
            self.folder_views.get_mut(ai_box)
        };
        if let Some(canvas) = target_canvas {
            canvas.items.push(CanvasItem {
                reference_id: reference_id.clone(),
                target: ReferenceTarget::Conversation(conversation),
                role: MemberRole::Conversation,
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
    }

    /// Links an existing snippet or folder as a read-only `Source` of an AI box.
    fn link_ai_source(
        &mut self,
        ai_box: &ContainerId,
        target: ReferenceTarget,
        position: Option<[f32; 2]>,
    ) {
        let Ok(reference_id) = self.workspace.add_source_reference(ai_box, target.clone()) else {
            return;
        };
        let _ = self.workspace_store.save(&self.workspace);
        let position = position.unwrap_or_else(|| {
            self.canvas_for(ai_box)
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
                .unwrap_or_else(|| default_card_position(0))
        });
        let target_canvas = if ai_box == &self.root.container_id {
            Some(&mut self.root)
        } else {
            self.folder_views.get_mut(ai_box)
        };
        if let Some(canvas) = target_canvas {
            canvas.items.push(CanvasItem {
                reference_id: reference_id.clone(),
                target,
                role: MemberRole::Source,
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
    }

    /// Removes a `Source` card from an AI box. Unlink only: the underlying
    /// entity is never deleted, even when this was its last visible reference.
    fn remove_ai_source(&mut self, ai_box: &ContainerId, reference: &ReferenceId) {
        let _ = self.workspace.remove_reference(ai_box, reference);
        let _ = self.workspace_store.save(&self.workspace);
        let target_canvas = if ai_box == &self.root.container_id {
            Some(&mut self.root)
        } else {
            self.folder_views.get_mut(ai_box)
        };
        if let Some(canvas) = target_canvas {
            canvas.remove_reference(reference, &self.workspace_store);
        }
    }

    /// Deletes a conversation: its sidecar state (title, messages, source
    /// bindings) and its canvas card. Sources, saved outputs and source
    /// entities are untouched.
    fn delete_conversation(&mut self, ai_box: &ContainerId, conversation: &ConversationId) {
        let reference = self
            .workspace
            .containers
            .get(ai_box)
            .and_then(|container| {
                container
                    .members
                    .iter()
                    .find(|reference| {
                        matches!(
                            &reference.target,
                            ReferenceTarget::Conversation(id) if id == conversation
                        )
                    })
                    .map(|reference| reference.id.clone())
            });
        if let Some(data) = self.ai_boxes.get_mut(ai_box) {
            data.delete_conversation(conversation);
            let _ = self.ai_store.save_box(data);
        }
        if let Some(reference) = reference {
            let _ = self.workspace.remove_reference(ai_box, &reference);
            let _ = self.workspace_store.save(&self.workspace);
            let target_canvas = if ai_box == &self.root.container_id {
                Some(&mut self.root)
            } else {
                self.folder_views.get_mut(ai_box)
            };
            if let Some(canvas) = target_canvas {
                canvas.remove_reference(&reference, &self.workspace_store);
            }
        }
    }

    /// Renames a conversation in the sidecar (display only; never touches
    /// source entities). The canvas reads titles from the sidecar cache, so the
    /// card label updates on the next frame.
    fn rename_conversation(
        &mut self,
        ai_box: &ContainerId,
        conversation: &ConversationId,
        title: String,
    ) -> bool {
        let title = title.trim();
        if title.is_empty() {
            return false;
        }
        let Some(data) = self.ai_boxes.get_mut(ai_box) else {
            return false;
        };
        let ok = data.rename_conversation(conversation, title.to_owned());
        if ok {
            let _ = self.ai_store.save_box(data);
        }
        ok
    }

    /// Applies a clipboard paste into `container`. Returns `false` when the
    /// operation is not allowed (self-reference, moving within the same
    /// container, or a stale target); the menu disables `Paste` in those cases.
    fn paste_clipboard(&mut self, container: &ContainerId, entry: &ClipboardEntry) -> bool {
        if !canvas::clipboard_valid_for(entry, container, &self.all_snippets, &self.workspace) {
            return false;
        }
        // Linking into an AI box always creates a read-only `Source`: Move is
        // not allowed, so the source card stays in its original box.
        let is_ai_box = self.workspace.is_ai_box(container);
        let effective_move = matches!(entry.semantics, ClipboardSemantics::Move) && !is_ai_box;
        let new_reference_id = match &entry.target {
            ReferenceTarget::Snippet(entity_id) => {
                let result = if is_ai_box {
                    self.workspace.add_source_reference(
                        container,
                        ReferenceTarget::Snippet(entity_id.clone()),
                    )
                } else {
                    self.workspace
                        .add_snippet_reference(container, entity_id.clone())
                };
                let Ok(id) = result else {
                    return false;
                };
                id
            }
            ReferenceTarget::Container(target_id) => {
                let result = if is_ai_box {
                    self.workspace.add_source_reference(
                        container,
                        ReferenceTarget::Container(target_id.clone()),
                    )
                } else {
                    self.workspace
                        .add_container_reference(container, target_id.clone())
                };
                let Ok(id) = result else {
                    return false;
                };
                id
            }
            // Special items cannot be linked or pasted.
            ReferenceTarget::Special(_) => return false,
            // Conversation cards cannot be pasted into other containers.
            ReferenceTarget::Conversation(_) => return false,
            // External files can now be linked into AI boxes as model-readable
            // sources; their content is extracted at conversation time.
            ReferenceTarget::ExternalFile(file) => {
                let result = if is_ai_box {
                    self.workspace
                        .add_source_reference(container, ReferenceTarget::ExternalFile(file.clone()))
                } else {
                    self.workspace
                        .add_external_file_reference(container, file.clone())
                };
                let Ok(id) = result else {
                    return false;
                };
                id
            }
        };
        let role = if is_ai_box {
            MemberRole::Source
        } else {
            MemberRole::Normal
        };
        let position = self.canvas_for(container).map(|canvas| {
            canvas::default_position_for(
                container,
                &canvas.items,
                &self.all_snippets,
                &self.workspace,
                &self.ai_boxes,
                &canvas::approx_text_rects(&canvas.texts),
            )
        });
        let target_canvas = if container == &self.root.container_id {
            Some(&mut self.root)
        } else {
            self.folder_views.get_mut(container)
        };
        if let Some(canvas) = target_canvas {
            let position = position.unwrap_or_else(|| default_card_position(canvas.items.len()));
            canvas.items.push(CanvasItem {
                reference_id: new_reference_id.clone(),
                target: entry.target.clone(),
                role,
                position,
                size: egui::vec2(CARD_WIDTH, 25.0),
            });
            canvas.layout.items.insert(
                new_reference_id,
                CardLayout {
                    position,
                    color: None,
                },
            );
            canvas.save_layout(&self.workspace_store);
        }
        if effective_move {
            let _ = self
                .workspace
                .remove_reference(&entry.source_container, &entry.reference_id);
            let source_canvas = if entry.source_container == self.root.container_id {
                Some(&mut self.root)
            } else {
                self.folder_views.get_mut(&entry.source_container)
            };
            if let Some(canvas) = source_canvas {
                canvas
                    .items
                    .retain(|item| item.reference_id != entry.reference_id);
                canvas.layout.items.remove(&entry.reference_id);
                canvas.save_layout(&self.workspace_store);
            }
        }
        let _ = self.workspace_store.save(&self.workspace);
        true
    }

    /// Applies a drag-and-drop: adds a reference to `container` at `position`
    /// (or a free default slot when `None`), optionally removing the source
    /// reference (`move_semantics`). Returns `false` when the drop is not
    /// allowed (self-reference, move within the same container, stale target).
    fn apply_drop(
        &mut self,
        container: &ContainerId,
        position: Option<[f32; 2]>,
        payload: &DragPayload,
        move_semantics: bool,
    ) -> bool {
        let DragPayload::Reference {
            source_container,
            reference_id,
            target,
        } = payload;
        if !canvas::drop_valid_for(
            payload,
            container,
            &self.all_snippets,
            &self.workspace,
            move_semantics,
        ) {
            return false;
        }
        // Dragging into an AI box is always a Link to a read-only Source
        // (Shift never switches to Move, per plan_ai.md §2.4).
        let is_ai_box = self.workspace.is_ai_box(container);
        let move_semantics = move_semantics && !is_ai_box;
        let new_reference_id = match target {
            ReferenceTarget::Snippet(entity_id) => {
                let result = if is_ai_box {
                    self.workspace.add_source_reference(
                        container,
                        ReferenceTarget::Snippet(entity_id.clone()),
                    )
                } else {
                    self.workspace
                        .add_snippet_reference(container, entity_id.clone())
                };
                let Ok(id) = result else {
                    return false;
                };
                id
            }
            ReferenceTarget::Container(target_id) => {
                let result = if is_ai_box {
                    self.workspace.add_source_reference(
                        container,
                        ReferenceTarget::Container(target_id.clone()),
                    )
                } else {
                    self.workspace
                        .add_container_reference(container, target_id.clone())
                };
                let Ok(id) = result else {
                    return false;
                };
                id
            }
            // Special items cannot be linked or dropped.
            ReferenceTarget::Special(_) => return false,
            // Conversation cards cannot be dropped into other containers.
            ReferenceTarget::Conversation(_) => return false,
            // External files can now be linked into AI boxes as model-readable
            // sources; their content is extracted at conversation time.
            ReferenceTarget::ExternalFile(file) => {
                let result = if is_ai_box {
                    self.workspace
                        .add_source_reference(container, ReferenceTarget::ExternalFile(file.clone()))
                } else {
                    self.workspace
                        .add_external_file_reference(container, file.clone())
                };
                let Ok(id) = result else {
                    return false;
                };
                id
            }
        };
        let role = if is_ai_box {
            MemberRole::Source
        } else {
            MemberRole::Normal
        };
        let position = match position {
            Some(position) => position,
            None => {
                let items: &[CanvasItem] = self
                    .canvas_for(container)
                    .map(|canvas| canvas.items.as_slice())
                    .unwrap_or(&[]);
                canvas::default_position_for(
                    container,
                    items,
                    &self.all_snippets,
                    &self.workspace,
                    &self.ai_boxes,
                    &[],
                )
            }
        };
        let target_canvas = if container == &self.root.container_id {
            Some(&mut self.root)
        } else {
            self.folder_views.get_mut(container)
        };
        if let Some(canvas) = target_canvas {
            canvas.items.push(CanvasItem {
                reference_id: new_reference_id.clone(),
                target: target.clone(),
                role,
                position,
                size: egui::vec2(CARD_WIDTH, 25.0),
            });
            canvas.layout.items.insert(
                new_reference_id.clone(),
                CardLayout {
                    position,
                    color: None,
                },
            );
            canvas.save_layout(&self.workspace_store);
        } else {
            // The target container is not open: persist just the layout entry.
            let mut layout = self
                .workspace_store
                .load_layout(container)
                .unwrap_or_else(|_| ContainerLayout::empty(container.clone()));
            layout.items.insert(
                new_reference_id.clone(),
                CardLayout {
                    position,
                    color: None,
                },
            );
            let _ = self.workspace_store.save_layout(&layout);
        }
        if move_semantics && source_container != container {
            let _ = self
                .workspace
                .remove_reference(source_container, reference_id);
            let source_canvas = if source_container == &self.root.container_id {
                Some(&mut self.root)
            } else {
                self.folder_views.get_mut(source_container)
            };
            if let Some(canvas) = source_canvas {
                canvas
                    .items
                    .retain(|item| &item.reference_id != reference_id);
                canvas.layout.items.remove(reference_id);
                canvas.save_layout(&self.workspace_store);
            }
        }
        let _ = self.workspace_store.save(&self.workspace);
        true
    }

    /// End-of-frame pass for cross-window drags. A drag that leaves its source
    /// canvas keeps the pointer "down" there (egui deliberately does not turn
    /// [`egui::Event::PointerGone`] into a release), so the source canvas never
    /// observes the release itself. The egui payload is the authoritative
    /// "still dragging" signal: once it is gone (consumed by a drop target or
    /// cleared by egui on release) the drag is finalized. Cards whose drop was
    /// not consumed bounce back to their start position.
    fn finalize_drops(&mut self, ctx: &egui::Context) {
        let payload = egui::DragAndDrop::payload::<DragPayload>(ctx).map(|arc| (*arc).clone());
        let consumed = std::mem::take(&mut self.consumed_drops);
        let mut canvases: Vec<&mut ContainerCanvas> = std::iter::once(&mut self.root)
            .chain(self.folder_views.values_mut())
            .collect();
        for canvas in &mut canvases {
            let Some(drag) = canvas.dragging.clone() else {
                continue;
            };
            // While the payload still matches this drag, the user is still
            // dragging (the pointer may be over another window).
            let alive = payload.as_ref().is_some_and(|p| {
                matches!(p, DragPayload::Reference { reference_id, .. }
                    if reference_id == &drag.reference_id)
            });
            if alive {
                continue;
            }
            let card_present = drag.index < canvas.items.len()
                && canvas.items[drag.index].reference_id == drag.reference_id;
            let consumed_here =
                consumed.contains(&(canvas.container_id.clone(), drag.reference_id.clone()));
            if card_present && !consumed_here {
                // The drop was not consumed by any canvas: bounce back.
                canvas.items[drag.index].position = drag.start_position;
            }
            if card_present {
                canvas.save_layout(&self.workspace_store);
            }
            canvas.dragging = None;
        }
    }
}

/// Counts how many references across the whole workspace point at `target`.
/// A count of `1` means the link is the last one to that snippet/folder.
fn reference_count(workspace: &Workspace, target: &ReferenceTarget) -> usize {
    workspace
        .containers
        .values()
        .flat_map(|container| &container.members)
        .filter(|reference| &reference.target == target)
        .count()
}

/// Renders the global search window: a floating filter field over all
/// snippets. Typing filters title/body live (title hits first); clicking a
/// result (or pressing Enter) opens that snippet; `Esc` or the close button
/// dismisses the window. Opened via `Ctrl+F`, `/` (when nothing is being
/// edited), or the box context menu's "Search…".
///
/// The window is drawn **only on the viewport that opened it** (`search.origin`,
/// root or a folder window), mirroring the rename/delete dialogs, so the search
/// box appears on the window that initiated it. Returns the snippet the user
/// chose to open, if any (the caller applies it as a command).
fn render_search_window(
    ui: &egui::Ui,
    search: &mut SearchState,
    snippets: &BTreeMap<EntityId, Snippet>,
) -> Option<EntityId> {
    if !search.open || ui.ctx().viewport_id() != search.origin {
        return None;
    }
    let mut open = search.open;
    let mut open_id: Option<EntityId> = None;
    let mut close = false;
    egui::Window::new("Search")
        .id(egui::Id::new(("search-window", search.origin)))
        .open(&mut open)
        .default_size([360.0, 420.0])
        .collapsible(false)
        .show(ui.ctx(), |ui| {
            let filter = ui.add(
                egui::TextEdit::singleline(&mut search.filter)
                    .id(egui::Id::new(("search-filter", search.origin)))
                    .hint_text("Search snippets…"),
            );
            // Focus only once on open (IME-safe).
            if search.focus_requested {
                filter.request_focus();
                search.focus_requested = false;
            }
            // Enter opens the first result; Esc closes the window. Both actions
            // are deferred out of the closure (the window builder already
            // borrows `search.open`).
            if ui.input(|input| input.key_pressed(egui::Key::Enter))
                && let Some(id) = search_snippets(&search.filter, snippets)
                    .into_iter()
                    .next()
            {
                open_id = Some(id);
            }
            if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                ui.input_mut(|input| input.consume_key(input.modifiers, egui::Key::Escape));
                close = true;
            }
            ui.add_space(6.0);
            let list_height = (ui.ctx().viewport_rect().height() - 180.0).clamp(80.0, 320.0);
            egui::ScrollArea::vertical()
                .id_salt(("search-results", search.origin))
                .max_height(list_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let query = search.filter.trim();
                    let results = search_snippets(&search.filter, snippets);
                    if query.is_empty() {
                        ui.label("Type to search all snippets");
                    } else if results.is_empty() {
                        ui.label("(no matches)");
                    }
                    for id in results {
                        let title = &snippets[&id].title;
                        if ui.selectable_label(false, title).clicked() {
                            open_id = Some(id);
                        }
                    }
                });
        });
    if open_id.is_some() || close {
        search.open = false;
    } else {
        // Reflect the close button (egui wrote back into `open`).
        search.open = open;
    }
    open_id
}

/// Renders the "Insert External File" dialog (path text field, Browse button,
/// and Insert button). Only renders on the viewport that opened it. The Browse
/// button spawns a thread that calls the blocking `rfd` file picker so the main
/// event loop stays responsive for other windows.
///
/// Returns `Some((container, file, position))` when the user clicks Insert.
/// Renders the transient "could not open external file" toast in the viewport
/// that triggered the failed open (bottom-left, mirroring the clipboard status
/// indicator). The countdown is driven per-frame by [`HomePage::ui_impl`].
fn render_external_open_error(ui: &egui::Ui, state: &Option<(String, egui::ViewportId, u32)>) {
    let Some((message, origin, _)) = state else {
        return;
    };
    if ui.ctx().viewport_id() != *origin {
        return;
    }
    egui::Window::new("external-open-error")
        .id(egui::Id::new(("external-open-error", *origin)))
        .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(8.0, -8.0))
        .collapsible(false)
        .resizable(false)
        .title_bar(false)
        .show(ui.ctx(), |ui| {
            ui.colored_label(ui.visuals().error_fg_color, message);
        });
}

/// Opens `path` with the operating system's default application
/// (`xdg-open` on Linux, `open` on macOS, `cmd /C start` on Windows).
/// The child process is detached: FloatDea does not wait for it to exit.
fn open_path_externally(path: &str) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).spawn().map(|_| ())
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", path])
            .spawn()
            .map(|_| ())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "opening external files is not supported on this platform",
        ))
    }
}

/// The display title of an inserted external file: the file name without its
/// extension, falling back to the full path when the stem cannot be derived.
fn file_stem(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.to_owned())
}

/// Case-insensitive substring search over all snippets. Title hits come first,
/// then body hits; within each group the order follows `BTreeMap` (id) order.
/// An empty/whitespace query matches nothing.
fn search_snippets(query: &str, snippets: &BTreeMap<EntityId, Snippet>) -> Vec<EntityId> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let mut title_hits = Vec::new();
    let mut body_hits = Vec::new();
    for (id, snippet) in snippets {
        if snippet.title.to_lowercase().contains(&query) {
            title_hits.push(id.clone());
        } else if snippet.content.to_lowercase().contains(&query) {
            body_hits.push(id.clone());
        }
    }
    title_hits.append(&mut body_hits);
    title_hits
}

fn default_card_position(index: usize) -> [f32; 2] {
    [
        24.0 + (index % 16) as f32 * 200.0,
        24.0 + (index / 16) as f32 * 130.0,
    ]
}

impl App for HomePage {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui_impl(ui);
    }
}

impl HomePage {
    /// Per-frame UI entry shared by [`App::ui`] and tests. Kept free of
    /// [`eframe::Frame`] so a whole frame can be driven headlessly with
    /// [`egui::Context::run_ui`].
    pub(crate) fn ui_impl(&mut self, ui: &mut egui::Ui) {
        self.apply_theme(ui.ctx());
        // Apply worker events that arrived since the last frame (streaming
        // deltas, completed turns, failures).
        self.drain_ai_events();
        // Count down the transient "could not open external file" toast.
        if let Some((_, _, frames)) = &mut self.external_open_error {
            if *frames == 0 {
                self.external_open_error = None;
            } else {
                *frames -= 1;
            }
        }
        let root_viewport = ui.ctx().viewport_id();
        // In full-window mode all snippet/folder windows float inside the root
        // viewport, so their dialogs and clipboard entries also carry the root
        // origin and are rendered at the root level.
        let floating = self.settings.window_mode == WindowMode::Floating;
        if floating {
            // The root `ui` handed to `App::ui` has no background; in full-window
            // mode nothing else fills the main window (the root box is itself a
            // floating window), so paint a theme-aware desktop fill instead of
            // egui's near-black clear color. Light theme → light gray/white,
            // dark theme → dark gray.
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, ui.visuals().panel_fill);
        }
        let root_commands = self.render_home_panel(ui);
        self.process_canvas_commands(root_commands, root_viewport);
        if floating {
            // Full-window mode: the root window's close button opens an "Exit?"
            // confirmation (only present in this mode; native mode exits directly).
            self.render_root_exit_dialog(ui.ctx());
        } else {
            self.root_exit_pending = false;
        }
        self.render_delete_dialog(ui);
        self.render_rename_dialog(ui);

        // Title index shared by the editor's "Insert Link…" picker, the paste
        // menu, and drag & drop into a note (looked up by id, so it stays
        // correct even while a snippet is mutably borrowed below).
        // (id, title, content) snapshot shared by link insertion and inline
        // embeds (id/content looked up by id, so it stays correct even while a
        // snippet is mutably borrowed below).
        let snippet_index: Vec<(EntityId, String, String)> = self
            .all_snippets
            .iter()
            .map(|(id, snippet)| (id.clone(), snippet.title.clone(), snippet.content.clone()))
            .collect();
        let mut closed_views = Vec::new();
        let mut open_views = Vec::new();
        for view in &mut self.views {
            let Some(item) = self.all_snippets.get_mut(&view.entity_id) else {
                continue;
            };
            let action = if floating {
                Self::render_snippet_window(
                    ui,
                    view,
                    item,
                    &self.store,
                    &snippet_index,
                    &self.clipboard,
                    &self.math_renderer,
                    &self.settings,
                )
            } else {
                Self::render_snippet_viewport(
                    ui,
                    view,
                    item,
                    &self.store,
                    &snippet_index,
                    &self.clipboard,
                    &self.math_renderer,
                    &self.settings,
                )
            };
            match action {
                ViewAction::Close => closed_views.push(view.id),
                ViewAction::OpenSnippet(id) => open_views.push(id),
                ViewAction::None => {}
            }
        }
        self.views.retain(|view| !closed_views.contains(&view.id));
        for id in open_views {
            if self.all_snippets.contains_key(&id) {
                self.open_view(id);
            }
        }

        let mut commands_by_viewport: Vec<(egui::ViewportId, Vec<CanvasCommand>)> = Vec::new();
        for (container_id, canvas) in &mut self.folder_views {
            let title = self
                .workspace
                .containers
                .get(container_id)
                .map(|container| container.title.clone())
                .unwrap_or_default();
            let commands = if floating {
                Self::render_folder_window(
                    ui,
                    canvas,
                    &title,
                    &mut self.workspace,
                    &self.workspace_store,
                    &self.store,
                    &mut self.all_snippets,
                    &mut self.clipboard,
                    &mut self.os_file_drop_consumed,
                    &self.ai_boxes,
                    self.settings.snap_to_grid,
                    self.settings.show_grid,
                    &mut self.hover_preview,
                )
            } else {
                Self::render_folder_viewport(
                    ui,
                    canvas,
                    &title,
                    &mut self.workspace,
                    &self.workspace_store,
                    &self.store,
                    &mut self.all_snippets,
                    &mut self.rename_dialog,
                    &mut self.pending_delete,
                    &mut self.search,
                    &mut self.clipboard,
                    &self.external_open_error,
                    &mut self.os_file_drop_consumed,
                    &self.ai_boxes,
                    self.settings.snap_to_grid,
                    self.settings.show_grid,
                    &mut self.hover_preview,
                )
            };
            let viewport_id = if floating {
                // All floating windows share the root viewport; dialogs are
                // rendered at the root level in this mode.
                egui::ViewportId::ROOT
            } else {
                egui::ViewportId::from_hash_of(("folder-view", container_id.as_str()))
            };
            commands_by_viewport.push((viewport_id, commands));
        }
        for (viewport_id, commands) in commands_by_viewport {
            self.process_canvas_commands(commands, viewport_id);
        }
        // Finalize any cross-window drag whose payload was consumed or cleared
        // during this frame (bouncing back un-consumed drops).
        self.finalize_drops(ui.ctx());
        // The system settings window floats above the root canvas.
        self.render_settings_window(ui.ctx());
        // The global search window (rendered only on its originating viewport;
        // a folder window renders it inside its own viewport pass).
        if let Some(id) = render_search_window(ui, &mut self.search, &self.all_snippets) {
            self.open_view(id);
        }
        // Poll the pending file picker result (spawned in a thread to avoid
        // blocking the main event loop).
        if let Some(pending) = self.pending_file_picker.as_ref() {
            if let Ok(Some(path)) = pending.rx.try_recv() {
                let container = pending.container.clone();
                let file = ExternalFileRef {
                    id: ExternalFileId::new(),
                    title: file_stem(&path),
                    path,
                };
                self.pending_file_picker = None;
                self.insert_external_file(&container, file, None);
            } else if pending.rx.try_recv().is_ok() {
                // Cancelled.
                self.pending_file_picker = None;
            }
        }
        // The transient "could not open external file" toast.
        render_external_open_error(ui, &self.external_open_error);
        // Diagnostics: a frame-global OS file drop that no canvas consumed means
        // the events arrived but were rejected (e.g. the drag did not offer
        // `text/uri-list`), which is worth reporting to the user.
        if ui.input(|input| !input.raw.dropped_files.is_empty())
            && !self.os_file_drop_consumed
        {
            log::warn!(
                "OS file drop received but not consumed by any canvas (drag type unsupported?)"
            );
        }
        // The AI conversation window (if open).
        self.render_ai_conversation_window(ui);
        // Repaint while a turn is streaming so deltas appear without waiting
        // for user input (throttled: the worker event channel bounds the rate).
        if self.ai_active_turn.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    /// Drains AI worker events once per frame and applies them to the
    /// conversation state. Events that do not match the active turn identity
    /// are dropped (late events from a cancelled/duplicated/deleted turn must
    /// never land in the wrong conversation).
    fn drain_ai_events(&mut self) {
        while let Ok(event) = self.ai_events.try_recv() {
            match event {
                TurnEvent::TitleGenerated { identity, title } => {
                    let title = title.trim().to_owned();
                    if !title.is_empty()
                        && self
                            .ai_boxes
                            .get(&identity.ai_box)
                            .and_then(|data| data.get(&identity.conversation))
                            .map(|c| c.title == "New Conversation")
                            .unwrap_or(false)
                    {
                        self.rename_conversation(
                            &identity.ai_box,
                            &identity.conversation,
                            title,
                        );
                    }
                }
                TurnEvent::Delta { identity, delta } => {
                    let Some(ref active) = self.ai_active_turn else {
                        continue;
                    };
                    if identity != *active {
                        continue;
                    }
                    self.ai_streaming.push_str(&delta);
                }
                TurnEvent::Done {
                    identity,
                    content,
                    usage,
                    tools,
                    proposal,
                } => {
                    let Some(ref active) = self.ai_active_turn else {
                        continue;
                    };
                    if identity != *active {
                        continue;
                    }
                    self.ai_active_turn = None;
                    let snapshots = std::mem::take(&mut self.ai_snapshots);
                    // Tool receipts are stored on the assistant message as
                    // independent, visible events; a `core.create_output_proposal`
                    // call surfaces as the Apply/Reject proposal card.
                    self.push_assistant(
                        &identity,
                        &content,
                        MessageStatus::Completed,
                        snapshots,
                        usage,
                        tools,
                        proposal,
                    );
                    // Auto-generate a title after the first turn completes.
                    if self.should_generate_title(&identity.ai_box, &identity.conversation) {
                        self.submit_title_generation(&identity.ai_box, &identity.conversation);
                    }
                }
                TurnEvent::Failed { identity, error } => {
                    let Some(ref active) = self.ai_active_turn else {
                        continue;
                    };
                    if identity != *active {
                        continue;
                    }
                    let partial = std::mem::take(&mut self.ai_streaming);
                    let snapshots = std::mem::take(&mut self.ai_snapshots);
                    let status = if error.kind == AiErrorKind::Cancelled {
                        MessageStatus::Stopped
                    } else {
                        MessageStatus::Failed
                    };
                    let content = if partial.is_empty() {
                        format!("(error: {})", error.message)
                    } else {
                        partial
                    };
                    self.ai_active_turn = None;
                    self.push_assistant(
                        &identity,
                        &content,
                        status,
                        snapshots,
                        TokenUsage::default(),
                        Vec::new(),
                        None,
                    );
                }
            }
        }
    }

    /// Returns true when the conversation still has the default title and has
    /// at least one message (meaning the first turn just completed).
    fn should_generate_title(&self, ai_box: &ContainerId, conversation: &ConversationId) -> bool {
        let Some(data) = self.ai_boxes.get(ai_box) else {
            return false;
        };
        let Some(conv) = data.get(conversation) else {
            return false;
        };
        conv.title == "New Conversation" && !conv.messages.is_empty()
    }

    /// Submits a lightweight title-generation task to the AI worker. Uses the
    /// summarizer model when configured, falls back to the main model.
    fn submit_title_generation(
        &mut self,
        ai_box: &ContainerId,
        conversation: &ConversationId,
    ) {
        let Some(data) = self.ai_boxes.get(ai_box) else {
            return;
        };
        let Some(conv) = data.get(conversation) else {
            return;
        };
        let Some(first_user) = conv
            .messages
            .iter()
            .find(|m| m.role == MessageRole::User)
        else {
            return;
        };
        let Some(provider) = self.summarizer_provider_arc() else {
            return;
        };
        let system = "Generate a concise title (4-6 words) for this conversation \
            based on the user's first message. Respond with ONLY the title, \
            no quotes, no punctuation, no explanation.";
        let request = ChatRequest::new(
            Some(system.to_owned()),
            vec![ChatMessage::user(first_user.content.clone())],
        );
        let identity = TurnIdentity {
            ai_box: ai_box.clone(),
            conversation: conversation.clone(),
            task: TurnTaskId::new(),
        };
        let _ = self.ai_worker.generate_title(identity, request, provider);
    }

    /// Builds a provider configured for the summarizer model (falls back to
    /// the main model when `summarizer_model` is blank in settings).
    fn summarizer_provider_arc(&self) -> Option<Arc<dyn ChatProvider>> {
        if let Some(provider) = &self.ai_provider_override {
            return Some(provider.clone());
        }
        if !self.settings.ai_enabled {
            return None;
        }
        let config = self.settings.summarizer_provider_config();
        build_provider(&config).ok().map(Arc::from)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    struct TestFolder(PathBuf);

    impl TestFolder {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(
                std::env::temp_dir()
                    .join(format!("floatdea-home-page-{}-{nonce}", std::process::id())),
            )
        }
    }

    impl Drop for TestFolder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn restores_root_card_positions() {
        let folder = TestFolder::new();
        let mut first_session = HomePage::new(&folder.0);
        first_session.root.items[0].position = [137.0, 281.0];
        first_session
            .root
            .save_layout(&first_session.workspace_store);

        let second_session = HomePage::new(&folder.0);

        assert_eq!(second_session.root.items[0].position, [137.0, 281.0]);
    }

    #[test]
    fn restores_snippet_and_folder_positions_from_one_layout() {
        let folder = TestFolder::new();
        let mut first_session = HomePage::new(&folder.0);
        let folder_id = first_session.workspace.create_container("Folder");
        let reference_id = first_session
            .workspace
            .add_container_to_root(folder_id.clone());
        first_session
            .workspace_store
            .save(&first_session.workspace)
            .unwrap();
        first_session.root.items.push(CanvasItem {
            reference_id,
            target: ReferenceTarget::Container(folder_id),
            role: MemberRole::Normal,
            position: [320.0, 144.0],
            size: egui::vec2(CARD_WIDTH, 25.0),
        });
        first_session
            .root
            .save_layout(&first_session.workspace_store);

        let second_session = HomePage::new(&folder.0);

        assert!(second_session.root.items.iter().any(|item| {
            matches!(item.target, ReferenceTarget::Container(_)) && item.position == [320.0, 144.0]
        }));
    }

    fn clip_root_first_entry(page: &HomePage, semantics: ClipboardSemantics) -> ClipboardEntry {
        ClipboardEntry {
            source_container: page.root.container_id.clone(),
            reference_id: page.root.items[0].reference_id.clone(),
            target: page.root.items[0].target.clone(),
            semantics,
            origin: egui::ViewportId::ROOT,
        }
    }

    #[test]
    fn pastes_link_reference_into_folder() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        page.open_folder(&folder_id);

        let entry = clip_root_first_entry(&page, ClipboardSemantics::Link);
        let root_count = page.root.items.len();
        assert!(page.paste_clipboard(&folder_id, &entry));
        assert_eq!(
            page.root.items.len(),
            root_count,
            "link keeps the source card"
        );
        let folder_canvas = page.folder_views.get(&folder_id).expect("folder view open");
        assert_eq!(folder_canvas.items.len(), 1);
        assert_eq!(folder_canvas.items[0].target, entry.target);
        assert_eq!(page.workspace.containers[&folder_id].members.len(), 1);
    }

    #[test]
    fn pastes_move_reference_into_folder() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        page.open_folder(&folder_id);

        let entry = clip_root_first_entry(&page, ClipboardSemantics::Move);
        let root_count = page.root.items.len();
        assert!(page.paste_clipboard(&folder_id, &entry));
        assert_eq!(
            page.root.items.len(),
            root_count - 1,
            "move removes the source card"
        );
        assert_eq!(page.workspace.containers[&folder_id].members.len(), 1);
        // The entity itself is untouched; only the reference moved.
        assert!(page.all_snippets.contains_key(match &entry.target {
            ReferenceTarget::Snippet(id) => id,
            ReferenceTarget::Container(_) => panic!("root cards are snippets"),
            ReferenceTarget::Special(_) => panic!("clipboard entries never target specials"),
            ReferenceTarget::Conversation(_) => {
                panic!("clipboard entries never target conversations")
            }
            ReferenceTarget::ExternalFile(_) => {
                panic!("clipboard entries never target external files")
            }
        }));
    }

    #[test]
    fn pasted_reference_survives_restart() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        let entry = clip_root_first_entry(&page, ClipboardSemantics::Link);
        assert!(page.paste_clipboard(&folder_id, &entry));
        drop(page);

        let reloaded = HomePage::new(&folder.0);
        assert_eq!(reloaded.workspace.containers[&folder_id].members.len(), 1);
    }

    #[test]
    fn rejects_self_reference_paste() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let reference_id = page.workspace.add_container_to_root(folder_id.clone());
        let entry = ClipboardEntry {
            source_container: page.root.container_id.clone(),
            reference_id,
            target: ReferenceTarget::Container(folder_id.clone()),
            semantics: ClipboardSemantics::Link,
            origin: egui::ViewportId::ROOT,
        };
        assert!(!page.paste_clipboard(&folder_id, &entry));
    }

    #[test]
    fn drop_creates_link_reference_in_folder() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        page.open_folder(&folder_id);

        let (entity_id, reference_id) = root_first_snippet(&page);
        let payload = DragPayload::Reference {
            source_container: page.root.container_id.clone(),
            reference_id,
            target: ReferenceTarget::Snippet(entity_id),
        };
        let root_count = page.root.items.len();
        assert!(page.apply_drop(&folder_id, Some([88.0, 44.0]), &payload, false));
        // Link keeps the source card.
        assert_eq!(page.root.items.len(), root_count);
        let folder_canvas = page.folder_views.get(&folder_id).expect("folder view open");
        assert_eq!(folder_canvas.items.len(), 1);
        assert_eq!(folder_canvas.items[0].position, [88.0, 44.0]);
        assert_eq!(page.workspace.containers[&folder_id].members.len(), 1);
    }

    #[test]
    fn drop_moves_reference_into_folder() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        page.open_folder(&folder_id);

        let (entity_id, reference_id) = root_first_snippet(&page);
        let payload = DragPayload::Reference {
            source_container: page.root.container_id.clone(),
            reference_id,
            target: ReferenceTarget::Snippet(entity_id.clone()),
        };
        let root_count = page.root.items.len();
        assert!(page.apply_drop(&folder_id, None, &payload, true));
        assert_eq!(
            page.root.items.len(),
            root_count - 1,
            "move removes the source card"
        );
        assert_eq!(page.workspace.containers[&folder_id].members.len(), 1);
        // The entity itself is untouched; only the reference moved.
        assert!(page.all_snippets.contains_key(&entity_id));
    }

    #[test]
    fn drop_rejects_self_reference() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let reference_id = page.workspace.add_container_to_root(folder_id.clone());
        let payload = DragPayload::Reference {
            source_container: page.root.container_id.clone(),
            reference_id,
            target: ReferenceTarget::Container(folder_id.clone()),
        };
        assert!(!page.apply_drop(&folder_id, None, &payload, false));
    }

    #[test]
    fn drop_rejects_move_into_same_container() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let (entity_id, reference_id) = root_first_snippet(&page);
        let payload = DragPayload::Reference {
            source_container: page.root.container_id.clone(),
            reference_id,
            target: ReferenceTarget::Snippet(entity_id),
        };
        let root_id = page.root.container_id.clone();
        let root_count = page.root.items.len();
        assert!(!page.apply_drop(&root_id, Some([24.0, 24.0]), &payload, true));
        assert_eq!(page.root.items.len(), root_count);
    }

    #[test]
    fn drop_command_records_consumption() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        let (entity_id, reference_id) = root_first_snippet(&page);
        let payload = DragPayload::Reference {
            source_container: page.root.container_id.clone(),
            reference_id: reference_id.clone(),
            target: ReferenceTarget::Snippet(entity_id),
        };
        page.process_canvas_commands(
            vec![CanvasCommand::DropOnCanvas {
                container: folder_id.clone(),
                position: None,
                payload,
                move_semantics: false,
            }],
            egui::ViewportId::ROOT,
        );
        assert!(
            page.consumed_drops
                .contains(&(page.root.container_id.clone(), reference_id))
        );
        assert_eq!(page.workspace.containers[&folder_id].members.len(), 1);
    }

    #[test]
    fn rejects_move_into_same_container() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let entry = clip_root_first_entry(&page, ClipboardSemantics::Move);
        let root_count = page.root.items.len();
        let root_id = page.root.container_id.clone();
        assert!(!page.paste_clipboard(&root_id, &entry));
        assert_eq!(page.root.items.len(), root_count);
    }

    #[test]
    fn link_paste_command_clears_clipboard() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        page.open_folder(&folder_id);
        let entry = clip_root_first_entry(&page, ClipboardSemantics::Link);
        page.clipboard = Some(entry.clone());
        page.process_canvas_commands(
            vec![CanvasCommand::PasteClipboard {
                container: folder_id,
                entry,
            }],
            egui::ViewportId::ROOT,
        );
        assert!(page.clipboard.is_none());
    }

    #[test]
    fn opening_snippet_clears_clipboard() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let entry = clip_root_first_entry(&page, ClipboardSemantics::Link);
        page.clipboard = Some(entry);
        let snippet_id = match &page.root.items[0].target {
            ReferenceTarget::Snippet(id) => id.clone(),
            ReferenceTarget::Container(_) => panic!("root cards are snippets"),
            ReferenceTarget::Special(_) => panic!("root cards are snippets"),
            ReferenceTarget::Conversation(_) => panic!("root cards are snippets"),
            ReferenceTarget::ExternalFile(_) => panic!("root cards are snippets"),
        };
        page.open_view(snippet_id);
        assert!(page.clipboard.is_none());
    }

    #[test]
    fn root_always_holds_a_settings_special_card() {
        let folder = TestFolder::new();
        let page = HomePage::new(&folder.0);
        assert!(page.root.items.iter().any(|item| matches!(
            &item.target,
            ReferenceTarget::Special(SpecialKind::Settings)
        )));
    }

    #[test]
    fn settings_special_cannot_be_pasted_or_dropped() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let (special_reference, special_target) = {
            let special = page
                .root
                .items
                .iter()
                .find(|item| matches!(item.target, ReferenceTarget::Special(_)))
                .expect("root has a settings special card");
            (special.reference_id.clone(), special.target.clone())
        };
        let folder_id = page.workspace.create_container("Folder");
        let entry = ClipboardEntry {
            source_container: page.root.container_id.clone(),
            reference_id: special_reference.clone(),
            target: special_target.clone(),
            semantics: ClipboardSemantics::Link,
            origin: egui::ViewportId::ROOT,
        };
        assert!(
            !page.paste_clipboard(&folder_id, &entry),
            "special items cannot be pasted"
        );
        let payload = DragPayload::Reference {
            source_container: page.root.container_id.clone(),
            reference_id: special_reference.clone(),
            target: special_target.clone(),
        };
        assert!(
            !page.apply_drop(&folder_id, None, &payload, false),
            "special items cannot be dropped"
        );
        assert_eq!(page.workspace.containers[&folder_id].members.len(), 0);
    }

    #[test]
    fn opening_special_opens_the_settings_window() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        assert!(!page.settings_open);
        page.process_canvas_commands(
            vec![CanvasCommand::OpenSpecial(SpecialKind::Settings)],
            egui::ViewportId::ROOT,
        );
        assert!(page.settings_open);
    }

    #[test]
    fn open_search_command_opens_the_search_window_on_the_originating_viewport() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        assert!(!page.search.open);
        let folder_viewport = egui::ViewportId::from_hash_of(("folder-view", "some-container"));
        page.process_canvas_commands(
            vec![CanvasCommand::OpenSearch],
            folder_viewport,
        );
        assert!(page.search.open);
        assert!(page.search.focus_requested);
        assert_eq!(
            page.search.origin, folder_viewport,
            "the search window remembers the viewport that opened it"
        );
    }

    #[test]
    fn closing_the_originating_viewport_closes_the_search_window() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_viewport = egui::ViewportId::from_hash_of(("folder-view", "some-container"));
        page.process_canvas_commands(vec![CanvasCommand::OpenSearch], folder_viewport);
        assert!(page.search.open);

        // Closing the folder window that opened the search dismisses it; other
        // viewports leave it untouched.
        page.clear_search_for_viewport(folder_viewport);
        assert!(!page.search.open);

        page.process_canvas_commands(vec![CanvasCommand::OpenSearch], folder_viewport);
        assert!(page.search.open);
        page.clear_search_for_viewport(egui::ViewportId::ROOT);
        assert!(page.search.open, "unrelated viewport closes leave search open");
    }

    #[test]
    fn search_snippets_prioritizes_titles_and_is_case_insensitive() {
        let mut snippets = BTreeMap::new();
        let insert = |snippets: &mut BTreeMap<EntityId, Snippet>, title: &str, content: &str| {
            let id = EntityId::new();
            snippets.insert(
                id.clone(),
                Snippet {
                    id,
                    title: title.to_owned(),
                    content: content.to_owned(),
                },
            );
        };
        insert(&mut snippets, "Math Notes", "y = x^2");
        insert(&mut snippets, "Shopping", "milk and math textbooks");
        insert(&mut snippets, "Journal", "a walk by the lake");
        let id_of = |title: &str| {
            snippets
                .iter()
                .find(|(_, snippet)| snippet.title == title)
                .map(|(id, _)| id.clone())
                .expect("snippet exists")
        };
        let math_id = id_of("Math Notes");
        let shopping_id = id_of("Shopping");

        // Title hit comes before a body hit.
        let results = search_snippets("math", &snippets);
        assert_eq!(results[0], math_id, "title match ranks first");
        assert!(results.contains(&shopping_id), "body match is found too");

        // Case-insensitive.
        assert_eq!(search_snippets("MILK", &snippets), vec![shopping_id]);

        // Empty query matches nothing.
        assert!(search_snippets("   ", &snippets).is_empty());
    }

    #[test]
    fn organize_arranges_cards_column_major_within_viewport() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        // Scatter the cards off-grid.
        for (index, item) in page.root.items.iter_mut().enumerate() {
            item.position = [1000.0 + index as f32, 1000.0];
        }
        let viewport_height = 480.0;
        // Snap off → organize lays cards out on the grid with breathing room.
        page.settings.snap_to_grid = false;
        page.root.organize(
            &page.workspace_store,
            viewport_height,
            page.settings.snap_to_grid,
        );

        // Mirror the layout math used by `organize` to derive the expected grid.
        const GRID: f32 = 32.0;
        const MARGIN: f32 = GRID;
        const STEP_X: f32 = 6.0 * GRID;
        const GAP_Y: f32 = 12.0;
        const MIN_CARD_HEIGHT: f32 = 25.0;
        let max_height = page
            .root
            .items
            .iter()
            .map(|item| item.size.y)
            .fold(MIN_CARD_HEIGHT, f32::max)
            .max(MIN_CARD_HEIGHT);
        let step_y = ((max_height + GAP_Y) / GRID).ceil() * GRID;
        let rows_per_column = (((viewport_height - MARGIN) / step_y).floor() as usize).max(1);
        // Every organized card corner is exactly on a grid point.
        assert!(
            page.root
                .items
                .iter()
                .all(|item| { item.position[0] % GRID == 0.0 && item.position[1] % GRID == 0.0 }),
            "organize always aligns cards to the grid"
        );

        for (index, item) in page.root.items.iter().enumerate() {
            let column = index / rows_per_column;
            let row = index % rows_per_column;
            assert_eq!(
                item.position,
                [
                    MARGIN + column as f32 * STEP_X,
                    MARGIN + row as f32 * step_y
                ]
            );
        }
        // The first column never exceeds the viewport height.
        let first_column_bottom = MARGIN
            + (page.root.items.len().min(rows_per_column).saturating_sub(1)) as f32 * step_y
            + max_height;
        assert!(first_column_bottom <= viewport_height);

        // The grid layout is persisted across sessions.
        let reloaded = HomePage::new(&folder.0);
        for (index, item) in reloaded.root.items.iter().enumerate() {
            let column = index / rows_per_column;
            let row = index % rows_per_column;
            assert_eq!(
                item.position,
                [
                    MARGIN + column as f32 * STEP_X,
                    MARGIN + row as f32 * step_y
                ]
            );
        }
    }

    #[test]
    fn organize_dense_packs_cards_without_gaps() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        for (index, item) in page.root.items.iter_mut().enumerate() {
            item.position = [1000.0 + index as f32, 1000.0];
        }
        let viewport_height = 480.0;
        // Snap on → organize packs densely without gaps.
        page.settings.snap_to_grid = true;
        page.root.organize(
            &page.workspace_store,
            viewport_height,
            page.settings.snap_to_grid,
        );

        // Dense: column pitch = card width, row pitch = tallest card, no gaps.
        const MARGIN: f32 = 32.0;
        let max_height = page
            .root
            .items
            .iter()
            .map(|item| item.size.y)
            .fold(25.0, f32::max)
            .max(25.0);
        let rows_per_column = (((viewport_height - MARGIN) / max_height).floor() as usize).max(1);
        for (index, item) in page.root.items.iter().enumerate() {
            let column = index / rows_per_column;
            let row = index % rows_per_column;
            assert_eq!(
                item.position,
                [
                    MARGIN + column as f32 * CARD_WIDTH,
                    MARGIN + row as f32 * max_height
                ]
            );
        }
        // Adjacent cards touch: vertical gap within a column and horizontal gap
        // across columns are both zero. Column-major order means items i and
        // i+1 share a column; items i and i+rows_per_column share a row.
        for i in 0..page.root.items.len() {
            if (i + 1) % rows_per_column != 0 && i + 1 < page.root.items.len() {
                let top = &page.root.items[i];
                let bottom = &page.root.items[i + 1];
                assert!(
                    (top.position[1] + max_height - bottom.position[1]).abs() < 0.01,
                    "dense rows touch vertically"
                );
            }
            if i + rows_per_column < page.root.items.len() {
                let left = &page.root.items[i];
                let right = &page.root.items[i + rows_per_column];
                assert!(
                    (left.position[0] + CARD_WIDTH - right.position[0]).abs() < 0.01,
                    "dense columns touch horizontally"
                );
            }
        }
    }

    fn root_first_snippet(page: &HomePage) -> (EntityId, ReferenceId) {
        let entity_id = match &page.root.items[0].target {
            ReferenceTarget::Snippet(id) => id.clone(),
            ReferenceTarget::Container(_) => panic!("root cards are snippets"),
            ReferenceTarget::Special(_) => panic!("root cards are snippets"),
            ReferenceTarget::Conversation(_) => panic!("root cards are snippets"),
            ReferenceTarget::ExternalFile(_) => panic!("root cards are snippets"),
        };
        (entity_id, page.root.items[0].reference_id.clone())
    }

    #[test]
    fn delete_reference_when_not_last_link_removes_only_the_link() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        page.open_folder(&folder_id);

        // Give the folder a second link to the same snippet so it is not the last one.
        let (entity_id, root_ref) = root_first_snippet(&page);
        let folder_ref = page
            .workspace
            .add_snippet_reference(&folder_id, entity_id.clone())
            .unwrap();
        page.workspace_store.save(&page.workspace).unwrap();
        page.folder_views
            .get_mut(&folder_id)
            .unwrap()
            .items
            .push(CanvasItem {
                reference_id: folder_ref,
                target: ReferenceTarget::Snippet(entity_id.clone()),
                role: MemberRole::Normal,
                position: [24.0, 24.0],
                size: egui::vec2(CARD_WIDTH, 25.0),
            });

        let root_count = page.root.items.len();
        page.process_canvas_commands(
            vec![CanvasCommand::DeleteReference {
                owner: page.root.container_id.clone(),
                reference: root_ref,
                target: ReferenceTarget::Snippet(entity_id.clone()),
            }],
            egui::ViewportId::ROOT,
        );

        // Not the last link → no confirmation dialog, entity untouched.
        assert!(page.pending_delete.is_none());
        assert_eq!(page.root.items.len(), root_count - 1);
        assert!(
            page.folder_views[&folder_id]
                .items
                .iter()
                .any(|item| item.target == ReferenceTarget::Snippet(entity_id.clone()))
        );
        assert!(page.all_snippets.contains_key(&entity_id));
    }

    #[test]
    fn delete_reference_when_last_link_asks_then_confirm_removes_entity() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let (entity_id, root_ref) = root_first_snippet(&page);
        let root_count = page.root.items.len();

        page.process_canvas_commands(
            vec![CanvasCommand::DeleteReference {
                owner: page.root.container_id.clone(),
                reference: root_ref,
                target: ReferenceTarget::Snippet(entity_id.clone()),
            }],
            egui::ViewportId::ROOT,
        );

        // Last link → confirmation dialog pending; nothing removed yet.
        assert!(page.pending_delete.is_some());
        assert_eq!(page.root.items.len(), root_count);
        assert!(page.all_snippets.contains_key(&entity_id));

        page.confirm_delete(page.pending_delete.clone().unwrap());
        assert!(page.pending_delete.is_none());
        assert!(!page.all_snippets.contains_key(&entity_id));
        assert_eq!(page.root.items.len(), root_count - 1);
    }

    #[test]
    fn confirm_delete_last_folder_link_removes_container_and_window() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let reference_id = page.workspace.add_container_to_root(folder_id.clone());
        page.open_folder(&folder_id);
        assert!(page.folder_views.contains_key(&folder_id));

        page.confirm_delete(PendingDelete {
            owner: page.root.container_id.clone(),
            reference: reference_id,
            target: ReferenceTarget::Container(folder_id.clone()),
            origin: egui::ViewportId::ROOT,
        });

        assert!(!page.workspace.containers.contains_key(&folder_id));
        assert!(!page.folder_views.contains_key(&folder_id));
    }

    #[test]
    fn create_text_enters_edit_and_persists_across_sessions() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        {
            let mut os_file_drop_consumed = false;
            let mut hover_preview = HoverPreview::default();
            let mut data = canvas::CanvasData::new(
                &mut page.all_snippets,
                &mut page.workspace,
                &page.workspace_store,
                &page.store,
                &mut page.clipboard,
                &page.ai_boxes,
                page.settings.snap_to_grid,
                page.settings.show_grid,
                true,
                false,
                &mut os_file_drop_consumed,
                &mut hover_preview,
            );
            canvas::create_text(&mut page.root, &mut data, [24.0, 36.0]);

            assert_eq!(page.root.texts.len(), 1);
            let text_id = page.root.texts[0].id.clone();
            assert_eq!(page.root.editing_text, Some(text_id));
        }
        drop(page);

        let reloaded = HomePage::new(&folder.0);
        assert_eq!(reloaded.root.texts.len(), 1);
        assert_eq!(reloaded.root.texts[0].position, [24.0, 36.0]);
        assert!(reloaded.root.texts[0].text.is_empty());
    }

    #[test]
    fn delete_text_removes_it_and_clears_edit_state() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        {
            let mut os_file_drop_consumed = false;
            let mut hover_preview = HoverPreview::default();
            let mut data = canvas::CanvasData::new(
                &mut page.all_snippets,
                &mut page.workspace,
                &page.workspace_store,
                &page.store,
                &mut page.clipboard,
                &page.ai_boxes,
                page.settings.snap_to_grid,
                page.settings.show_grid,
                true,
                false,
                &mut os_file_drop_consumed,
                &mut hover_preview,
            );
            canvas::create_text(&mut page.root, &mut data, [24.0, 24.0]);
            let text_id = page.root.texts[0].id.clone();

            canvas::delete_text(&mut page.root, &mut data, &text_id);

            assert!(page.root.texts.is_empty());
            assert!(page.root.editing_text.is_none());
        }
        drop(page);

        let reloaded = HomePage::new(&folder.0);
        assert!(reloaded.root.texts.is_empty());
    }

    #[test]
    fn snaps_positions_to_the_nearest_grid_point() {
        assert_eq!(canvas::snap_position([25.0, 47.0]), [32.0, 32.0]);
        assert_eq!(canvas::snap_position([63.0, 49.0]), [64.0, 64.0]);
        assert_eq!(canvas::snap_position([32.0, 96.0]), [32.0, 96.0]);
    }

    /// A single-frame `RawInput` with the pointer at `pos`. `button` presses or
    /// releases the primary button on that frame; `None` is a pure move.
    fn pointer_frame(
        pos: egui::Pos2,
        button: Option<(egui::PointerButton, bool)>,
    ) -> egui::RawInput {
        let mut events = vec![egui::Event::PointerMoved(pos)];
        if let Some((button, pressed)) = button {
            events.push(egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 1000.0),
            )),
            events,
            ..Default::default()
        }
    }

    #[test]
    fn floating_windows_accept_cross_window_card_drops() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        page.settings.window_mode = WindowMode::Floating;
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        page.workspace_store.save(&page.workspace).unwrap();
        page.open_folder(&folder_id);
        let root_card_count = page.root.items.len();

        // Deterministic window placement: the root box window opens at (8,8)
        // and the folder box window at (680,60), so root card 0 (a snippet,
        // the first of the default layout) is near (90,67) and the folder's
        // empty canvas covers (1000,250).
        let press = egui::pos2(90.0, 67.0);
        let drop = egui::pos2(1000.0, 250.0);

        let ctx = egui::Context::default();
        // Settle the floating windows: egui runs sizing passes on the first
        // frames, which are unreliable for hit-testing.
        for _ in 0..3 {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| page.ui_impl(ui));
        }
        // Park the pointer, then press on the root card.
        let _ = ctx.run_ui(pointer_frame(press, None), |ui| page.ui_impl(ui));
        let _ = ctx.run_ui(
            pointer_frame(press, Some((egui::PointerButton::Primary, true))),
            |ui| page.ui_impl(ui),
        );
        // Drag over to the folder window (drag starts, payload published).
        let _ = ctx.run_ui(pointer_frame(drop, None), |ui| page.ui_impl(ui));
        // Release over the folder canvas → DropOnCanvas → reference created.
        let _ = ctx.run_ui(
            pointer_frame(drop, Some((egui::PointerButton::Primary, false))),
            |ui| page.ui_impl(ui),
        );

        assert_eq!(
            page.workspace.containers[&folder_id].members.len(),
            1,
            "a card dropped onto another floating window's canvas creates a reference"
        );
        let folder_canvas = page.folder_views.get(&folder_id).expect("folder view open");
        assert_eq!(folder_canvas.items.len(), 1);
        assert!(matches!(
            folder_canvas.items[0].target,
            ReferenceTarget::Snippet(_)
        ));
        // Link semantics: the source card stays in the root box.
        assert_eq!(page.root.items.len(), root_card_count);
    }

    #[test]
    fn hover_preview_anchors_to_the_card_not_the_pointer() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let first = &page.root.items[0];
        // Root canvas starts at the central-panel inset (8) inside the main
        // window; a card at `position` renders at canvas_rect.min + position.
        let card_min = egui::pos2(8.0 + first.position[0], 8.0 + first.position[1]);
        let inside = card_min + egui::vec2(20.0, 12.0);
        let still_inside = card_min + egui::vec2(40.0, 8.0);
        let far_away = egui::pos2(1500.0, 900.0);

        let ctx = egui::Context::default();
        // Alt+hover shows the preview immediately (no 800 ms timer to wait for).
        let alt = egui::Modifiers {
            alt: true,
            ..Default::default()
        };
        let mut frame = pointer_frame(inside, None);
        frame.modifiers = alt;
        let _ = ctx.run_ui(frame, |ui| page.ui_impl(ui));
        assert!(
            page.hover_preview.visible,
            "Alt+hover triggers the preview immediately"
        );
        let first_rect = page.hover_preview.popup_rect.expect("popup rendered");

        // Moving the pointer within the same card must NOT move the popup: it
        // stays anchored to the card instead of sliding after the cursor.
        let mut frame = pointer_frame(still_inside, None);
        frame.modifiers = alt;
        let _ = ctx.run_ui(frame, |ui| page.ui_impl(ui));
        assert!(page.hover_preview.visible, "preview stays visible on the card");
        let second_rect = page.hover_preview.popup_rect.expect("popup rendered");
        assert_eq!(
            first_rect, second_rect,
            "popup is anchored to the card, not the moving pointer"
        );

        // Leaving the card (and the popup) dismisses the preview.
        let _ = ctx.run_ui(pointer_frame(far_away, None), |ui| page.ui_impl(ui));
        assert!(!page.hover_preview.visible, "preview dismissed when pointer leaves");
        assert!(page.hover_preview.target.is_none());
    }

    #[test]
    fn hover_preview_folder_shows_its_members() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        // Create a folder with a snippet member and place its card on root.
        let folder_id = page.workspace.create_container("Folder");
        let (s1, _) = root_first_snippet(&page);
        let _ = page.workspace.add_snippet_reference(&folder_id, s1.clone()).unwrap();
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        page.workspace_store.save(&page.workspace).unwrap();
        page.root = HomePage::load_container_canvas(
            &page.workspace,
            &page.workspace_store,
            &page.all_snippets,
            page.root.container_id.clone(),
        );

        let ctx = egui::Context::default();
        let folder_item = page
            .root
            .items
            .iter()
            .find(|i| matches!(&i.target, ReferenceTarget::Container(id) if id == &folder_id))
            .expect("folder card on root");
        let card_min = egui::pos2(8.0 + folder_item.position[0], 8.0 + folder_item.position[1]);
        let inside = card_min + egui::vec2(20.0, 12.0);
        let alt = egui::Modifiers {
            alt: true,
            ..Default::default()
        };
        let mut frame = pointer_frame(inside, None);
        frame.modifiers = alt;
        let _ = ctx.run_ui(frame, |ui| page.ui_impl(ui));
        assert!(page.hover_preview.visible, "folder preview visible");
        assert!(page.hover_preview.target.is_some());
        let rect = page.hover_preview.popup_rect.expect("popup rendered");
        assert!(rect.width() > 0.0 && rect.height() > 0.0, "popup has size");
        // The preview must show the folder's member card, not "(empty)".
        let cards = {
            let container = page.workspace.containers[&folder_id].clone();
            let layout = page.workspace_store.load_layout(&folder_id).unwrap_or_else(|_| {
                ContainerLayout::empty(folder_id.clone())
            });
            let mut os_file_drop_consumed = false;
            let mut hover_preview = HoverPreview::default();
            let data = canvas::CanvasData::new(
                &mut page.all_snippets,
                &mut page.workspace,
                &page.workspace_store,
                &page.store,
                &mut page.clipboard,
                &page.ai_boxes,
                true,
                true,
                false,
                false,
                &mut os_file_drop_consumed,
                &mut hover_preview,
            );
            let (cards, _) = canvas::container_preview_cards(&ctx, &container, &layout, &data);
            cards
        };
        assert_eq!(cards.len(), 1, "folder preview shows its member card");
        assert_eq!(cards[0].label, "hello");
    }

    #[test]
    fn folder_preview_paints_cards_inside_the_popup() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let (s1, _) = root_first_snippet(&page);
        let _ = page.workspace.add_snippet_reference(&folder_id, s1.clone()).unwrap();
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        page.workspace_store.save(&page.workspace).unwrap();
        page.root = HomePage::load_container_canvas(
            &page.workspace,
            &page.workspace_store,
            &page.all_snippets,
            page.root.container_id.clone(),
        );

        let ctx = egui::Context::default();
        let folder_item = page
            .root
            .items
            .iter()
            .find(|i| matches!(&i.target, ReferenceTarget::Container(id) if id == &folder_id))
            .expect("folder card on root");
        let card_min = egui::pos2(8.0 + folder_item.position[0], 8.0 + folder_item.position[1]);
        let inside = card_min + egui::vec2(20.0, 12.0);
        let alt = egui::Modifiers {
            alt: true,
            ..Default::default()
        };
        let mut frame = pointer_frame(inside, None);
        frame.modifiers = alt;
        let _ = ctx.run_ui(frame, |ui| page.ui_impl(ui));
        assert!(page.hover_preview.visible, "folder preview visible");
        // Let the popup's fade-in animation complete (egui Areas fade in over
        // the first few frames; during the fade its shapes are Noops).
        let mut full = egui::FullOutput::default();
        for _ in 0..12 {
            let mut frame = pointer_frame(inside, None);
            frame.modifiers = alt;
            full = ctx.run_ui(frame, |ui| page.ui_impl(ui));
        }
        let popup_rect = page.hover_preview.popup_rect.expect("popup rendered");

        // The mini-cards must be painted inside the popup. Regression: cards
        // were painted in canvas-local coordinates interpreted as absolute
        // screen coordinates, landing near (0,0) where the popup's clip rect
        // clipped them away entirely (popup frame showed, cards did not).
// The mini-cards must be painted inside the popup. Regression: cards
        // were painted in canvas-local coordinates interpreted as absolute
        // screen coordinates, landing near (0,0) where the popup's clip rect
        // clipped them away entirely (popup frame showed, cards did not).
        let card_shapes: Vec<egui::Rect> = full
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Rect(rs)
                    if (100.0..150.0).contains(&rs.rect.width())
                        && popup_rect.intersects(rs.rect) =>
                {
                    Some(rs.rect)
                }
                _ => None,
            })
            .collect();
        assert!(
            !card_shapes.is_empty(),
            "expected at least one mini-card rectangle painted inside the popup {popup_rect:?}"
        );
        for rect in &card_shapes {
            assert!(
                rect.min.x >= popup_rect.min.x && rect.min.y >= popup_rect.min.y,
                "card {rect:?} must be positioned inside the popup {popup_rect:?}"
            );
        }
    }

    #[test]
    fn folder_preview_cards_match_the_real_canvas() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let (s1, _) = root_first_snippet(&page);
        let s2 = {
            let snippet = Snippet {
                id: EntityId::new(),
                title: "second".to_owned(),
                content: "content".to_owned(),
            };
            page.store.save(&snippet).unwrap();
            page.all_snippets.insert(snippet.id.clone(), snippet.clone());
            snippet.id
        };
        let r1 = page.workspace.add_snippet_reference(&folder_id, s1.clone()).unwrap();
        let _r2 = page.workspace.add_snippet_reference(&folder_id, s2.clone()).unwrap();
        // Only the first member has a saved position; the second must fall back
        // to the same default grid slot the real canvas would pick.
        let mut layout = ContainerLayout::empty(folder_id.clone());
        layout.items.insert(
            r1.clone(),
            CardLayout {
                position: [10.0, 20.0],
                color: None,
            },
        );
        page.workspace_store.save_layout(&layout).unwrap();
        page.workspace_store.save(&page.workspace).unwrap();

        let ctx = egui::Context::default();
        // Initialize the font atlas before measuring text.
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = ui;
        });
        let container = page.workspace.containers[&folder_id].clone();
        let canvas = HomePage::load_container_canvas(
            &page.workspace,
            &page.workspace_store,
            &page.all_snippets,
            folder_id.clone(),
        );
        let expected: Vec<(String, canvas::ItemKind)> = canvas
            .items
            .iter()
            .map(|item| {
                canvas::item_label(
                    &folder_id,
                    item,
                    &page.all_snippets,
                    &page.workspace,
                    &page.ai_boxes,
                )
                .expect("real canvas renders this item")
            })
            .collect();
        let mut os_file_drop_consumed = false;
        let mut hover_preview = HoverPreview::default();
        let cards = {
            let data = canvas::CanvasData::new(
                &mut page.all_snippets,
                &mut page.workspace,
                &page.workspace_store,
                &page.store,
                &mut page.clipboard,
                &page.ai_boxes,
                true,
                true,
                false,
                false,
                &mut os_file_drop_consumed,
                &mut hover_preview,
            );
            let (cards, _) = canvas::container_preview_cards(&ctx, &container, &layout, &data);
            cards
        };

        // Same membership: every card the real canvas renders appears in the
        // preview, at the exact same position and label.
        assert_eq!(cards.len(), canvas.items.len());
        for (card, (item, (label, kind))) in
            cards.iter().zip(canvas.items.iter().zip(expected))
        {
            assert_eq!(card.position, item.position);
            assert_eq!(card.role, item.role);
            assert_eq!(card.label, label);
            assert_eq!(card.kind, kind);
        }
    }

    /// Creates a fresh AI box in the root canvas via the `NewAiBox` command.
    fn create_ai_box_in_root(page: &mut HomePage) -> ContainerId {
        let root = page.root.container_id.clone();
        page.process_canvas_commands(
            vec![CanvasCommand::NewAiBox {
                owner: root,
                position: Some([120.0, 90.0]),
            }],
            egui::ViewportId::ROOT,
        );
        page.workspace
            .containers
            .values()
            .find(|container| container.kind == ContainerKind::AiWorkspace)
            .expect("an AI box exists")
            .id
            .clone()
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
    fn new_ai_box_command_creates_an_ai_workspace_card() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let before = page.root.items.len();
        let ai_box = create_ai_box_in_root(&mut page);

        assert!(page.workspace.is_ai_box(&ai_box));
        assert_eq!(page.root.items.len(), before + 1);
        let item = page
            .root
            .items
            .iter()
            .find(|item| matches!(&item.target, ReferenceTarget::Container(id) if id == &ai_box))
            .expect("the AI box card was placed");
        assert_eq!(item.position, [120.0, 90.0]);

        drop(page);
        let reloaded = HomePage::new(&folder.0);
        assert!(reloaded
            .workspace
            .containers
            .values()
            .any(|container| container.kind == ContainerKind::AiWorkspace));
    }

    #[test]
    fn new_conversation_binds_direct_sources_and_adds_card() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let ai_box = create_ai_box_in_root(&mut page);
        let entity = first_snippet_id(&page);

        page.process_canvas_commands(
            vec![CanvasCommand::LinkAiSource {
                ai_box: ai_box.clone(),
                target: ReferenceTarget::Snippet(entity.clone()),
                position: Some([50.0, 50.0]),
            }],
            egui::ViewportId::ROOT,
        );
        assert!(page.workspace.containers[&ai_box]
            .members
            .iter()
            .any(|reference| reference.role == MemberRole::Source));

        page.process_canvas_commands(
            vec![CanvasCommand::NewConversation {
                ai_box: ai_box.clone(),
                position: Some([80.0, 80.0]),
            }],
            egui::ViewportId::ROOT,
        );

        let data = page.ai_boxes.get(&ai_box).expect("sidecar loaded");
        let conversation = data.conversations.values().next().expect("conversation created");
        assert_eq!(conversation.sources.len(), 1);
        assert!(matches!(conversation.sources[0], SourceTarget::Snippet(_)));
        assert!(page.workspace.containers[&ai_box].members.iter().any(|reference| {
            matches!(&reference.target, ReferenceTarget::Conversation(id) if id == &conversation.id)
                && reference.role == MemberRole::Conversation
        }));

        drop(page);
        let reloaded = HomePage::new(&folder.0);
        assert_eq!(reloaded.ai_boxes[&ai_box].conversations.len(), 1);
    }

    #[test]
    fn remove_ai_source_unlinks_without_deleting_the_entity() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let ai_box = create_ai_box_in_root(&mut page);
        let entity = first_snippet_id(&page);
        page.process_canvas_commands(
            vec![CanvasCommand::LinkAiSource {
                ai_box: ai_box.clone(),
                target: ReferenceTarget::Snippet(entity.clone()),
                position: None,
            }],
            egui::ViewportId::ROOT,
        );
        let reference = page.workspace.containers[&ai_box]
            .members
            .iter()
            .find(|reference| reference.role == MemberRole::Source)
            .expect("a source reference exists")
            .id
            .clone();

        page.process_canvas_commands(
            vec![CanvasCommand::RemoveAiSource {
                ai_box: ai_box.clone(),
                reference: reference.clone(),
            }],
            egui::ViewportId::ROOT,
        );

        assert!(page.workspace.containers[&ai_box]
            .members
            .iter()
            .all(|reference| reference.role != MemberRole::Source));
        assert!(
            page.all_snippets.contains_key(&entity),
            "removing a source never deletes the entity"
        );
        drop(page);
        let reloaded = HomePage::new(&folder.0);
        assert!(reloaded.all_snippets.contains_key(&entity));
    }

    #[test]
    fn deleting_an_ai_box_removes_only_its_sidecar() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let root = page.root.container_id.clone();
        let ai_box = create_ai_box_in_root(&mut page);
        page.process_canvas_commands(
            vec![CanvasCommand::NewConversation {
                ai_box: ai_box.clone(),
                position: None,
            }],
            egui::ViewportId::ROOT,
        );
        assert_eq!(page.ai_boxes[&ai_box].conversations.len(), 1);

        let reference = page.workspace.containers[&root]
            .members
            .iter()
            .find(|reference| {
                matches!(&reference.target, ReferenceTarget::Container(id) if id == &ai_box)
            })
            .expect("root holds the AI box card")
            .id
            .clone();
        page.confirm_delete(PendingDelete {
            owner: root.clone(),
            reference,
            target: ReferenceTarget::Container(ai_box.clone()),
            origin: egui::ViewportId::ROOT,
        });

        assert!(!page.workspace.containers.contains_key(&ai_box));
        assert!(!page.ai_boxes.contains_key(&ai_box));
        drop(page);
        let reloaded = HomePage::new(&folder.0);
        assert!(!reloaded.ai_boxes.contains_key(&ai_box));
    }

    fn external_file(path: &str) -> ExternalFileRef {
        ExternalFileRef {
            id: ExternalFileId::new(),
            path: path.to_owned(),
            title: file_stem(path),
        }
    }

    #[test]
    fn insert_external_file_command_creates_reference_and_card() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let file = external_file("/tmp/notes.pdf");
        page.process_canvas_commands(
            vec![CanvasCommand::InsertExternalFile {
                container: page.root.container_id.clone(),
                file: file.clone(),
                position: Some([64.0, 64.0]),
            }],
            egui::ViewportId::ROOT,
        );
        let target = ReferenceTarget::ExternalFile(file);
        let card = page
            .root
            .items
            .iter()
            .find(|item| item.target == target)
            .expect("external file card was placed");
        assert_eq!(card.position, [64.0, 64.0]);
        assert_eq!(card.role, MemberRole::Normal);
        assert!(page
            .workspace
            .root()
            .members
            .iter()
            .any(|reference| reference.target == target));
        // The card and its reference survive a restart.
        let reloaded = HomePage::new(&folder.0);
        assert!(reloaded
            .root
            .items
            .iter()
            .any(|item| item.target == target));
    }

    #[test]
    fn external_file_delete_removes_card_without_confirmation() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let file = external_file("/tmp/spec.pdf");
        page.insert_external_file(
            &page.root.container_id.clone(),
            file.clone(),
            Some([32.0, 32.0]),
        );
        let reference = page
            .root
            .items
            .iter()
            .find(|item| item.target == ReferenceTarget::ExternalFile(file.clone()))
            .map(|item| item.reference_id.clone())
            .expect("external file card exists");
        page.process_canvas_commands(
            vec![CanvasCommand::DeleteReference {
                owner: page.root.container_id.clone(),
                reference: reference.clone(),
                target: ReferenceTarget::ExternalFile(file),
            }],
            egui::ViewportId::ROOT,
        );
        assert!(
            page.pending_delete.is_none(),
            "external file cards are removed without a confirmation dialog"
        );
        assert!(
            page.root
                .items
                .iter()
                .all(|item| item.reference_id != reference),
            "the card is removed"
        );
        assert!(page
            .workspace
            .root()
            .members
            .iter()
            .all(|member| !matches!(member.target, ReferenceTarget::ExternalFile(_))));
    }

    #[test]
    fn rename_external_file_updates_card_titles_not_the_file() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let file = external_file("/tmp/report.md");
        page.insert_external_file(
            &page.root.container_id.clone(),
            file.clone(),
            Some([32.0, 32.0]),
        );
        let id = file.id.clone();
        // Open the rename dialog via the card's command, then confirm.
        page.process_canvas_commands(
            vec![CanvasCommand::RenameExternalFile(id.clone())],
            egui::ViewportId::ROOT,
        );
        assert!(matches!(
            page.rename_dialog.pending,
            Some(RenameTarget::ExternalFile { .. })
        ));
        page.rename_dialog.buffer = "Architecture Notes".to_owned();
        page.process_canvas_commands(
            vec![CanvasCommand::ApplyRename(RenameTarget::ExternalFile {
                id: id.clone(),
                origin: egui::ViewportId::ROOT,
            })],
            egui::ViewportId::ROOT,
        );
        assert!(page.rename_dialog.pending.is_none());
        let target = ReferenceTarget::ExternalFile(ExternalFileRef {
            id,
            path: "/tmp/report.md".to_owned(),
            title: "Architecture Notes".to_owned(),
        });
        assert!(page
            .root
            .items
            .iter()
            .any(|item| item.target == target));
        assert!(page
            .workspace
            .root()
            .members
            .iter()
            .any(|reference| reference.target == target));
        // Only the recorded title changed: the path (and thus the on-disk
        // file) is untouched, and the rename survives a restart.
        let reloaded = HomePage::new(&folder.0);
        assert!(reloaded
            .root
            .items
            .iter()
            .any(|item| item.target == target));
    }

    #[test]
    fn external_files_can_be_pasted_into_ordinary_containers() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let file = external_file("/tmp/guide.pdf");
        let entry = ClipboardEntry {
            source_container: page.root.container_id.clone(),
            reference_id: ReferenceId::new(),
            target: ReferenceTarget::ExternalFile(file),
            semantics: ClipboardSemantics::Link,
            origin: egui::ViewportId::ROOT,
        };
        assert!(page.paste_clipboard(&folder_id, &entry));
        assert_eq!(page.workspace.containers[&folder_id].members.len(), 1);
    }

    #[test]
    fn external_files_can_enter_ai_boxes() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let ai_box = page.workspace.create_ai_box("AI Box");
        let file = external_file("/tmp/data.pdf");
        let entry = ClipboardEntry {
            source_container: page.root.container_id.clone(),
            reference_id: ReferenceId::new(),
            target: ReferenceTarget::ExternalFile(file),
            semantics: ClipboardSemantics::Link,
            origin: egui::ViewportId::ROOT,
        };
        assert!(
            page.paste_clipboard(&ai_box, &entry),
            "external files can now be pasted into AI boxes as model-readable sources"
        );
        let payload = DragPayload::Reference {
            source_container: page.root.container_id.clone(),
            reference_id: ReferenceId::new(),
            target: ReferenceTarget::ExternalFile(external_file("/tmp/data.pdf")),
        };
        assert!(
            page.apply_drop(&ai_box, None, &payload, false),
            "external files can now be dropped into AI boxes"
        );
        assert_eq!(page.workspace.containers[&ai_box].members.len(), 2);
    }

    #[test]
    fn external_file_shows_as_source_in_new_conversation() {
        // Create a real file on disk so resolve_source_text can read it.
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let ai_box = create_ai_box_in_root(&mut page);
        let file_path = folder.0.join("test-source.md");
        std::fs::write(&file_path, "Hello from external file").unwrap();

        let file = ExternalFileRef {
            id: ExternalFileId::new(),
            title: "Test Source".to_owned(),
            path: file_path.to_str().unwrap().to_owned(),
        };
        // Link the external file as a Source into the AI box.
        page.process_canvas_commands(
            vec![CanvasCommand::LinkAiSource {
                ai_box: ai_box.clone(),
                target: ReferenceTarget::ExternalFile(file),
                position: Some([50.0, 50.0]),
            }],
            egui::ViewportId::ROOT,
        );
        assert!(page.workspace.containers[&ai_box]
            .members
            .iter()
            .any(|reference| reference.role == MemberRole::Source
                && matches!(&reference.target, ReferenceTarget::ExternalFile(_))));

        // Create a new conversation — it should pick up the ExternalFile source.
        page.process_canvas_commands(
            vec![CanvasCommand::NewConversation {
                ai_box: ai_box.clone(),
                position: Some([80.0, 80.0]),
            }],
            egui::ViewportId::ROOT,
        );
        let data = page.ai_boxes.get(&ai_box).expect("sidecar loaded");
        let conversation = data.conversations.values().next().expect("conversation created");
        assert_eq!(conversation.sources.len(), 1);
        assert!(matches!(conversation.sources[0], SourceTarget::ExternalFile(_)));
    }

    #[test]
    fn file_stem_derives_display_title() {
        assert_eq!(file_stem("/home/user/docs/notes.pdf"), "notes");
        assert_eq!(file_stem("report.md"), "report");
        assert_eq!(file_stem("no-extension"), "no-extension");
    }

    #[test]
    fn dropping_files_from_the_file_manager_inserts_external_file_cards() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let ctx = egui::Context::default();
        // Settle the root canvas layout, then drop files over the canvas.
        for _ in 0..2 {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| page.ui_impl(ui));
        }
        let drop_pos = egui::pos2(320.0, 240.0);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events: vec![egui::Event::PointerMoved(drop_pos)],
            dropped_files: vec![
                egui::DroppedFile {
                    path: Some(std::path::PathBuf::from("/tmp/manual.pdf")),
                    ..Default::default()
                },
                egui::DroppedFile {
                    path: Some(std::path::PathBuf::from("/tmp/readme.md")),
                    ..Default::default()
                },
                // A pathless drop (e.g. pasted content) is ignored.
                egui::DroppedFile {
                    name: "no-path.bin".to_owned(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| page.ui_impl(ui));
        let cards: Vec<(ExternalFileRef, [f32; 2])> = page
            .root
            .items
            .iter()
            .filter_map(|item| match &item.target {
                ReferenceTarget::ExternalFile(file) => Some((file.clone(), item.position)),
                _ => None,
            })
            .collect();
        assert_eq!(
            cards.len(),
            2,
            "path-backed drops become external-file cards, pathless ones are skipped"
        );
        assert!(cards.iter().any(|(file, _)| {
            file.path == "/tmp/manual.pdf" && file.title == "manual"
        }));
        assert!(cards.iter().any(|(file, _)| {
            file.path == "/tmp/readme.md" && file.title == "readme"
        }));
        assert_ne!(
            cards[0].1, cards[1].1,
            "additional files cascade below the previous one"
        );
        // The workspace reference (and thus persistence) is created too.
        assert!(page.workspace.root().members.iter().any(|reference| {
            matches!(&reference.target, ReferenceTarget::ExternalFile(_))
        }));
    }

    #[test]
    fn native_drop_without_a_live_pointer_is_still_consumed_once() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let ctx = egui::Context::default();
        for _ in 0..2 {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| page.ui_impl(ui));
        }
        // During a real X11 file drag the window receives no pointer motion (the
        // source grabs the pointer), so the pointer position can be stale or
        // absent. In native mode each viewport holds exactly one canvas, so the
        // drop must still be consumed — landing on a free default slot — rather
        // than being silently lost. Exactly one card is created (the per-frame
        // guard prevents double insertion).
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            // Pointer far outside the canvas (as if no motion was received).
            events: vec![egui::Event::PointerMoved(egui::pos2(7000.0, 7000.0))],
            dropped_files: vec![egui::DroppedFile {
                path: Some(std::path::PathBuf::from("/tmp/x.pdf")),
                ..Default::default()
            }],
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| page.ui_impl(ui));
        let cards: Vec<_> = page
            .root
            .items
            .iter()
            .filter(|item| matches!(item.target, ReferenceTarget::ExternalFile(_)))
            .collect();
        assert_eq!(
            cards.len(),
            1,
            "the native viewport's single canvas consumes the drop exactly once"
        );
    }
}
