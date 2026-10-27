//! The message box under the conversation, and the requests waiting above it (approvals,
//! questions, the plan, queued messages, the task list).

mod cards;
mod logic;
mod popovers;
mod widgets;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::Engine as _;
use claude_chat::artifacts::{ArtifactKind, kind_label_pt};
use claude_chat::protocol::OutgoingImage;
use claude_chat::reducer::{ConversationChange, ConversationStatus};
use claude_chat::timeline::{PendingRequest, PendingRequestKind, TimelineItem};
use instant::Instant;
use pathfinder_color::ColorU;
use warp_core::ui::Icon as WarpIcon;
use warp_core::ui::theme::Fill as ThemeFill;
use warp_editor::editor::NavigationKey;
use warpui::assets::asset_cache::{AssetCache, AssetSource};
use warpui::r#async::Timer;
use warpui::clipboard_utils::CLIPBOARD_IMAGE_MIME_TYPES;
use warpui::elements::{
    Border, ChildAnchor, ChildView, Clipped, ClippedScrollStateHandle, ConstrainedBox, Container,
    CrossAxisAlignment, DropShadow, Element, Flex, Hoverable, Image, MainAxisAlignment,
    MainAxisSize, MouseStateHandle, OffsetPositioning, ParentElement, PositionedElementAnchor,
    PositionedElementOffsetBounds, SavePosition, Stack,
};
use warpui::geometry::vector::vec2f;
use warpui::image_cache::{CacheOption, ImageType};
use warpui::keymap::FixedBinding;
use warpui::platform::{Cursor, FilePickerConfiguration, FileType};
use warpui::{
    AppContext, Entity, FocusContext, ModelHandle, SingletonEntity, TypedActionView, View,
    ViewContext, ViewHandle, WindowId,
};

use self::logic::{CommandEntry, UiCommand};
use self::widgets::{Look, icon, with_alpha};
use super::SidePanelSection;
use crate::editor::{
    CursorColors, EditorOptions, EditorView, Event as EditorEvent, PlainTextEditorViewAction,
    PropagateAndNoOpNavigationKeys, TextColors, TextOptions,
};
use crate::native_chat::design::{self, ChatDesign};
use crate::native_chat::session::{
    ClaudeChatSession, ClaudeChatSessionEvent, PermissionAnswer, PlanAnswer, ProcessState,
};

const MENTION_CHIP_HEIGHT: f32 = 24.;
/// Lines the mention pills up with the text inside the box.
const MENTION_CHIPS_INSET: f32 = 14.;
/// Space between the mention pills and the box's top edge.
const MENTION_CHIPS_OFFSET: f32 = 6.;
const PLACEHOLDER: &str = "Digite / para comandos";
const PLACEHOLDER_PERMISSION: &str = "Digite para negar e dizer ao Claude o que fazer";
const PLACEHOLDER_QUESTION: &str =
    "Escreva outra resposta, ou deixe vazio para usar a opção marcada";
const PLACEHOLDER_PLAN: &str = "Comente para continuar planejando, ou deixe vazio para aprovar";
const PLACEHOLDER_WORKING: &str = "Digite para enfileirar uma mensagem";

/// The keymap flag on the composer's editor, so Shift+Tab cycles the permission mode instead of
/// unindenting.
const EDITOR_FLAG: &str = "NativeChatComposerEditor";
/// Set while a question waits and the editor is empty, so digits pick its options.
const PICK_DIGITS_FLAG: &str = "NativeChatPickDigits";

/// How soon a second Esc must follow the first to interrupt Claude.
const DOUBLE_ESCAPE_WINDOW: Duration = Duration::from_millis(1200);
#[cfg(feature = "voice_input")]
const VOICE_DISABLED_MESSAGE: &str =
    "Ative a entrada por voz em Configurações > Aparência > Chat do Claude para ditar mensagens.";
#[cfg(feature = "voice_input")]
const VOICE_LOGIN_MESSAGE: &str =
    "O ditado usa a transcrição do Warp: entre na sua conta Warp para usar o microfone.";
#[cfg(feature = "voice_input")]
const VOICE_NOT_ALLOWED_MESSAGE: &str = "A sua conta Warp não permite entrada por voz.";
const POPOVER_REOPEN_GUARD: Duration = Duration::from_millis(300);
const SLIDER_SETTLE_DURATION: Duration = Duration::from_millis(160);
const SLIDER_FRAME: Duration = Duration::from_millis(16);
/// Rows of the "/" and "@" menus shown at once.
const MENU_VISIBLE_ROWS: usize = 8;
/// Upper bounds of the lists that keep a mouse state per row.
const MAX_MENU_ROWS: usize = MENU_VISIBLE_ROWS;
const MAX_OPTIONS: usize = 40;
const MAX_MODELS: usize = 32;
const MAX_EFFORT_LEVELS: usize = 8;
const MAX_CHIPS: usize = 16;
const MENTION_PREVIEW_CHARS: usize = 36;
const QUEUED_PREVIEW_CHARS: usize = 160;
const QUEUED_ALREADY_SENT: &str = "O Claude já começou a ler essa mensagem.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerEvent {
    OpenSidePanel(SidePanelSection),
    NewChat,
    /// Renames the conversation to this title.
    Rename(String),
}

/// What the effort slider and the permission card's buttons do, among others.
#[derive(Debug, Clone, PartialEq)]
pub enum ComposerAction {
    Submit,
    Interrupt,
    AttachImage,
    RemoveImage(usize),
    RemoveMention(usize),
    ToggleModeMenu,
    SetMode(usize),
    CycleMode,
    TogglePicker,
    ShowModels,
    ChooseModel(usize),
    /// The effort level at this index of the model's levels was picked on the slider.
    SetEffort(usize),
    /// Starts or stops dictation.
    ToggleVoice,
    ToggleArtifactKindMenu,
    RemoveArtifactKind,
    ResetEffort,
    ClosePopovers,
    OpenContext,
    /// A row of the "/" or "@" menu was clicked.
    AcceptMenuRow(usize),
    Permission(PermissionChoice),
    ToggleOption {
        question: usize,
        option: usize,
    },
    /// A digit key picked this option of the active question.
    PickDigit(usize),
    SubmitAnswers,
    DismissQuestion,
    Plan(PlanChoice),
    ToggleTodos,
    Restart,
    SendQueuedNow(String),
    EditQueued(String),
    DeleteQueued(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionChoice {
    Allow,
    AllowAlways,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanChoice {
    KeepPlanning,
    Approve,
    ApproveAcceptEdits,
}

/// The two pages of the model and effort picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerPage {
    Effort,
    Models,
}

/// The popup over the composer while a "/" command or an "@" mention is typed.
#[derive(Debug, Clone, PartialEq)]
enum Menu {
    Commands {
        entries: Vec<CommandEntry>,
        matches: Vec<usize>,
        selected: usize,
    },
    Files {
        /// Byte offset of the `@` in the editor's text.
        at: usize,
        query: String,
        paths: Vec<String>,
        /// Whether `paths` answer an older query.
        is_loading: bool,
        selected: usize,
    },
}

impl Menu {
    fn len(&self) -> usize {
        match self {
            Menu::Commands { matches, .. } => matches.len(),
            Menu::Files { paths, .. } => paths.len(),
        }
    }

    fn selected(&self) -> usize {
        match self {
            Menu::Commands { selected, .. } | Menu::Files { selected, .. } => *selected,
        }
    }

    fn select(&mut self, index: usize) {
        let len = self.len();
        match self {
            Menu::Commands { selected, .. } | Menu::Files { selected, .. } => {
                *selected = index.min(len.saturating_sub(1));
            }
        }
    }
}

struct Mention {
    text: String,
    remove: MouseStateHandle,
}

/// A message sent while Claude was busy, until it gets to it.
struct QueuedMessage {
    uuid: String,
    text: String,
    preview: String,
    images: Vec<Attachment>,
    artifact_kind: Option<ArtifactKind>,
    /// What the user asked for while the CLI is answering whether it could still take it back.
    withdrawing: Option<Withdrawal>,
    /// The user asked Claude to stop and read it now.
    sending_now: bool,
    row: MouseStateHandle,
    send_now: MouseStateHandle,
    edit: MouseStateHandle,
    delete: MouseStateHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Withdrawal {
    Edit,
    Delete,
}

struct Attachment {
    image: OutgoingImage,
    asset_id: String,
    remove: MouseStateHandle,
}

/// The options picked in the question card on top.
#[derive(Debug, Default)]
struct QuestionState {
    request_id: String,
    selected: Vec<Vec<usize>>,
    /// The question digits and typed text answer.
    active: usize,
}

/// The knob of the effort slider gliding from where it was let go to its level.
#[derive(Debug, Clone, Copy)]
struct SliderSettle {
    from: f32,
    to: f32,
    started_at: Instant,
}

impl SliderSettle {
    fn position(&self) -> Option<f32> {
        let progress =
            self.started_at.elapsed().as_secs_f32() / SLIDER_SETTLE_DURATION.as_secs_f32();
        (progress < 1.).then(|| {
            let eased = 1. - (1. - progress).powi(3);
            self.from + (self.to - self.from) * eased
        })
    }
}

/// The dictation state the editor last reported.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct VoiceState {
    is_listening: bool,
    is_transcribing: bool,
}

fn handles(count: usize) -> Vec<MouseStateHandle> {
    (0..count).map(|_| MouseStateHandle::default()).collect()
}

/// Mouse states created once, so hovering works across renders.
struct MouseStates {
    attach: MouseStateHandle,
    mic: MouseStateHandle,
    artifact_kind: MouseStateHandle,
    remove_artifact_kind: MouseStateHandle,
    send: MouseStateHandle,
    mode: MouseStateHandle,
    model: MouseStateHandle,
    context: MouseStateHandle,
    model_link: MouseStateHandle,
    reset_effort: MouseStateHandle,
    effort_levels: Vec<MouseStateHandle>,
    models: Vec<MouseStateHandle>,
    modes: Vec<MouseStateHandle>,
    menu_rows: Vec<MouseStateHandle>,
    options: Vec<MouseStateHandle>,
    allow: MouseStateHandle,
    allow_always: MouseStateHandle,
    deny: MouseStateHandle,
    submit_answers: MouseStateHandle,
    dismiss_question: MouseStateHandle,
    keep_planning: MouseStateHandle,
    approve: MouseStateHandle,
    approve_accept_edits: MouseStateHandle,
    todos: MouseStateHandle,
    restart: MouseStateHandle,
}

impl Default for MouseStates {
    fn default() -> Self {
        Self {
            attach: Default::default(),
            mic: Default::default(),
            artifact_kind: Default::default(),
            remove_artifact_kind: Default::default(),
            send: Default::default(),
            mode: Default::default(),
            model: Default::default(),
            context: Default::default(),
            model_link: Default::default(),
            reset_effort: Default::default(),
            effort_levels: handles(MAX_EFFORT_LEVELS),
            models: handles(MAX_MODELS),
            modes: handles(logic::PERMISSION_MODES.len()),
            menu_rows: handles(MAX_MENU_ROWS),
            options: handles(MAX_OPTIONS),
            allow: Default::default(),
            allow_always: Default::default(),
            deny: Default::default(),
            submit_answers: Default::default(),
            dismiss_question: Default::default(),
            keep_planning: Default::default(),
            approve: Default::default(),
            approve_accept_edits: Default::default(),
            todos: Default::default(),
            restart: Default::default(),
        }
    }
}

pub struct ComposerView {
    session: ModelHandle<ClaudeChatSession>,
    editor: ViewHandle<EditorView>,
    window_id: WindowId,
    /// Makes this composer's save-position ids unique.
    position_prefix: String,
    placeholder: &'static str,
    is_editor_empty: bool,
    mentions: Vec<Mention>,
    images: Vec<Attachment>,
    next_image_id: usize,
    menu: Option<Menu>,
    /// The text the menu was closed with Esc for; it stays closed until the text changes.
    menu_dismissed_for: Option<String>,
    picker: Option<PickerPage>,
    is_mode_menu_open: bool,
    /// When a click outside closed the popovers; the same click on their chip mustn't reopen them.
    popovers_closed_at: Option<Instant>,
    slider_settle: Option<SliderSettle>,
    /// The kind of artifact the next message asks Claude to build, from a gallery tile.
    artifact_kind: Option<ArtifactKind>,
    is_artifact_kind_menu_open: bool,
    voice: VoiceState,
    queued: Vec<QueuedMessage>,
    last_escape: Option<Instant>,
    question: QuestionState,
    is_todos_expanded: bool,
    plan_scroll: ClippedScrollStateHandle,
    models_scroll: ClippedScrollStateHandle,
    queue_scroll: ClippedScrollStateHandle,
    mouse: MouseStates,
}

/// Registers the composer's key bindings the first time one is created, after the editor's own,
/// so they take precedence.
fn register_bindings(app: &mut AppContext) {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.swap(true, Ordering::Relaxed) {
        return;
    }
    use warpui::keymap::macros::*;

    app.register_fixed_bindings([FixedBinding::new(
        "shift-tab",
        ComposerAction::CycleMode,
        id!(EDITOR_FLAG),
    )]);
    for digit in 1..=9usize {
        app.register_fixed_bindings([FixedBinding::new(
            format!("{digit}"),
            ComposerAction::PickDigit(digit - 1),
            id!(ComposerView::ui_name()) & id!(PICK_DIGITS_FLAG),
        )]);
    }
}

impl ComposerView {
    pub fn new(
        session: ModelHandle<ClaudeChatSession>,
        draft: Option<String>,
        artifact_kind: Option<String>,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        register_bindings(ctx);
        let font_size = ChatDesign::as_ref(ctx).sizes().font_size;
        let mut text =
            TextOptions::ui_text(Some(font_size), crate::appearance::Appearance::as_ref(ctx));
        text.font_family_override = Some(design::fonts(ctx).ui);
        text.text_colors_override = Some(editor_text_colors(ctx));
        let editor = ctx.add_view(|ctx| {
            let options = EditorOptions {
                autogrow: true,
                soft_wrap: true,
                text,
                delegate_paste_handling: true,
                propagate_and_no_op_vertical_navigation_keys:
                    PropagateAndNoOpNavigationKeys::AtBoundary,
                cursor_colors_fn: Box::new(|app| {
                    let palette = ChatDesign::as_ref(app).palette(app);
                    CursorColors {
                        cursor: ThemeFill::Solid(palette.accent.0),
                        selection: ThemeFill::Solid(palette.selection.0),
                    }
                }),
                keymap_context_modifier: Some(Box::new(|context, _| {
                    context.set.insert(EDITOR_FLAG);
                })),
                ..Default::default()
            };
            let mut editor = EditorView::new(options, ctx);
            editor.set_placeholder_text(PLACEHOLDER, ctx);
            if let Some(draft) = &draft {
                editor.system_reset_buffer_text(draft, ctx);
            }
            editor
        });
        ctx.subscribe_to_view(&editor, |me, _, event, ctx| me.on_editor_event(event, ctx));
        ctx.subscribe_to_model(&session, |me, _, event, ctx| {
            me.on_session_event(event, ctx)
        });
        ctx.subscribe_to_model(&ChatDesign::handle(ctx), |me, _, _, ctx| {
            let colors = editor_text_colors(ctx);
            me.editor
                .update(ctx, |editor, ctx| editor.set_text_colors(colors, ctx));
            ctx.notify();
        });

        let mut me = Self {
            session,
            editor,
            window_id: ctx.window_id(),
            position_prefix: format!("native_chat_composer_{}", ctx.view_id()),
            placeholder: PLACEHOLDER,
            is_editor_empty: draft.as_deref().is_none_or(str::is_empty),
            mentions: Vec::new(),
            images: Vec::new(),
            next_image_id: 0,
            menu: None,
            menu_dismissed_for: None,
            picker: None,
            is_mode_menu_open: false,
            popovers_closed_at: None,
            slider_settle: None,
            artifact_kind: artifact_kind.as_deref().and_then(ArtifactKind::parse),
            is_artifact_kind_menu_open: false,
            voice: VoiceState::default(),
            queued: Vec::new(),
            last_escape: None,
            question: QuestionState::default(),
            is_todos_expanded: false,
            plan_scroll: Default::default(),
            models_scroll: Default::default(),
            queue_scroll: Default::default(),
            mouse: MouseStates::default(),
        };
        me.refresh_menu(ctx);
        me.update_placeholder(ctx);
        me.send_dev_queue(ctx);
        me
    }

    /// `PICOLAS_CHAT_QUEUE=<text>[|<text>…]` sends those messages a few seconds after the chat
    /// opens, while `PICOLAS_CHAT_SEND`'s message keeps Claude busy, to show the queue. Debug
    /// builds only.
    fn send_dev_queue(&self, ctx: &mut ViewContext<Self>) {
        if !cfg!(debug_assertions) {
            return;
        }
        let Ok(texts) = std::env::var("PICOLAS_CHAT_QUEUE") else {
            return;
        };
        ctx.spawn(Timer::after(Duration::from_secs(6)), move |me, _, ctx| {
            for text in texts.split('|').filter(|text| !text.trim().is_empty()) {
                me.send(text.trim().to_owned(), ctx);
            }
        });
    }

    pub fn focus(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.focus(&self.editor);
    }

    /// Quotes `text` in the next message, shown as a chip until then.
    pub fn add_mention(&mut self, text: String, ctx: &mut ViewContext<Self>) {
        if text.trim().is_empty() || self.mentions.len() >= MAX_CHIPS {
            return;
        }
        self.mentions.push(Mention {
            text,
            remove: Default::default(),
        });
        self.focus(ctx);
        ctx.notify();
    }

    fn position_id(&self, name: &str) -> String {
        format!("{}_{name}", self.position_prefix)
    }

    fn buffer_text(&self, app: &AppContext) -> String {
        self.editor.as_ref(app).buffer_text(app)
    }

    fn set_text(&mut self, text: &str, ctx: &mut ViewContext<Self>) {
        self.editor
            .update(ctx, |editor, ctx| editor.set_buffer_text(text, ctx));
    }

    fn clear_text(&mut self, ctx: &mut ViewContext<Self>) {
        self.editor
            .update(ctx, |editor, ctx| editor.clear_buffer(ctx));
    }

    fn status(&self, app: &AppContext) -> ConversationStatus {
        self.session.as_ref(app).conversation().status()
    }

    fn is_working(&self, app: &AppContext) -> bool {
        matches!(self.status(app), ConversationStatus::Working { .. })
    }

    /// The request the cards above the composer answer first.
    fn first_pending(&self, app: &AppContext) -> Option<PendingRequest> {
        self.session
            .as_ref(app)
            .conversation()
            .pending_requests()
            .first()
            .cloned()
    }

    fn wants_digits(&self, app: &AppContext) -> bool {
        self.menu.is_none()
            && self.is_editor_empty
            && self
                .session
                .as_ref(app)
                .conversation()
                .pending_requests()
                .first()
                .is_some_and(|request| matches!(request.kind, PendingRequestKind::Question { .. }))
    }

    // --- events -----------------------------------------------------------------------------

    fn on_editor_event(&mut self, event: &EditorEvent, ctx: &mut ViewContext<Self>) {
        match event {
            EditorEvent::Enter => {
                if self.menu.is_some() {
                    self.accept_menu(false, ctx);
                } else {
                    self.submit(ctx);
                }
            }
            EditorEvent::Escape => self.on_escape(ctx),
            EditorEvent::Navigate(key) => self.on_navigate(*key, ctx),
            EditorEvent::Edited(_) | EditorEvent::BufferReplaced => {
                self.is_editor_empty = self.editor.as_ref(ctx).is_empty(ctx);
                self.refresh_menu(ctx);
                ctx.notify();
            }
            EditorEvent::Paste => self.on_paste(ctx),
            EditorEvent::VoiceStateUpdated {
                is_listening,
                is_transcribing,
            } => {
                self.voice = VoiceState {
                    is_listening: *is_listening,
                    is_transcribing: *is_transcribing,
                };
                ctx.notify();
            }
            EditorEvent::DroppedImageFiles(paths) => self.load_image_files(paths.clone(), ctx),
            EditorEvent::Focused => {
                self.picker = None;
                self.is_mode_menu_open = false;
                ctx.notify();
            }
            _ => {}
        }
    }

    fn on_session_event(&mut self, event: &ClaudeChatSessionEvent, ctx: &mut ViewContext<Self>) {
        match event {
            ClaudeChatSessionEvent::Changed(changes) => {
                let queued = self.queued.len();
                self.sync_queue(changes, ctx);
                if self.queued.len() != queued || affects_composer(changes) {
                    self.sync_question(ctx);
                    self.update_placeholder(ctx);
                    ctx.notify();
                }
            }
            ClaudeChatSessionEvent::FileSuggestions { query, paths } => {
                if let Some(Menu::Files {
                    query: current,
                    paths: current_paths,
                    is_loading,
                    selected,
                    ..
                }) = &mut self.menu
                    && current == query
                {
                    *current_paths = paths.clone();
                    *is_loading = false;
                    *selected = 0;
                    ctx.notify();
                }
            }
            ClaudeChatSessionEvent::ProcessStateChanged | ClaudeChatSessionEvent::HistoryLoaded => {
                self.update_placeholder(ctx);
                ctx.notify();
            }
            ClaudeChatSessionEvent::QueuedMessageCancelled { uuid, cancelled } => {
                self.on_queued_message_cancelled(uuid, *cancelled, ctx);
            }
            ClaudeChatSessionEvent::TitleChanged
            | ClaudeChatSessionEvent::SessionIdAssigned
            | ClaudeChatSessionEvent::Error(_) => {}
        }
    }

    /// Drops the queued messages Claude has taken in (or the CLI dropped).
    fn sync_queue(&mut self, changes: &[ConversationChange], ctx: &mut ViewContext<Self>) {
        let conversation = self.session.as_ref(ctx).conversation();
        if conversation.tracks_queue() {
            // A withdrawal resolves in `on_queued_message_cancelled`, which needs the message.
            self.queued.retain(|message| {
                message.withdrawing.is_some() || conversation.is_queued(&message.uuid)
            });
            return;
        }
        // Without lifecycle frames, assume the CLI runs queued messages one turn at a time.
        for change in changes {
            if let ConversationChange::ItemInserted { id, .. } = change
                && matches!(conversation.item(id), Some(TimelineItem::TurnEnd(_)))
                && !self.queued.is_empty()
            {
                self.queued.remove(0);
            }
        }
        if !matches!(
            conversation.status(),
            ConversationStatus::Working { .. } | ConversationStatus::AwaitingUser
        ) {
            self.queued.clear();
        }
    }

    /// Resets the picked options when another question comes up.
    fn sync_question(&mut self, ctx: &mut ViewContext<Self>) {
        match self.first_pending(ctx) {
            Some(PendingRequest {
                request_id,
                kind: PendingRequestKind::Question { questions, .. },
                ..
            }) => {
                if self.question.request_id != request_id {
                    self.question = QuestionState {
                        request_id,
                        selected: vec![Vec::new(); questions.len()],
                        active: 0,
                    };
                }
            }
            Some(_) | None => self.question = QuestionState::default(),
        }
    }

    fn update_placeholder(&mut self, ctx: &mut ViewContext<Self>) {
        let placeholder = match self.first_pending(ctx).map(|request| request.kind) {
            Some(PendingRequestKind::Permission { .. }) => PLACEHOLDER_PERMISSION,
            Some(PendingRequestKind::Question { .. }) => PLACEHOLDER_QUESTION,
            Some(PendingRequestKind::PlanApproval { .. }) => PLACEHOLDER_PLAN,
            None if self.is_working(ctx) => PLACEHOLDER_WORKING,
            None => self
                .artifact_kind
                .map_or(PLACEHOLDER, logic::artifact_placeholder),
        };
        if placeholder != self.placeholder {
            self.placeholder = placeholder;
            self.editor.update(ctx, |editor, ctx| {
                editor.set_placeholder_text(placeholder, ctx)
            });
        }
    }

    fn on_escape(&mut self, ctx: &mut ViewContext<Self>) {
        if self.menu.is_some() {
            self.menu = None;
            self.menu_dismissed_for = Some(self.buffer_text(ctx));
            self.set_menu_navigation(ctx);
            ctx.notify();
            return;
        }
        if self.picker.is_some() || self.is_mode_menu_open {
            self.close_popovers(ctx);
            return;
        }
        if self.is_editor_empty
            && let Some(PendingRequest {
                request_id,
                kind: PendingRequestKind::Permission { .. },
                ..
            }) = self.first_pending(ctx)
        {
            self.session.update(ctx, |session, ctx| {
                session.answer_permission(
                    &request_id,
                    PermissionAnswer::Deny { message: None },
                    ctx,
                )
            });
            return;
        }
        if matches!(
            self.status(ctx),
            ConversationStatus::Working { .. } | ConversationStatus::AwaitingUser
        ) {
            let is_second = self
                .last_escape
                .is_some_and(|at| at.elapsed() < DOUBLE_ESCAPE_WINDOW);
            if is_second {
                self.last_escape = None;
                self.session
                    .update(ctx, |session, ctx| session.interrupt(ctx));
            } else {
                self.last_escape = Some(Instant::now());
                ctx.spawn(Timer::after(DOUBLE_ESCAPE_WINDOW), |me, _, ctx| {
                    if me
                        .last_escape
                        .is_some_and(|at| at.elapsed() >= DOUBLE_ESCAPE_WINDOW)
                    {
                        me.last_escape = None;
                        ctx.notify();
                    }
                });
            }
            ctx.notify();
        }
    }

    fn on_navigate(&mut self, key: NavigationKey, ctx: &mut ViewContext<Self>) {
        let Some(menu) = &mut self.menu else {
            return;
        };
        let len = menu.len();
        if len == 0 {
            return;
        }
        match key {
            NavigationKey::Up => menu.select((menu.selected() + len - 1) % len),
            NavigationKey::Down => menu.select((menu.selected() + 1) % len),
            NavigationKey::Tab => {
                self.accept_menu(true, ctx);
                return;
            }
            NavigationKey::ShiftTab
            | NavigationKey::PageUp
            | NavigationKey::PageDown
            | NavigationKey::Left
            | NavigationKey::Right => return,
        }
        ctx.notify();
    }

    // --- "/" and "@" menus --------------------------------------------------------------------

    fn refresh_menu(&mut self, ctx: &mut ViewContext<Self>) {
        let text = self.buffer_text(ctx);
        if self
            .menu_dismissed_for
            .as_ref()
            .is_some_and(|dismissed| *dismissed == text)
        {
            return;
        }
        self.menu_dismissed_for = None;
        let menu = if let Some(query) = logic::slash_query(&text) {
            let entries =
                logic::command_entries(self.session.as_ref(ctx).conversation().slash_commands());
            let matches = logic::filter_commands(&entries, query);
            let selected = match &self.menu {
                Some(Menu::Commands {
                    matches: old,
                    selected,
                    ..
                }) if *old == matches => *selected,
                _ => 0,
            };
            (!matches.is_empty()).then_some(Menu::Commands {
                entries,
                matches,
                selected,
            })
        } else if let Some((at, query)) = logic::mention_query(&text) {
            match self.menu.take() {
                Some(Menu::Files {
                    query: old_query,
                    paths,
                    is_loading,
                    selected,
                    ..
                }) if old_query == query => Some(Menu::Files {
                    at,
                    query: old_query,
                    paths,
                    is_loading,
                    selected,
                }),
                old => {
                    let paths = match old {
                        Some(Menu::Files { paths, .. }) => paths,
                        Some(Menu::Commands { .. }) | None => Vec::new(),
                    };
                    let query = query.to_owned();
                    self.session.update(ctx, |session, ctx| {
                        session.request_file_suggestions(query.clone(), ctx)
                    });
                    Some(Menu::Files {
                        at,
                        query,
                        paths,
                        is_loading: true,
                        selected: 0,
                    })
                }
            }
        } else {
            None
        };
        self.menu = menu;
        self.set_menu_navigation(ctx);
    }

    /// Arrow keys and Tab drive the menu while it's open, and the editor otherwise.
    fn set_menu_navigation(&mut self, ctx: &mut ViewContext<Self>) {
        let keys = if self.menu.is_some() {
            PropagateAndNoOpNavigationKeys::Always
        } else {
            PropagateAndNoOpNavigationKeys::AtBoundary
        };
        self.editor.update(ctx, |editor, _| {
            editor.set_propagate_vertical_navigation_keys(keys)
        });
    }

    /// Runs or completes the selected menu row; `complete_only` (Tab) only fills in its text.
    fn accept_menu(&mut self, complete_only: bool, ctx: &mut ViewContext<Self>) {
        let Some(menu) = self.menu.take() else {
            return;
        };
        match menu {
            Menu::Commands {
                entries,
                matches,
                selected,
            } => {
                let Some(entry) = matches.get(selected).and_then(|index| entries.get(*index))
                else {
                    return;
                };
                let name = entry.name.clone();
                let ui = logic::ui_command(&name);
                if complete_only || (ui.is_none() && !entry.argument_hint.is_empty()) {
                    self.set_text(&format!("/{name} "), ctx);
                } else {
                    self.clear_text(ctx);
                    match ui {
                        Some(command) => self.run_ui_command(command, "", ctx),
                        None => self.send(format!("/{name}"), ctx),
                    }
                }
            }
            Menu::Files {
                at,
                paths,
                selected,
                ..
            } => {
                let Some(path) = paths.get(selected) else {
                    return;
                };
                let text = self.buffer_text(ctx);
                if at <= text.len() {
                    let completed = logic::complete_mention(&text, at, path);
                    self.set_text(&completed, ctx);
                }
            }
        }
        self.menu = None;
        self.set_menu_navigation(ctx);
        ctx.notify();
    }

    fn run_ui_command(&mut self, command: UiCommand, args: &str, ctx: &mut ViewContext<Self>) {
        match command {
            UiCommand::ModelPicker => {
                if args.is_empty() {
                    self.open_picker(PickerPage::Models, ctx);
                } else {
                    let model = args.to_owned();
                    self.session
                        .update(ctx, |session, ctx| session.set_model(Some(model), ctx));
                }
            }
            UiCommand::EffortPicker => {
                if args.is_empty() {
                    self.open_picker(PickerPage::Effort, ctx);
                } else {
                    let effort = args.to_owned();
                    self.session
                        .update(ctx, |session, ctx| session.set_effort(effort, ctx));
                }
            }
            UiCommand::OpenSidePanel(section) => ctx.emit(ComposerEvent::OpenSidePanel(section)),
            UiCommand::ToggleRemoteControl => {
                let enabled = !self
                    .session
                    .as_ref(ctx)
                    .conversation()
                    .remote_control()
                    .enabled;
                self.session
                    .update(ctx, |session, ctx| session.set_remote_control(enabled, ctx));
                ctx.emit(ComposerEvent::OpenSidePanel(SidePanelSection::Environment));
            }
            UiCommand::Compact => self
                .session
                .update(ctx, |session, ctx| session.compact(ctx)),
            UiCommand::NewChat => ctx.emit(ComposerEvent::NewChat),
            UiCommand::Rename => {
                let title = args.trim();
                if title.is_empty() {
                    self.set_text("/rename ", ctx);
                } else {
                    ctx.emit(ComposerEvent::Rename(title.to_owned()));
                }
            }
        }
        ctx.notify();
    }

    // --- sending ----------------------------------------------------------------------------

    fn submit(&mut self, ctx: &mut ViewContext<Self>) {
        let text = self.buffer_text(ctx);
        if let Some(request) = self.first_pending(ctx) {
            match request.kind {
                PendingRequestKind::Permission { .. } => {
                    let answer = if text.trim().is_empty() {
                        PermissionAnswer::AllowOnce
                    } else {
                        PermissionAnswer::Deny {
                            message: Some(text.trim().to_owned()),
                        }
                    };
                    self.clear_text(ctx);
                    self.session.update(ctx, |session, ctx| {
                        session.answer_permission(&request.request_id, answer, ctx)
                    });
                    return;
                }
                PendingRequestKind::Question { .. } => {
                    self.submit_answers(ctx);
                    return;
                }
                PendingRequestKind::PlanApproval { .. } => {
                    let choice = if text.trim().is_empty() {
                        PlanChoice::Approve
                    } else {
                        PlanChoice::KeepPlanning
                    };
                    self.answer_plan(choice, ctx);
                    return;
                }
            }
        }

        if let Some((name, args)) = logic::parse_command(&text)
            && let Some(command) = logic::ui_command(name)
        {
            let args = args.to_owned();
            self.clear_text(ctx);
            self.run_ui_command(command, &args, ctx);
            return;
        }
        let is_command = text.trim_start().starts_with('/');
        let message = if is_command {
            text.trim().to_owned()
        } else {
            let quotes: Vec<String> = self
                .mentions
                .drain(..)
                .map(|mention| mention.text)
                .collect();
            logic::message_with_quotes(&quotes, &text)
        };
        if message.is_empty() && self.images.is_empty() {
            return;
        }
        self.clear_text(ctx);
        self.send(message, ctx);
    }

    fn send(&mut self, message: String, ctx: &mut ViewContext<Self>) {
        let attachments: Vec<Attachment> = self.images.drain(..).collect();
        let images: Vec<OutgoingImage> = attachments
            .iter()
            .map(|attachment| attachment.image.clone())
            .collect();
        let is_busy = matches!(
            self.status(ctx),
            ConversationStatus::Working { .. } | ConversationStatus::AwaitingUser
        );
        let artifact_kind = self
            .artifact_kind
            .take()
            .filter(|_| !message.trim_start().starts_with('/'));
        let text = message.clone();
        let uuid = match artifact_kind {
            Some(kind) => {
                let instructions = logic::artifact_instructions(kind);
                self.session.update(ctx, |session, ctx| {
                    session.send_message_with_instructions(&instructions, message, images, ctx)
                })
            }
            None => self.session.update(ctx, |session, ctx| {
                session.send_message(message, images, ctx)
            }),
        };
        let conversation = self.session.as_ref(ctx).conversation();
        let is_queued = if conversation.tracks_queue() {
            conversation.is_queued(&uuid)
        } else {
            is_busy
        };
        if is_queued {
            let preview = if text.is_empty() {
                "Imagem".to_owned()
            } else {
                logic::preview(&text, QUEUED_PREVIEW_CHARS)
            };
            self.queued.push(QueuedMessage {
                uuid,
                text,
                preview,
                images: attachments,
                artifact_kind,
                withdrawing: None,
                sending_now: false,
                row: Default::default(),
                send_now: Default::default(),
                edit: Default::default(),
                delete: Default::default(),
            });
        }
        self.update_placeholder(ctx);
        ctx.notify();
    }

    fn withdraw_queued(&mut self, uuid: &str, withdrawal: Withdrawal, ctx: &mut ViewContext<Self>) {
        let Some(message) = self.queued.iter_mut().find(|message| message.uuid == uuid) else {
            return;
        };
        if message.withdrawing.is_some() {
            return;
        }
        message.withdrawing = Some(withdrawal);
        let uuid = uuid.to_owned();
        self.session
            .update(ctx, |session, ctx| session.cancel_queued_message(uuid, ctx));
        ctx.notify();
    }

    fn show_notice(&self, message: &str, ctx: &mut ViewContext<Self>) {
        let window_id = ctx.window_id();
        crate::workspace::ToastStack::handle(ctx).update(ctx, |toasts, ctx| {
            toasts.add_ephemeral_toast(
                crate::view_components::DismissibleToast::default(message.to_owned()),
                window_id,
                ctx,
            );
        });
    }

    fn on_queued_message_cancelled(
        &mut self,
        uuid: &str,
        cancelled: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(position) = self.queued.iter().position(|message| message.uuid == uuid) else {
            return;
        };
        if !cancelled {
            self.queued[position].withdrawing = None;
            self.sync_queue(&[], ctx);
            self.show_notice(QUEUED_ALREADY_SENT, ctx);
            ctx.notify();
            return;
        }
        let message = self.queued.remove(position);
        if message.withdrawing == Some(Withdrawal::Edit) {
            let current = self.buffer_text(ctx);
            let text = if current.trim().is_empty() {
                message.text
            } else {
                format!("{}\n\n{current}", message.text)
            };
            self.set_text(&text, ctx);
            for attachment in message.images {
                if self.images.len() < MAX_CHIPS {
                    self.images.push(attachment);
                }
            }
            if self.artifact_kind.is_none() {
                self.artifact_kind = message.artifact_kind;
            }
            self.focus(ctx);
        }
        self.update_placeholder(ctx);
        ctx.notify();
    }

    fn answer_plan(&mut self, choice: PlanChoice, ctx: &mut ViewContext<Self>) {
        let Some(PendingRequest {
            request_id,
            kind: PendingRequestKind::PlanApproval { .. },
            ..
        }) = self.first_pending(ctx)
        else {
            return;
        };
        let answer = match choice {
            PlanChoice::Approve => PlanAnswer::Approve {
                accept_edits: false,
            },
            PlanChoice::ApproveAcceptEdits => PlanAnswer::Approve { accept_edits: true },
            PlanChoice::KeepPlanning => {
                let feedback = self.buffer_text(ctx).trim().to_owned();
                PlanAnswer::KeepPlanning {
                    feedback: if feedback.is_empty() {
                        "Continue planejando.".to_owned()
                    } else {
                        feedback
                    },
                }
            }
        };
        self.clear_text(ctx);
        self.session.update(ctx, |session, ctx| {
            session.answer_plan(&request_id, answer, ctx)
        });
    }

    fn answer_permission(&mut self, choice: PermissionChoice, ctx: &mut ViewContext<Self>) {
        let Some(PendingRequest {
            request_id,
            kind: PendingRequestKind::Permission { suggestions, .. },
            ..
        }) = self.first_pending(ctx)
        else {
            return;
        };
        let answer = match choice {
            PermissionChoice::Allow => PermissionAnswer::AllowOnce,
            PermissionChoice::AllowAlways => match suggestions.into_iter().next() {
                Some(suggestion) => PermissionAnswer::AllowAlways { suggestion },
                None => PermissionAnswer::AllowOnce,
            },
            PermissionChoice::Deny => {
                let message = self.buffer_text(ctx).trim().to_owned();
                self.clear_text(ctx);
                PermissionAnswer::Deny {
                    message: (!message.is_empty()).then_some(message),
                }
            }
        };
        self.session.update(ctx, |session, ctx| {
            session.answer_permission(&request_id, answer, ctx)
        });
        self.focus(ctx);
    }

    fn toggle_option(&mut self, question: usize, option: usize, ctx: &mut ViewContext<Self>) {
        let Some(PendingRequest {
            kind: PendingRequestKind::Question { questions, .. },
            ..
        }) = self.first_pending(ctx)
        else {
            return;
        };
        let Some(spec) = questions.get(question) else {
            return;
        };
        if option >= spec.options.len() {
            return;
        }
        self.sync_question(ctx);
        let Some(selected) = self.question.selected.get_mut(question) else {
            return;
        };
        if spec.multi_select {
            if let Some(position) = selected.iter().position(|&picked| picked == option) {
                selected.remove(position);
            } else {
                selected.push(option);
            }
            self.question.active = question;
        } else {
            *selected = vec![option];
            self.question.active = (question + 1).min(questions.len().saturating_sub(1));
        }
        ctx.notify();
    }

    fn submit_answers(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(PendingRequest {
            request_id,
            kind: PendingRequestKind::Question { questions, .. },
            ..
        }) = self.first_pending(ctx)
        else {
            return;
        };
        self.sync_question(ctx);
        let typed = self.buffer_text(ctx);
        let mut answers = Vec::new();
        for (index, question) in questions.iter().enumerate() {
            let labels: Vec<&str> = self
                .question
                .selected
                .get(index)
                .into_iter()
                .flatten()
                .filter_map(|&option| question.options.get(option))
                .map(|option| option.label.as_str())
                .collect();
            let own_text = if index == self.question.active {
                typed.as_str()
            } else {
                ""
            };
            let Some(answer) = logic::question_answer(&labels, own_text) else {
                // Every question needs an answer before sending.
                self.question.active = index;
                ctx.notify();
                return;
            };
            answers.push((question.question.clone(), answer));
        }
        self.clear_text(ctx);
        self.session.update(ctx, |session, ctx| {
            session.answer_question(&request_id, answers, ctx)
        });
    }

    fn dismiss_question(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(PendingRequest {
            request_id,
            kind: PendingRequestKind::Question { .. },
            ..
        }) = self.first_pending(ctx)
        else {
            return;
        };
        self.session.update(ctx, |session, ctx| {
            session.answer_question(&request_id, Vec::new(), ctx)
        });
    }

    // --- images -------------------------------------------------------------------------------

    fn on_paste(&mut self, ctx: &mut ViewContext<Self>) {
        let content = ctx.clipboard().read();
        if let Some(images) = &content.images {
            let best = CLIPBOARD_IMAGE_MIME_TYPES
                .iter()
                .find_map(|format| images.iter().find(|image| image.mime_type == *format));
            if let Some(image) = best {
                let media_type = if image.mime_type == "image/jpg" {
                    "image/jpeg".to_owned()
                } else {
                    image.mime_type.clone()
                };
                self.add_image(media_type, image.data.clone(), ctx);
                return;
            }
        }
        if let Some(paths) = &content.paths {
            let images: Vec<String> = paths
                .iter()
                .filter(|path| image_media_type(Path::new(path)).is_some())
                .cloned()
                .collect();
            if !images.is_empty() && images.len() == paths.len() {
                self.load_image_files(images, ctx);
                return;
            }
        }
        let text = self
            .editor
            .read(ctx, |editor, _| editor.clipboard_text_content(content));
        self.editor.update(ctx, |editor, ctx| {
            editor.user_initiated_insert(&text, PlainTextEditorViewAction::Paste, ctx)
        });
    }

    fn pick_images(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.open_file_picker(
            move |result, ctx| {
                let Ok(paths) = result else { return };
                if let Some(handle) = ctx.handle().upgrade(ctx) {
                    handle.update(ctx, |me, ctx| me.load_image_files(paths, ctx));
                }
            },
            FilePickerConfiguration::new()
                .allow_multi_select()
                .set_allowed_file_types(vec![FileType::Image]),
        );
    }

    fn load_image_files(&mut self, paths: Vec<String>, ctx: &mut ViewContext<Self>) {
        ctx.spawn(
            async move {
                paths
                    .into_iter()
                    .filter_map(|path| {
                        let media_type = image_media_type(Path::new(&path))?;
                        let bytes = std::fs::read(&path).ok()?;
                        Some((media_type.to_owned(), bytes))
                    })
                    .collect::<Vec<_>>()
            },
            |me, images, ctx| {
                for (media_type, bytes) in images {
                    me.add_image(media_type, bytes, ctx);
                }
            },
        );
    }

    fn add_image(&mut self, media_type: String, bytes: Vec<u8>, ctx: &mut ViewContext<Self>) {
        if self.images.len() >= MAX_CHIPS {
            return;
        }
        let asset_id = format!("{}_image_{}", self.position_prefix, self.next_image_id);
        self.next_image_id += 1;
        AssetCache::handle(ctx).update(ctx, |cache, ctx| {
            cache.insert_raw_asset_bytes::<ImageType>(asset_id.clone(), &bytes, ctx);
        });
        self.images.push(Attachment {
            image: OutgoingImage {
                media_type,
                data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            },
            asset_id,
            remove: Default::default(),
        });
        self.focus(ctx);
        ctx.notify();
    }

    // --- model, effort and mode ---------------------------------------------------------------

    fn open_picker(&mut self, page: PickerPage, ctx: &mut ViewContext<Self>) {
        self.is_mode_menu_open = false;
        self.is_artifact_kind_menu_open = false;
        self.picker = Some(page);
        self.slider_settle = None;
        ctx.notify();
    }

    fn close_popovers(&mut self, ctx: &mut ViewContext<Self>) {
        if self.picker.is_some() || self.is_mode_menu_open || self.is_artifact_kind_menu_open {
            self.popovers_closed_at = Some(Instant::now());
        }
        self.picker = None;
        self.is_mode_menu_open = false;
        self.is_artifact_kind_menu_open = false;
        self.focus(ctx);
        ctx.notify();
    }

    fn just_closed_popovers(&self) -> bool {
        self.popovers_closed_at
            .is_some_and(|at| at.elapsed() < POPOVER_REOPEN_GUARD)
    }

    fn set_mode(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        let Some(mode) = logic::PERMISSION_MODES.get(index) else {
            return;
        };
        self.session.update(ctx, |session, ctx| {
            session.set_permission_mode(mode.value, ctx)
        });
        ctx.notify();
    }

    fn cycle_mode(&mut self, ctx: &mut ViewContext<Self>) {
        let current = self
            .session
            .as_ref(ctx)
            .conversation()
            .session()
            .permission_mode
            .clone();
        let next = logic::next_permission_mode(current.as_deref());
        self.session.update(ctx, |session, ctx| {
            session.set_permission_mode(next.value, ctx)
        });
        ctx.notify();
    }

    /// The effort levels of the current model, and the index among them of the effort Claude runs
    /// with: the one picked, else the model's default.
    fn effort_state(&self, app: &AppContext) -> (Vec<String>, Option<usize>) {
        let session = self.session.as_ref(app);
        let conversation = session.conversation();
        let model = logic::current_model(
            conversation.models(),
            session.chosen_model(),
            conversation.session().model.as_deref(),
        );
        let levels = logic::effort_levels(model);
        let current = session
            .effort()
            .and_then(|effort| levels.iter().position(|level| level == effort))
            .or_else(|| logic::default_effort_index(&levels));
        (levels, current)
    }

    /// Switches to the effort at `index`, gliding the slider's knob there.
    fn set_effort_level(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        let (levels, current) = self.effort_state(ctx);
        let Some(effort) = levels.get(index).cloned() else {
            return;
        };
        let from = self
            .slider_settle
            .and_then(|settle| settle.position())
            .or_else(|| current.map(|current| logic::effort_position(current, levels.len())))
            .unwrap_or(0.);
        self.slider_settle = Some(SliderSettle {
            from,
            to: logic::effort_position(index, levels.len()),
            started_at: Instant::now(),
        });
        self.session
            .update(ctx, |session, ctx| session.set_effort(effort, ctx));
        self.animate_slider_settle(ctx);
    }

    /// Starts or stops dictation into the editor, with Warp's voice input.
    fn toggle_voice(&mut self, ctx: &mut ViewContext<Self>) {
        #[cfg(feature = "voice_input")]
        {
            let toggled = self.editor.update(ctx, |editor, ctx| {
                editor.toggle_voice_input(&voice_input::VoiceInputToggledFrom::Button, ctx)
            });
            if !toggled && !self.voice.is_listening && !self.voice.is_transcribing {
                let message = if crate::auth::AuthStateProvider::as_ref(ctx)
                    .get()
                    .is_anonymous_or_logged_out()
                {
                    VOICE_LOGIN_MESSAGE
                } else if !crate::workspaces::user_workspaces::UserWorkspaces::as_ref(ctx)
                    .is_voice_enabled()
                {
                    VOICE_NOT_ALLOWED_MESSAGE
                } else {
                    VOICE_DISABLED_MESSAGE
                };
                let window_id = ctx.window_id();
                crate::workspace::ToastStack::handle(ctx).update(ctx, |toasts, ctx| {
                    toasts.add_ephemeral_toast(
                        crate::view_components::DismissibleToast::error(message.to_owned()),
                        window_id,
                        ctx,
                    );
                });
            }
        }
        #[cfg(not(feature = "voice_input"))]
        let _ = ctx;
    }

    fn animate_slider_settle(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.notify();
        if self
            .slider_settle
            .is_some_and(|settle| settle.position().is_none())
        {
            self.slider_settle = None;
            return;
        }
        ctx.spawn(Timer::after(SLIDER_FRAME), |me, _, ctx| {
            if me.slider_settle.is_some() {
                me.animate_slider_settle(ctx);
            }
        });
    }

    // --- rendering ----------------------------------------------------------------------------

    fn look(app: &AppContext) -> Look {
        let design = ChatDesign::as_ref(app);
        let fonts = design::fonts(app);
        Look {
            palette: design.palette(app).clone(),
            sizes: design.sizes().clone(),
            ui_font: fonts.ui,
            mono_font: fonts.mono,
        }
    }

    fn render_box(&self, look: &Look, app: &AppContext) -> Box<dyn Element> {
        let sizes = &look.sizes;
        let palette = &look.palette;
        let mut column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch);

        if !self.images.is_empty() {
            column.add_child(
                Container::new(self.render_attachments(look))
                    .with_padding_bottom(8.)
                    .finish(),
            );
        }

        let line_height = sizes.font_size * sizes.line_height_ratio;
        let editor_min = (sizes.composer_min_height - sizes.button_size).max(line_height);
        column.add_child(
            Container::new(
                ConstrainedBox::new(Clipped::new(ChildView::new(&self.editor).finish()).finish())
                    .with_min_height(editor_min)
                    .with_max_height(sizes.composer_max_height)
                    .finish(),
            )
            .with_horizontal_padding(4.)
            .finish(),
        );

        let is_working = self.is_working(app);
        let can_send = !self.is_editor_empty || !self.images.is_empty();
        let mut left = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(2.)
            .with_child(widgets::icon_button(
                look,
                self.mouse.attach.clone(),
                WarpIcon::Plus,
                sizes.button_size,
                ComposerAction::AttachImage,
            ))
            .with_child(self.render_mic_button(look));
        if let Some(kind) = self.artifact_kind {
            left.add_child(
                Container::new(self.render_artifact_kind_chip(look, kind))
                    .with_padding_left(4.)
                    .finish(),
            );
        }
        let mut controls = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(left.finish());
        let mut right = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(10.);
        if self.last_escape.is_some() {
            right.add_child(look.text(
                "Esc de novo para interromper",
                sizes.tiny_font_size,
                palette.text_faint.0,
            ));
        }
        right.add_child(self.render_send_button(look, is_working && !can_send, can_send));
        controls.add_child(right.finish());
        column.add_child(
            Container::new(controls.finish())
                .with_padding_top(6.)
                .finish(),
        );

        let card = Container::new(column.finish())
            .with_padding_top(sizes.composer_padding + 2.)
            .with_padding_bottom(sizes.composer_padding - 2.)
            .with_horizontal_padding(sizes.composer_padding)
            .with_background_color(palette.surface.0)
            .with_border(Border::all(1.).with_border_color(palette.border.0))
            .with_corner_radius(look.radius(sizes.composer_radius))
            .with_drop_shadow(DropShadow {
                color: palette.shadow.0,
                offset: vec2f(0., 4.),
                blur_radius: sizes.shadow_blur,
                spread_radius: 0.,
            })
            .finish();
        SavePosition::new(card, &self.position_id("box")).finish()
    }

    /// The dictation button: a microphone, a stop button while listening, dots while the audio is
    /// transcribed.
    fn render_mic_button(&self, look: &Look) -> Box<dyn Element> {
        let look = look.clone();
        let voice = self.voice;
        let size = look.sizes.button_size;
        Hoverable::new(self.mouse.mic.clone(), move |state| {
            let palette = &look.palette;
            let hovered = state.is_hovered();
            let (warp_icon, color, background) = if voice.is_listening {
                (
                    WarpIcon::StopFilled,
                    palette.on_accent.0,
                    Some(palette.error.0),
                )
            } else if voice.is_transcribing {
                (
                    WarpIcon::DotsHorizontal,
                    palette.accent.0,
                    Some(palette.surface_hover.0),
                )
            } else if hovered {
                (
                    WarpIcon::Microphone,
                    palette.text.0,
                    Some(palette.surface_hover.0),
                )
            } else {
                (WarpIcon::Microphone, palette.text_muted.0, None)
            };
            let icon_size = if voice.is_listening {
                look.sizes.small_icon_size - 1.
            } else {
                look.sizes.icon_size
            };
            let mut container = Container::new(icon(warp_icon, color, icon_size))
                .with_uniform_padding((size - icon_size) / 2.)
                .with_corner_radius(look.radius(size / 2.));
            if let Some(background) = background {
                container = container.with_background_color(background);
            }
            container.finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(ComposerAction::ToggleVoice))
        .finish()
    }

    /// The kind of artifact the next message asks for, in the accent color; a click offers to
    /// remove it.
    fn render_artifact_kind_chip(&self, look: &Look, kind: ArtifactKind) -> Box<dyn Element> {
        let look = look.clone();
        let label = kind_label_pt(kind);
        let warp_icon = artifact_kind_icon(kind);
        let chip = Hoverable::new(self.mouse.artifact_kind.clone(), move |state| {
            let palette = &look.palette;
            let sizes = &look.sizes;
            let alpha = if state.is_hovered() { 0x38 } else { 0x24 };
            Container::new(
                ConstrainedBox::new(
                    Flex::row()
                        .with_main_axis_size(MainAxisSize::Min)
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_spacing(6.)
                        .with_child(icon(warp_icon, palette.accent.0, sizes.small_icon_size))
                        .with_child(look.medium_text(
                            label,
                            sizes.small_font_size,
                            palette.accent.0,
                        ))
                        .with_child(icon(
                            WarpIcon::ChevronDown,
                            palette.accent.0,
                            sizes.small_icon_size - 3.,
                        ))
                        .finish(),
                )
                .with_height(sizes.chip_height)
                .finish(),
            )
            .with_horizontal_padding(10.)
            .with_background_color(with_alpha(palette.accent.0, alpha))
            .with_corner_radius(look.radius(sizes.chip_height / 2.))
            .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(ComposerAction::ToggleArtifactKindMenu))
        .finish();
        SavePosition::new(chip, &self.position_id("artifact_kind_chip")).finish()
    }

    fn render_send_button(&self, look: &Look, is_stop: bool, can_send: bool) -> Box<dyn Element> {
        let look = look.clone();
        let size = look.sizes.button_size;
        let (warp_icon, action) = if is_stop {
            (WarpIcon::StopFilled, ComposerAction::Interrupt)
        } else {
            (WarpIcon::ArrowUp, ComposerAction::Submit)
        };
        let is_enabled = is_stop || can_send;
        Hoverable::new(self.mouse.send.clone(), move |state| {
            let palette = &look.palette;
            let (background, color) = if is_enabled {
                let alpha = if state.is_hovered() { 0xe6 } else { 0xff };
                (with_alpha(palette.accent.0, alpha), palette.on_accent.0)
            } else {
                (palette.surface_strong.0, palette.text_faint.0)
            };
            let icon_size = if is_stop {
                look.sizes.small_icon_size - 1.
            } else {
                look.sizes.icon_size
            };
            Container::new(icon(warp_icon, color, icon_size))
                .with_uniform_padding((size - icon_size) / 2.)
                .with_background_color(background)
                .with_corner_radius(look.radius(size / 2.))
                .finish()
        })
        .with_cursor(if is_enabled {
            Cursor::PointingHand
        } else {
            Cursor::Arrow
        })
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish()
    }

    /// Quote chips and image thumbnails waiting to go with the next message.
    /// The quotes mentioned in the next message, as pills floating on the box's top edge.
    fn render_mention_chips(&self, look: &Look) -> Box<dyn Element> {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let page = palette.page.0;
        let is_dark = u32::from(page.r) + u32::from(page.g) + u32::from(page.b) < 3 * 128;
        let accent = palette.accent.0;
        let text_color = mix_colors(palette.text.0, accent, if is_dark { 0.3 } else { 0.55 });
        let background = mix_colors(palette.page.0, accent, if is_dark { 0.2 } else { 0.12 });
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.);
        for (index, mention) in self.mentions.iter().enumerate() {
            let content = Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(6.)
                .with_child(icon(
                    WarpIcon::MessageText,
                    widgets::with_alpha(text_color, 0xc0),
                    sizes.small_icon_size,
                ))
                .with_child(look.text(
                    logic::preview(&mention.text, MENTION_PREVIEW_CHARS),
                    sizes.small_font_size,
                    text_color,
                ))
                .with_child(self.render_remove_button(
                    look,
                    mention.remove.clone(),
                    ComposerAction::RemoveMention(index),
                ))
                .finish();
            row.add_child(
                Container::new(
                    ConstrainedBox::new(content)
                        .with_height(MENTION_CHIP_HEIGHT)
                        .finish(),
                )
                .with_horizontal_padding(10.)
                .with_background_color(background)
                .with_border(Border::all(1.).with_border_color(widgets::with_alpha(accent, 0x80)))
                .with_corner_radius(look.radius(MENTION_CHIP_HEIGHT / 2.))
                .finish(),
            );
        }
        row.finish()
    }

    fn render_attachments(&self, look: &Look) -> Box<dyn Element> {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let mut row = warpui::elements::Wrap::row()
            .with_spacing(6.)
            .with_run_spacing(6.);
        for (index, attachment) in self.images.iter().enumerate() {
            let thumbnail = Image::new(
                AssetSource::Raw {
                    id: attachment.asset_id.clone(),
                },
                CacheOption::BySize,
            )
            .cover()
            .with_corner_radius(look.radius(sizes.chip_radius + 2.))
            .finish();
            let thumb_size = 56.;
            let mut stack = Stack::new().with_child(
                ConstrainedBox::new(
                    Container::new(thumbnail)
                        .with_border(Border::all(1.).with_border_color(palette.border.0))
                        .with_corner_radius(look.radius(sizes.chip_radius + 2.))
                        .finish(),
                )
                .with_width(thumb_size)
                .with_height(thumb_size)
                .finish(),
            );
            stack.add_positioned_child(
                self.render_remove_button(
                    look,
                    attachment.remove.clone(),
                    ComposerAction::RemoveImage(index),
                ),
                OffsetPositioning::offset_from_parent(
                    vec2f(-3., 3.),
                    warpui::elements::ParentOffsetBounds::Unbounded,
                    warpui::elements::ParentAnchor::TopRight,
                    ChildAnchor::TopRight,
                ),
            );
            row.add_child(stack.finish());
        }
        row.finish()
    }

    fn render_remove_button(
        &self,
        look: &Look,
        mouse_state: MouseStateHandle,
        action: ComposerAction,
    ) -> Box<dyn Element> {
        let look = look.clone();
        Hoverable::new(mouse_state, move |state| {
            let palette = &look.palette;
            let (background, color) = if state.is_hovered() {
                (palette.text.0, palette.page.0)
            } else {
                (palette.surface_hover.0, palette.text_muted.0)
            };
            Container::new(icon(WarpIcon::X, color, 9.))
                .with_uniform_padding(3.5)
                .with_background_color(background)
                .with_corner_radius(look.radius(8.))
                .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish()
    }

    /// The permission mode on the left, the model, effort and context ring on the right.
    fn render_footer(&self, look: &Look, app: &AppContext) -> Box<dyn Element> {
        let palette = &look.palette;
        let session = self.session.as_ref(app);
        let conversation = session.conversation();
        let mode_index =
            logic::permission_mode_index(conversation.session().permission_mode.as_deref());
        let mode = &logic::PERMISSION_MODES[mode_index];
        let mode_color = mode_color(mode.value, look);
        let mode_icon = match mode.value {
            "plan" => WarpIcon::BookOpen,
            "acceptEdits" => WarpIcon::EditLine,
            "auto" => WarpIcon::Lightning,
            "bypassPermissions" => WarpIcon::AlertTriangle,
            _ => WarpIcon::Lock,
        };
        let mode_chip = SavePosition::new(
            widgets::footer_control(
                look,
                self.mouse.mode.clone(),
                Some((mode_icon, mode_color)),
                vec![(mode.label.to_owned(), mode_color)],
                ComposerAction::ToggleModeMenu,
            ),
            &self.position_id("mode_chip"),
        )
        .finish();

        let model_name = logic::model_display_name(
            conversation.models(),
            session.chosen_model(),
            conversation.session().model.as_deref(),
        );
        let (levels, current) = self.effort_state(app);
        let mut parts = vec![(model_name, palette.text_muted.0)];
        if !levels.is_empty()
            && let Some(effort) = current
                .map(|index| levels[index].clone())
                .or_else(|| session.effort().map(str::to_owned))
        {
            parts.push((logic::effort_label(&effort), palette.text_faint.0));
        }
        let model_chip = SavePosition::new(
            widgets::footer_control(
                look,
                self.mouse.model.clone(),
                None,
                parts,
                ComposerAction::TogglePicker,
            ),
            &self.position_id("model_chip"),
        )
        .finish();

        let fraction = logic::context_fraction(
            conversation.context_usage(),
            conversation.live_context_tokens(),
            conversation.context_window(),
        );
        let window = conversation.context_window().or_else(|| {
            conversation
                .context_usage()
                .and_then(|usage| usage.window_tokens())
        });
        let used = conversation.live_context_tokens().or_else(|| {
            conversation
                .context_usage()
                .and_then(|usage| usage.total_tokens)
        });
        let percent = (fraction.unwrap_or(0.) * 100.).round() as u32;
        let ring_label = match (used, window) {
            (Some(used), Some(window)) => format!(
                "{percent}%  ·  {} / {}",
                logic::format_tokens(used),
                logic::format_tokens(window)
            ),
            _ => format!("{percent}% do contexto"),
        };
        let right = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(2.)
            .with_child(model_chip)
            .with_child(self.render_context_ring(look, fraction, ring_label))
            .finish();

        Container::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(mode_chip)
                .with_child(right)
                .finish(),
        )
        .with_padding_top(6.)
        .finish()
    }

    fn render_context_ring(
        &self,
        look: &Look,
        fraction: Option<f32>,
        label: String,
    ) -> Box<dyn Element> {
        let look = look.clone();
        Hoverable::new(self.mouse.context.clone(), move |state| {
            let palette = &look.palette;
            let size = look.sizes.icon_size;
            let fraction = fraction.unwrap_or(0.);
            let color = if fraction >= 0.9 {
                palette.error.0
            } else if fraction >= 0.75 {
                palette.warning.0
            } else {
                palette.text.0
            };
            let mut ring = Stack::new().with_child(widgets::svg(
                "bundled/svg/picolas-ring-track.svg",
                palette.border_strong.0,
                size,
            ));
            if let Some(path) = ring_icon(fraction) {
                ring.add_child(widgets::svg(path, color, size));
            }
            let mut row = Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(6.);
            if state.is_hovered() {
                row.add_child(look.text(
                    label.clone(),
                    look.sizes.small_font_size,
                    palette.text_muted.0,
                ));
            }
            row.add_child(ring.finish());
            let mut container = Container::new(
                ConstrainedBox::new(row.finish())
                    .with_height(look.sizes.chip_height - 2.)
                    .finish(),
            )
            .with_horizontal_padding(6.)
            .with_corner_radius(look.radius(look.sizes.chip_radius));
            if state.is_hovered() {
                container = container.with_background_color(palette.surface_hover.0);
            }
            container.finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(ComposerAction::OpenContext))
        .finish()
    }
}

/// The arc icon for a context ring filled to `fraction`, in steps of 5%.
fn ring_icon(fraction: f32) -> Option<&'static str> {
    const RINGS: [&str; 20] = [
        "bundled/svg/picolas-ring-005.svg",
        "bundled/svg/picolas-ring-010.svg",
        "bundled/svg/picolas-ring-015.svg",
        "bundled/svg/picolas-ring-020.svg",
        "bundled/svg/picolas-ring-025.svg",
        "bundled/svg/picolas-ring-030.svg",
        "bundled/svg/picolas-ring-035.svg",
        "bundled/svg/picolas-ring-040.svg",
        "bundled/svg/picolas-ring-045.svg",
        "bundled/svg/picolas-ring-050.svg",
        "bundled/svg/picolas-ring-055.svg",
        "bundled/svg/picolas-ring-060.svg",
        "bundled/svg/picolas-ring-065.svg",
        "bundled/svg/picolas-ring-070.svg",
        "bundled/svg/picolas-ring-075.svg",
        "bundled/svg/picolas-ring-080.svg",
        "bundled/svg/picolas-ring-085.svg",
        "bundled/svg/picolas-ring-090.svg",
        "bundled/svg/picolas-ring-095.svg",
        "bundled/svg/picolas-ring-100.svg",
    ];
    let step = (fraction.clamp(0., 1.) * 20.).round() as usize;
    // Any use at all shows a sliver.
    let step = if fraction > 0. { step.max(1) } else { 0 };
    step.checked_sub(1).map(|index| RINGS[index])
}

fn artifact_kind_icon(kind: ArtifactKind) -> WarpIcon {
    match kind {
        ArtifactKind::Document => WarpIcon::File,
        ArtifactKind::Presentation => WarpIcon::LayoutAlt01,
        ArtifactKind::Design => WarpIcon::PaintBrush,
        ArtifactKind::Dashboard => WarpIcon::PieChart,
        ArtifactKind::Motion => WarpIcon::Play,
        ArtifactKind::Other => WarpIcon::Stars,
    }
}

/// Amber for the modes that act without asking, blue for plan, red for full access.
fn mode_color(mode: &str, look: &Look) -> ColorU {
    let palette = &look.palette;
    match mode {
        "plan" => palette.info.0,
        "acceptEdits" | "auto" => palette.warning.0,
        "bypassPermissions" => palette.error.0,
        _ => palette.text_muted.0,
    }
}

fn image_media_type(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn editor_text_colors(app: &AppContext) -> TextColors {
    let palette = ChatDesign::as_ref(app).palette(app);
    TextColors {
        default_color: ThemeFill::Solid(palette.text.0),
        disabled_color: ThemeFill::Solid(palette.text_faint.0),
        hint_color: ThemeFill::Solid(palette.text_faint.0),
    }
}

impl Entity for ComposerView {
    type Event = ComposerEvent;
}

impl View for ComposerView {
    fn ui_name() -> &'static str {
        "NativeChatComposer"
    }

    fn on_focus(&mut self, focus_ctx: &FocusContext, ctx: &mut ViewContext<Self>) {
        if focus_ctx.is_self_focused() {
            ctx.focus(&self.editor);
        }
    }

    fn keymap_context(&self, app: &AppContext) -> warpui::keymap::Context {
        let mut context = Self::default_keymap_context();
        if self.wants_digits(app) {
            context.set.insert(PICK_DIGITS_FLAG);
        }
        context
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let look = Self::look(app);
        let mut column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        for card in self.render_cards(&look, app) {
            column.add_child(Container::new(card).with_padding_bottom(8.).finish());
        }
        column.add_child(self.render_box(&look, app));
        column.add_child(self.render_footer(&look, app));

        let mut stack = Stack::new().with_child(column.finish());
        if !self.mentions.is_empty() {
            stack.add_positioned_overlay_child(
                self.render_mention_chips(&look),
                OffsetPositioning::offset_from_save_position_element(
                    self.position_id("box"),
                    vec2f(MENTION_CHIPS_INSET, -MENTION_CHIPS_OFFSET),
                    PositionedElementOffsetBounds::WindowByPosition,
                    PositionedElementAnchor::TopLeft,
                    ChildAnchor::BottomLeft,
                ),
            );
        }
        if let Some(menu) = self.render_menu(&look) {
            let width = app
                .element_position_by_id_at_last_frame(self.window_id, self.position_id("box"))
                .map_or(look.sizes.column_max_width, |rect| rect.width());
            stack.add_positioned_overlay_child(
                ConstrainedBox::new(menu).with_width(width).finish(),
                OffsetPositioning::offset_from_save_position_element(
                    self.position_id("box"),
                    vec2f(0., -6.),
                    PositionedElementOffsetBounds::WindowByPosition,
                    PositionedElementAnchor::TopLeft,
                    ChildAnchor::BottomLeft,
                ),
            );
        }
        if let Some(page) = self.picker {
            stack.add_positioned_overlay_child(
                self.render_picker(page, &look, app),
                OffsetPositioning::offset_from_save_position_element(
                    self.position_id("model_chip"),
                    vec2f(0., -6.),
                    PositionedElementOffsetBounds::WindowByPosition,
                    PositionedElementAnchor::TopRight,
                    ChildAnchor::BottomRight,
                ),
            );
        }
        if self.is_artifact_kind_menu_open
            && let Some(kind) = self.artifact_kind
        {
            stack.add_positioned_overlay_child(
                self.render_artifact_kind_menu(kind, &look),
                OffsetPositioning::offset_from_save_position_element(
                    self.position_id("artifact_kind_chip"),
                    vec2f(0., -6.),
                    PositionedElementOffsetBounds::WindowByPosition,
                    PositionedElementAnchor::TopLeft,
                    ChildAnchor::BottomLeft,
                ),
            );
        }
        if self.is_mode_menu_open {
            stack.add_positioned_overlay_child(
                self.render_mode_menu(&look, app),
                OffsetPositioning::offset_from_save_position_element(
                    self.position_id("mode_chip"),
                    vec2f(0., -6.),
                    PositionedElementOffsetBounds::WindowByPosition,
                    PositionedElementAnchor::TopLeft,
                    ChildAnchor::BottomLeft,
                ),
            );
        }
        stack.finish()
    }
}

impl TypedActionView for ComposerView {
    type Action = ComposerAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ComposerAction::Submit => {
                if self.menu.is_some() {
                    self.accept_menu(false, ctx);
                } else {
                    self.submit(ctx);
                }
                self.focus(ctx);
            }
            ComposerAction::Interrupt => {
                self.session
                    .update(ctx, |session, ctx| session.interrupt(ctx));
            }
            ComposerAction::AttachImage => self.pick_images(ctx),
            ComposerAction::RemoveImage(index) => {
                if *index < self.images.len() {
                    self.images.remove(*index);
                    ctx.notify();
                }
            }
            ComposerAction::RemoveMention(index) => {
                if *index < self.mentions.len() {
                    self.mentions.remove(*index);
                    ctx.notify();
                }
            }
            ComposerAction::ToggleModeMenu => {
                if self.is_mode_menu_open {
                    self.close_popovers(ctx);
                } else if !self.just_closed_popovers() {
                    self.picker = None;
                    self.is_mode_menu_open = true;
                    ctx.notify();
                }
            }
            ComposerAction::SetMode(index) => {
                self.set_mode(*index, ctx);
                self.close_popovers(ctx);
            }
            ComposerAction::CycleMode => self.cycle_mode(ctx),
            ComposerAction::TogglePicker => {
                if self.picker.is_some() {
                    self.close_popovers(ctx);
                } else if !self.just_closed_popovers() {
                    self.open_picker(PickerPage::Effort, ctx);
                }
            }
            ComposerAction::ShowModels => {
                self.picker = Some(PickerPage::Models);
                ctx.notify();
            }
            ComposerAction::ChooseModel(index) => {
                let value = self
                    .session
                    .as_ref(ctx)
                    .conversation()
                    .models()
                    .get(*index)
                    .map(|model| model.value.clone());
                if let Some(value) = value {
                    self.session
                        .update(ctx, |session, ctx| session.set_model(Some(value), ctx));
                }
                self.close_popovers(ctx);
            }
            ComposerAction::SetEffort(index) => self.set_effort_level(*index, ctx),
            ComposerAction::ToggleVoice => self.toggle_voice(ctx),
            ComposerAction::ToggleArtifactKindMenu => {
                if self.is_artifact_kind_menu_open {
                    self.close_popovers(ctx);
                } else if !self.just_closed_popovers() {
                    self.picker = None;
                    self.is_mode_menu_open = false;
                    self.is_artifact_kind_menu_open = true;
                    ctx.notify();
                }
            }
            ComposerAction::RemoveArtifactKind => {
                self.artifact_kind = None;
                self.close_popovers(ctx);
                self.update_placeholder(ctx);
            }
            ComposerAction::ResetEffort => {
                self.session
                    .update(ctx, |session, ctx| session.reset_effort(ctx));
                self.close_popovers(ctx);
            }
            ComposerAction::ClosePopovers => self.close_popovers(ctx),
            ComposerAction::OpenContext => {
                ctx.emit(ComposerEvent::OpenSidePanel(SidePanelSection::Context));
            }
            ComposerAction::AcceptMenuRow(index) => {
                if let Some(menu) = &mut self.menu {
                    menu.select(*index);
                    self.accept_menu(false, ctx);
                }
                self.focus(ctx);
            }
            ComposerAction::Permission(choice) => self.answer_permission(*choice, ctx),
            ComposerAction::ToggleOption { question, option } => {
                self.toggle_option(*question, *option, ctx);
            }
            ComposerAction::PickDigit(option) => {
                let question = self.question.active;
                self.toggle_option(question, *option, ctx);
            }
            ComposerAction::SubmitAnswers => self.submit_answers(ctx),
            ComposerAction::DismissQuestion => self.dismiss_question(ctx),
            ComposerAction::Plan(choice) => self.answer_plan(*choice, ctx),
            ComposerAction::ToggleTodos => {
                self.is_todos_expanded = !self.is_todos_expanded;
                ctx.notify();
            }
            ComposerAction::Restart => {
                self.session
                    .update(ctx, |session, ctx| session.restart(ctx));
            }
            ComposerAction::SendQueuedNow(uuid) => {
                let Some(message) = self
                    .queued
                    .iter_mut()
                    .find(|message| message.uuid == *uuid && !message.sending_now)
                else {
                    return;
                };
                message.sending_now = true;
                let uuid = uuid.clone();
                self.session.update(ctx, |session, ctx| {
                    session.send_queued_message_now(uuid, ctx)
                });
                ctx.notify();
            }
            ComposerAction::EditQueued(uuid) => self.withdraw_queued(uuid, Withdrawal::Edit, ctx),
            ComposerAction::DeleteQueued(uuid) => {
                self.withdraw_queued(uuid, Withdrawal::Delete, ctx)
            }
        }
    }
}

/// Whether conversation changes touch anything the composer shows; streamed text doesn't.
fn affects_composer(changes: &[ConversationChange]) -> bool {
    changes.is_empty()
        || changes.iter().any(|change| match change {
            ConversationChange::Status
            | ConversationChange::PendingRequests
            | ConversationChange::Session
            | ConversationChange::Commands
            | ConversationChange::Models
            | ConversationChange::ContextUsage
            | ConversationChange::Todos => true,
            ConversationChange::ItemInserted { .. }
            | ConversationChange::ItemUpdated { .. }
            | ConversationChange::ItemRemoved { .. }
            | ConversationChange::Account
            | ConversationChange::McpServers
            | ConversationChange::RateLimits
            | ConversationChange::Cost
            | ConversationChange::Tasks
            | ConversationChange::RemoteControl => false,
        })
}

/// Whether the process ended with an error, and why.
fn exit_error(state: &ProcessState) -> Option<&str> {
    match state {
        ProcessState::Exited { error } => error.as_deref(),
        ProcessState::Starting | ProcessState::Running => None,
    }
}

#[cfg(test)]
#[path = "composer_tests.rs"]
mod tests;

/// `from` moved `amount` (0 to 1) of the way toward `to`.
fn mix_colors(from: ColorU, to: ColorU, amount: f32) -> ColorU {
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
    ColorU {
        r: channel(from.r, to.r),
        g: channel(from.g, to.g),
        b: channel(from.b, to.b),
        a: 0xff,
    }
}
