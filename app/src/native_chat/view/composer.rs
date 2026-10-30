//! The message box under the conversation, and the requests waiting above it (approvals,
//! questions, the plan, queued messages, the task list).

mod cards;
pub(super) mod git_status;
mod logic;
mod popovers;
mod widgets;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::Engine as _;
use claude_chat::artifacts::{ArtifactKind, kind_label_pt};
use claude_chat::attachments::{self, AttachmentKind, FileDelivery, ListedFile};
use claude_chat::inline::{self, DraftAttachment, DraftContent, TokenId, TokenKind};
use claude_chat::protocol::{OutgoingDocument, OutgoingImage};
use claude_chat::reducer::{ConversationChange, ConversationStatus};
use claude_chat::timeline::{PendingRequest, PendingRequestKind, TimelineItem};
use instant::Instant;
use pathfinder_color::ColorU;
use string_offset::{ByteOffset, CharOffset};
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
    PositionedElementOffsetBounds, SavePosition, Shrinkable, Stack, Text,
};
use warpui::geometry::vector::vec2f;
use warpui::image_cache::{CacheOption, ImageType};
use warpui::keymap::FixedBinding;
use warpui::platform::{Cursor, FilePickerConfiguration};
use warpui::text_layout::ClipConfig;
use warpui::text_layout::TextBorder;
use warpui::{
    AppContext, Entity, FocusContext, ModelHandle, SingletonEntity, TypedActionView, View,
    ViewContext, ViewHandle, WindowId,
};

use self::logic::{CommandEntry, UiCommand};
use self::widgets::{Look, icon, with_alpha};
use super::SidePanelSection;
use crate::editor::{
    CursorColors, EditorOptions, EditorView, Event as EditorEvent, PlainTextEditorViewAction,
    PropagateAndNoOpNavigationKeys, TextColors, TextOptions, TextStyleOperation,
};
use crate::native_chat::design::{self, ChatDesign};
use crate::native_chat::provider::ChatProvider;
use crate::native_chat::session::{
    ClaudeChatSession, ClaudeChatSessionEvent, PermissionAnswer, PlanAnswer, ProcessState,
};

const MENTION_CHIP_HEIGHT: f32 = 24.;
/// Lines the mention pills up with the text inside the box.
const MENTION_CHIPS_INSET: f32 = 14.;
/// Space between the mention pills and the box's top edge.
const MENTION_CHIPS_OFFSET: f32 = 6.;
const PLACEHOLDER: &str = "Digite / para comandos";
const PLACEHOLDER_PERMISSION: &str = "Digite para negar e dizer o que fazer";
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
const FILE_CHIP_MAX_NAME_WIDTH: f32 = 220.;
const GIT_BRANCH_MAX_WIDTH: f32 = 180.;
/// Below this width the footer's chips wrap onto more lines.
const FOOTER_WRAP_WIDTH: f32 = 420.;
const CURSOR_WIDTH: f32 = 2.;
/// A rough average glyph width as a share of the font size, to estimate the footer's width.
const FOOTER_GLYPH_WIDTH_RATIO: f32 = 0.6;
const FOOTER_CHIP_PADDING: f32 = 16.;
const FOOTER_RING_WIDTH: f32 = 28.;
const FOOTER_SPACING_SLACK: f32 = 24.;
/// How often the footer's git status is read again between Claude's turns.
const GIT_POLL_INTERVAL: Duration = Duration::from_secs(15);
const QUEUED_PREVIEW_CHARS: usize = 160;
const QUEUED_ALREADY_SENT: &str = "A mensagem já começou a ser lida.";
/// How much shorter than a line an attachment's token is, in font sizes. The text layout pads a
/// bordered run by a fifth of the font size, so this leaves a tenth of it between the tokens of
/// consecutive lines.
const TOKEN_LINE_GAP: f32 = 0.3;
const TOKEN_RADIUS: u8 = 5;
/// Attachments whose token was deleted kept at most, so undoing the deletion brings them back.
const MAX_DETACHED: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerEvent {
    OpenSidePanel(SidePanelSection),
    NewChat,
    /// Renames the conversation to this title.
    Rename(String),
    /// ⌘C with nothing selected in the composer, which copies what's selected in the transcript.
    CopyWithoutSelection,
    /// The git chip was clicked: show the changes of the repository rooted here.
    OpenDiff {
        repo_root: PathBuf,
    },
    /// The git status read for the chat's repositories changed; see [`ComposerView::git_statuses`].
    GitChanged,
    /// A model of another provider was picked in a chat whose conversation already started: open
    /// a new chat with it, taking the draft along.
    NewChatWithProvider {
        provider: ChatProvider,
        model: Option<String>,
        draft: Option<String>,
    },
}

/// What the effort slider and the permission card's buttons do, among others.
#[derive(Debug, Clone, PartialEq)]
pub enum ComposerAction {
    Submit,
    Interrupt,
    AttachFiles,
    RemoveImage(usize),
    RemoveFile(usize),
    /// The git chip was clicked: opens the diff, or the list of repositories when there are
    /// several.
    OpenDiff,
    /// A repository of the git chip's list was picked.
    OpenRepoDiff(usize),
    RemoveMention(usize),
    RemovePasted(usize),
    /// Puts a long paste's text back in the editor in place of its token.
    ExpandPasted(usize),
    ToggleModeMenu,
    SetMode(usize),
    CycleMode,
    TogglePicker,
    ShowModels,
    ChooseModel(usize),
    /// The model picker's Claude / Codex switch.
    BrowseProvider(ChatProvider),
    /// A model of a provider other than the chat's, by its index in the picker.
    ChooseProviderModel {
        provider: ChatProvider,
        index: usize,
    },
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
    key: u64,
    /// The number in its token.
    number: usize,
    text: String,
    remove: MouseStateHandle,
}

/// A message sent while Claude was busy, until it gets to it.
struct QueuedMessage {
    uuid: String,
    /// The draft as typed, tokens included, for editing it again.
    text: String,
    preview: String,
    images: Vec<Attachment>,
    files: Vec<FileChip>,
    mentions: Vec<Mention>,
    pasted: Vec<PastedText>,
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
    key: u64,
    /// The number in its token.
    number: usize,
    image: OutgoingImage,
    asset_id: String,
    remove: MouseStateHandle,
}

/// A file or folder, other than an inline image, waiting to go with the next message.
struct FileChip {
    key: u64,
    /// The number in its token.
    number: usize,
    /// Absolute path.
    path: String,
    name: String,
    kind: AttachmentKind,
    delivery: FileDelivery,
    /// The base64 block for a [`FileDelivery::Document`].
    document: Option<OutgoingDocument>,
    remove: MouseStateHandle,
}

impl FileChip {
    fn listed(&self) -> ListedFile {
        ListedFile {
            path: self.path.clone(),
            is_dir: self.kind == AttachmentKind::Folder,
            is_mention: self.delivery == FileDelivery::Mention,
        }
    }
}

/// A long paste kept out of the editor, whose text holds its token instead.
struct PastedText {
    key: u64,
    /// The number in its token.
    number: usize,
    text: String,
    open: MouseStateHandle,
    remove: MouseStateHandle,
}

/// An attachment whose token left the editor, kept while undoing could bring the token back.
enum Detached {
    Image(Attachment),
    File(FileChip),
    Mention(Mention),
    Pasted(PastedText),
}

impl Detached {
    fn key(&self) -> u64 {
        match self {
            Detached::Image(image) => image.key,
            Detached::File(file) => file.key,
            Detached::Mention(mention) => mention.key,
            Detached::Pasted(pasted) => pasted.key,
        }
    }
}

/// What reading a picked, pasted or dropped path gave.
enum LoadedPath {
    Image { media_type: String, bytes: Vec<u8> },
    File(FileChip),
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
    git: MouseStateHandle,
    git_repos: Vec<MouseStateHandle>,
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
            git: Default::default(),
            git_repos: handles(MAX_CHIPS),
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
    files: Vec<FileChip>,
    pasted: Vec<PastedText>,
    /// The editor's text as of the last edit, to tell when an edit cut into a token.
    last_text: String,
    /// Source of the attachments' [`inline::Link`] keys.
    next_key: u64,
    detached: Vec<Detached>,
    link_history: inline::LinkHistory,
    /// Where the cursor was, in bytes, so one moving into a token goes on past it.
    last_cursor: Option<usize>,
    /// The git status of each repository the chat works in, the chat's folder first.
    git: Vec<git_status::GitStatus>,
    is_loading_git: bool,
    is_git_menu_open: bool,
    menu: Option<Menu>,
    /// The text the menu was closed with Esc for; it stays closed until the text changes.
    menu_dismissed_for: Option<String>,
    picker: Option<PickerPage>,
    provider_picker: popovers::ProviderPicker,
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
    preview_scroll: ClippedScrollStateHandle,
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
        let line_height_ratio = ChatDesign::as_ref(ctx).sizes().line_height_ratio;
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
            editor.set_forwards_dropped_files(true);
            editor.set_line_height_ratio(Some(line_height_ratio), ctx);
            editor.set_beam_cursor_width(CURSOR_WIDTH, ctx);
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
            let line_height_ratio = ChatDesign::as_ref(ctx).sizes().line_height_ratio;
            me.editor.update(ctx, |editor, ctx| {
                editor.set_text_colors(colors, ctx);
                editor.set_line_height_ratio(Some(line_height_ratio), ctx);
            });
            me.style_tokens(ctx);
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
            files: Vec::new(),
            pasted: Vec::new(),
            last_text: draft.clone().unwrap_or_default(),
            next_key: 0,
            detached: Vec::new(),
            link_history: Default::default(),
            last_cursor: None,
            git: Vec::new(),
            is_loading_git: false,
            is_git_menu_open: false,
            menu: None,
            menu_dismissed_for: None,
            picker: None,
            provider_picker: Default::default(),
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
            preview_scroll: Default::default(),
            models_scroll: Default::default(),
            queue_scroll: Default::default(),
            mouse: MouseStates::default(),
        };
        me.refresh_menu(ctx);
        me.update_placeholder(ctx);
        me.send_dev_queue(ctx);
        me.attach_dev_files(ctx);
        me.refresh_git(ctx);
        me.poll_git(ctx);
        me.open_dev_diff(ctx);
        me
    }

    /// `PICOLAS_CHAT_OPEN_DIFF=<n>` clicks the git chip a few seconds after the chat opens, then
    /// picks the n-th repository when the chat has several (`menu` leaves the list open). Debug
    /// builds only.
    fn open_dev_diff(&self, ctx: &mut ViewContext<Self>) {
        if !cfg!(debug_assertions) {
            return;
        }
        let Ok(choice) = std::env::var("PICOLAS_CHAT_OPEN_DIFF") else {
            return;
        };
        // Only the chat `PICOLAS_OPEN_CHAT` opened, not the ones restored with the window.
        let cwd = self.session.as_ref(ctx).cwd().to_path_buf();
        let is_dev_chat = std::env::var("PICOLAS_OPEN_CHAT").is_ok_and(|spec| {
            let folder = spec.split('#').next().unwrap_or_default();
            Path::new(folder) == cwd || std::fs::canonicalize(folder).is_ok_and(|path| path == cwd)
        });
        if !is_dev_chat {
            return;
        }
        ctx.spawn(Timer::after(Duration::from_secs(8)), move |me, _, ctx| {
            me.handle_action(&ComposerAction::OpenDiff, ctx);
            if me.is_git_menu_open
                && let Ok(index) = choice.parse::<usize>()
            {
                me.handle_action(&ComposerAction::OpenRepoDiff(index), ctx);
            }
        });
    }

    /// `PICOLAS_CHAT_FILES=<path>[|<path>…]` attaches those files when the chat opens (with
    /// `PICOLAS_CHAT_PASTE=<path>` pasting that file's text and `PICOLAS_CHAT_MENTION=<text>`
    /// mentioning that text), and `PICOLAS_CHAT_FILES_SEND=<text>` sends them with that text a
    /// few seconds later, its `{Imagem 1}`-like placeholders replaced by the tokens. Debug builds
    /// only.
    fn attach_dev_files(&mut self, ctx: &mut ViewContext<Self>) {
        if !cfg!(debug_assertions) {
            return;
        }
        let Ok(paths) = std::env::var("PICOLAS_CHAT_FILES") else {
            return;
        };
        let paths = paths
            .split('|')
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect();
        self.attach_paths(paths, ctx);
        if let Ok(path) = std::env::var("PICOLAS_CHAT_PASTE")
            && let Ok(text) = std::fs::read_to_string(path)
        {
            self.add_pasted(text, ctx);
        }
        if let Ok(text) = std::env::var("PICOLAS_CHAT_MENTION") {
            self.add_mention(text, ctx);
        }
        if let Ok(text) = std::env::var("PICOLAS_CHAT_FILES_SEND") {
            ctx.spawn(Timer::after(Duration::from_secs(4)), move |me, _, ctx| {
                let current = me.buffer_text(ctx);
                let text =
                    inline::find_tokens(&current)
                        .into_iter()
                        .fold(text.clone(), |text, token| {
                            let token = &current[token.range];
                            let name_end = token.find([':', '·']).unwrap_or(token.len() - 1);
                            let name = token[1..name_end].trim_end().replace('\u{a0}', " ");
                            let placeholder = format!("{{{name}}}");
                            text.replace(&placeholder, token)
                        });
                me.set_text(&text, ctx);
                me.submit(ctx);
            });
        }
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

    /// Whether anything was typed or attached that wasn't sent yet.
    pub fn has_draft(&self) -> bool {
        !self.is_editor_empty
            || !self.mentions.is_empty()
            || !self.images.is_empty()
            || !self.files.is_empty()
            || !self.pasted.is_empty()
            || !self.queued.is_empty()
    }

    fn has_attachments(&self) -> bool {
        !self.mentions.is_empty()
            || !self.images.is_empty()
            || !self.files.is_empty()
            || !self.pasted.is_empty()
    }

    /// Quotes `text` in the next message where the cursor is, shown as a chip until then.
    pub fn add_mention(&mut self, text: String, ctx: &mut ViewContext<Self>) {
        if text.trim().is_empty() || self.mentions.len() >= MAX_CHIPS {
            return;
        }
        let number = self.next_number(TokenKind::Mention);
        let key = self.new_key();
        self.mentions.push(Mention {
            key,
            number,
            text,
            remove: Default::default(),
        });
        self.insert_token(
            TokenId {
                kind: TokenKind::Mention,
                number,
            },
            "",
            ctx,
        );
        self.focus(ctx);
        ctx.notify();
    }

    // --- inline tokens ----------------------------------------------------------------------

    fn numbers(&self, kind: TokenKind) -> Vec<usize> {
        match kind {
            TokenKind::Image => self.images.iter().map(|image| image.number).collect(),
            TokenKind::File => self.files.iter().map(|file| file.number).collect(),
            TokenKind::Mention => self.mentions.iter().map(|mention| mention.number).collect(),
            TokenKind::Pasted => self.pasted.iter().map(|pasted| pasted.number).collect(),
        }
    }

    /// One more than the highest number in use, so a draft's numbers start at 1 again once the
    /// previous one is sent.
    fn next_number(&self, kind: TokenKind) -> usize {
        self.numbers(kind).into_iter().max().unwrap_or(0) + 1
    }

    fn has_attachment(&self, id: TokenId) -> bool {
        self.numbers(id.kind).contains(&id.number)
    }

    /// Inserts an attachment's token where the cursor is, spaced from the text around it.
    fn insert_token(&mut self, id: TokenId, detail: &str, ctx: &mut ViewContext<Self>) {
        let text = self.buffer_text(ctx);
        let (before, after) = match self.editor.as_ref(ctx).single_cursor_to_point(ctx) {
            Some(point) => chars_around(&text, point.row as usize, point.column as usize),
            None => (None, None),
        };
        let mut insert = String::new();
        if before.is_some_and(|character| !character.is_whitespace()) {
            insert.push(' ');
        }
        insert.push_str(&inline::editor_token_text(id, detail));
        if after.is_none_or(|character| !character.is_whitespace()) {
            insert.push(' ');
        }
        self.editor.update(ctx, |editor, ctx| {
            editor.user_initiated_insert(&insert, PlainTextEditorViewAction::Paste, ctx)
        });
    }

    /// Deletes the tokens of `id` from the editor, each with a space beside it.
    fn remove_tokens(&mut self, id: TokenId, ctx: &mut ViewContext<Self>) {
        let text = self.buffer_text(ctx);
        let ranges: Vec<_> = inline::find_tokens(&text)
            .into_iter()
            .filter(|token| token.id == id)
            .map(|token| {
                let mut range = token.range;
                if text[range.end..].starts_with(' ') {
                    range.end += 1;
                } else if text[..range.start].ends_with(' ') {
                    range.start -= 1;
                }
                ByteOffset::from(range.start)..ByteOffset::from(range.end)
            })
            .collect();
        for range in ranges.into_iter().rev() {
            self.editor
                .update(ctx, |editor, ctx| editor.system_delete(range, ctx));
        }
    }

    /// The tokens of the attachments in `text`.
    fn live_tokens(&self, text: &str) -> Vec<inline::Token> {
        inline::find_tokens(text)
            .into_iter()
            .filter(|token| self.has_attachment(token.id))
            .collect()
    }

    /// Undoes edits that broke a token: text typed inside one moves after it, and a deletion
    /// that cut into one deletes the rest of it.
    fn keep_tokens_whole(&mut self, ctx: &mut ViewContext<Self>) {
        let text = self.buffer_text(ctx);
        let previous = std::mem::replace(&mut self.last_text, text.clone());
        let tokens = self.live_tokens(&previous);
        if let Some((fixed, cursor)) =
            inline::move_insertion_out_of_token(&previous, &text, &tokens)
        {
            self.set_text(&fixed, ctx);
            self.place_cursor(cursor, ctx);
            return;
        }
        let Some((id, remnant)) = inline::token_remnant(&previous, &text) else {
            return;
        };
        if self.has_attachment(id) {
            let range = ByteOffset::from(remnant.start)..ByteOffset::from(remnant.end);
            self.editor
                .update(ctx, |editor, ctx| editor.system_delete(range, ctx));
        }
    }

    fn place_cursor(&mut self, offset: usize, ctx: &mut ViewContext<Self>) {
        let offset = ByteOffset::from(offset);
        self.editor.update(ctx, |editor, ctx| {
            editor.select_ranges_by_byte_offset([offset..offset], ctx)
        });
    }

    /// Keeps the cursor out of tokens and makes selections take tokens whole.
    fn snap_selection(&mut self, ctx: &mut ViewContext<Self>) {
        let Some((tail, head)) = self.editor.as_ref(ctx).first_selection_char_offsets(ctx) else {
            return;
        };
        let text = self.buffer_text(ctx);
        let byte_offset = |offset: CharOffset| {
            text.char_indices()
                .nth(offset.as_usize())
                .map_or(text.len(), |(index, _)| index)
        };
        let (tail, head) = (byte_offset(tail), byte_offset(head));
        let previous = self.last_cursor.replace(head);
        let tokens = self.live_tokens(&text);
        let Some((tail, head)) = inline::snap_selection(&tokens, tail, head, previous) else {
            return;
        };
        self.last_cursor = Some(head);
        let (start, end) = (tail.min(head), tail.max(head));
        self.editor.update(ctx, |editor, ctx| {
            editor
                .select_ranges_by_byte_offset([ByteOffset::from(start)..ByteOffset::from(end)], ctx)
        });
    }

    fn new_key(&mut self) -> u64 {
        self.next_key += 1;
        self.next_key
    }

    /// Each attachment with the token that places it.
    fn links(&self) -> Vec<inline::Link> {
        let link = |key, kind, number| inline::Link {
            key,
            id: TokenId { kind, number },
        };
        let images = self
            .images
            .iter()
            .map(|image| link(image.key, TokenKind::Image, image.number));
        let files = self
            .files
            .iter()
            .map(|file| link(file.key, TokenKind::File, file.number));
        let mentions = self
            .mentions
            .iter()
            .map(|mention| link(mention.key, TokenKind::Mention, mention.number));
        let pasted = self
            .pasted
            .iter()
            .map(|pasted| link(pasted.key, TokenKind::Pasted, pasted.number));
        images.chain(files).chain(mentions).chain(pasted).collect()
    }

    /// Keeps the attachments in step with the editor's text: one whose token is gone is removed,
    /// and undoing back to a text seen before brings back the attachments it had.
    fn link_attachments(&mut self, ctx: &mut ViewContext<Self>) {
        let text = self.buffer_text(ctx);
        let links = self.links();
        let keys: Vec<u64> = links.iter().map(|link| link.key).collect();
        if let Some(saved) = self.link_history.find(&text, &keys).map(<[_]>::to_vec) {
            self.relink(&saved);
            self.link_history.note(&text, saved);
            ctx.notify();
            return;
        }
        let unlinked = inline::unlinked(&text, &links);
        if unlinked.is_empty() {
            self.link_history.note(&text, links);
        } else {
            self.detach(&unlinked, ctx);
        }
    }

    /// Gives the attachments in `links` their numbers there, bringing detached ones back.
    fn relink(&mut self, links: &[inline::Link]) {
        let number = |key: u64| {
            links
                .iter()
                .find(|link| link.key == key)
                .map(|link| link.id.number)
        };
        for detached in std::mem::take(&mut self.detached) {
            let Some(number) = number(detached.key()) else {
                self.detached.push(detached);
                continue;
            };
            match detached {
                Detached::Image(image) => self.images.push(Attachment { number, ..image }),
                Detached::File(file) => self.files.push(FileChip { number, ..file }),
                Detached::Mention(mention) => self.mentions.push(Mention { number, ..mention }),
                Detached::Pasted(pasted) => self.pasted.push(PastedText { number, ..pasted }),
            }
        }
        for image in &mut self.images {
            image.number = number(image.key).unwrap_or(image.number);
        }
        for file in &mut self.files {
            file.number = number(file.key).unwrap_or(file.number);
        }
        for mention in &mut self.mentions {
            mention.number = number(mention.key).unwrap_or(mention.number);
        }
        for pasted in &mut self.pasted {
            pasted.number = number(pasted.key).unwrap_or(pasted.number);
        }
        self.images.sort_by_key(|image| image.number);
        self.files.sort_by_key(|file| file.number);
        self.mentions.sort_by_key(|mention| mention.number);
        self.pasted.sort_by_key(|pasted| pasted.number);
    }

    /// Removes the attachments with `keys`, whose tokens left the editor, and numbers the rest
    /// again. They're kept in case undo brings their tokens back.
    fn detach(&mut self, keys: &[u64], ctx: &mut ViewContext<Self>) {
        let mut kinds = Vec::new();
        let mut detached = Vec::new();
        for image in std::mem::take(&mut self.images) {
            if keys.contains(&image.key) {
                kinds.push(TokenKind::Image);
                detached.push(Detached::Image(image));
            } else {
                self.images.push(image);
            }
        }
        for file in std::mem::take(&mut self.files) {
            if keys.contains(&file.key) {
                kinds.push(TokenKind::File);
                detached.push(Detached::File(file));
            } else {
                self.files.push(file);
            }
        }
        for mention in std::mem::take(&mut self.mentions) {
            if keys.contains(&mention.key) {
                kinds.push(TokenKind::Mention);
                detached.push(Detached::Mention(mention));
            } else {
                self.mentions.push(mention);
            }
        }
        for pasted in std::mem::take(&mut self.pasted) {
            if keys.contains(&pasted.key) {
                kinds.push(TokenKind::Pasted);
                detached.push(Detached::Pasted(pasted));
            } else {
                self.pasted.push(pasted);
            }
        }
        self.detached.extend(detached);
        let text = self.buffer_text(ctx);
        self.link_history.save(&text, self.links());
        self.detached
            .retain(|detached| self.link_history.remembers(detached.key()));
        let excess = self.detached.len().saturating_sub(MAX_DETACHED);
        self.detached.drain(..excess);
        kinds.dedup();
        self.renumber(&kinds, ctx);
        ctx.notify();
    }

    /// Removes an attachment and its tokens, as its chip's remove button does.
    fn remove_attachment(&mut self, key: u64, ctx: &mut ViewContext<Self>) {
        let Some(link) = self.links().into_iter().find(|link| link.key == key) else {
            return;
        };
        self.remove_tokens(link.id, ctx);
        self.detach(&[key], ctx);
    }

    /// Numbers the attachments of `kinds` 1, 2, 3… again after some were removed, in the editor's
    /// tokens too, so they keep matching the chips.
    fn renumber(&mut self, kinds: &[TokenKind], ctx: &mut ViewContext<Self>) {
        let links = self.links();
        let numbers = inline::compact_numbers(&links, kinds);
        let new_number = |key: u64| {
            numbers
                .iter()
                .find(|(numbered, _)| *numbered == key)
                .map(|(_, number)| *number)
        };
        let changes: HashMap<TokenId, usize> = links
            .iter()
            .filter_map(|link| {
                new_number(link.key)
                    .filter(|number| *number != link.id.number)
                    .map(|number| (link.id, number))
            })
            .collect();
        if changes.is_empty() {
            return;
        }
        for image in &mut self.images {
            image.number = new_number(image.key).unwrap_or(image.number);
        }
        for file in &mut self.files {
            file.number = new_number(file.key).unwrap_or(file.number);
        }
        for mention in &mut self.mentions {
            mention.number = new_number(mention.key).unwrap_or(mention.number);
        }
        for pasted in &mut self.pasted {
            pasted.number = new_number(pasted.key).unwrap_or(pasted.number);
        }
        let text = self.buffer_text(ctx);
        let new_text = inline::renumber_tokens(&text, |id| changes.get(&id).copied());
        if new_text == text {
            return;
        }
        let cursor = self.last_cursor.unwrap_or(text.len()).min(text.len());
        let new_cursor = new_text.len() - (text.len() - cursor).min(new_text.len());
        self.set_text(&new_text, ctx);
        self.last_text = new_text;
        self.place_cursor(new_cursor, ctx);
    }

    /// Draws the tokens of the attachments in the editor as pills, and nothing else: typed text
    /// takes the background of the text before it, or of text deleted where it goes, which would
    /// look selected.
    fn style_tokens(&mut self, ctx: &mut ViewContext<Self>) {
        let text = self.buffer_text(ctx);
        let tokens = self.live_tokens(&text);
        let has_styles = self.editor.as_ref(ctx).text_style_runs(ctx).any(|run| {
            run.text_style().background_color.is_some() || run.text_style().border.is_some()
        });
        if tokens.is_empty() && !has_styles {
            return;
        }
        let palette = ChatDesign::as_ref(ctx).palette(ctx);
        let token_height = token_height_percent(ChatDesign::as_ref(ctx).sizes().line_height_ratio);
        let styled: Vec<_> = tokens
            .into_iter()
            .map(|token| {
                let start = text[..token.range.start].chars().count();
                let end = start + text[token.range.clone()].chars().count();
                let tint = match token.id.kind {
                    TokenKind::Image => palette.kind_design.0,
                    TokenKind::File | TokenKind::Mention => palette.accent.0,
                    TokenKind::Pasted => palette.text_muted.0,
                };
                let style = TextStyleOperation::default()
                    .set_background_color(mix_colors(palette.surface.0, tint, 0.14))
                    .set_border(TextBorder {
                        color: widgets::with_alpha(tint, 0x73),
                        radius: TOKEN_RADIUS,
                        width: 1,
                        line_height_ratio_override: Some(token_height),
                    });
                (CharOffset::from(start)..CharOffset::from(end), style)
            })
            .collect();
        let len = text.chars().count();
        self.editor.update(ctx, |editor, ctx| {
            if len > 0 {
                editor.update_buffer_styles(
                    [CharOffset::from(0)..CharOffset::from(len)],
                    TextStyleOperation::default()
                        .clear_background_color()
                        .clear_border(),
                    ctx,
                );
            }
            for (range, style) in styled {
                editor.update_buffer_styles([range], style, ctx);
            }
        });
    }

    /// Keeps a long paste out of the editor, which gets its token instead.
    fn add_pasted(&mut self, text: String, ctx: &mut ViewContext<Self>) {
        let number = self.next_number(TokenKind::Pasted);
        let detail = inline::pasted_detail(&text);
        let key = self.new_key();
        self.pasted.push(PastedText {
            key,
            number,
            text,
            open: Default::default(),
            remove: Default::default(),
        });
        self.insert_token(
            TokenId {
                kind: TokenKind::Pasted,
                number,
            },
            &detail,
            ctx,
        );
        ctx.notify();
    }

    /// Puts a long paste's text in place of its token, which detaches it.
    fn expand_pasted(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        let Some(pasted) = self.pasted.get(index) else {
            return;
        };
        let id = TokenId {
            kind: TokenKind::Pasted,
            number: pasted.number,
        };
        let pasted_text = pasted.text.clone();
        let text = self.buffer_text(ctx);
        let token = inline::find_tokens(&text)
            .into_iter()
            .find(|token| token.id == id);
        self.editor.update(ctx, |editor, ctx| {
            if let Some(token) = token {
                editor.select_ranges_by_byte_offset(
                    [ByteOffset::from(token.range.start)..ByteOffset::from(token.range.end)],
                    ctx,
                );
            }
            editor.user_initiated_insert(&pasted_text, PlainTextEditorViewAction::Paste, ctx);
        });
        self.remove_tokens(id, ctx);
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

    /// Empties the editor but for the tokens of the attachments still in the draft, which would
    /// go with them otherwise.
    fn clear_text(&mut self, ctx: &mut ViewContext<Self>) {
        let text = self.buffer_text(ctx);
        let tokens = self.live_tokens(&text);
        if tokens.is_empty() {
            self.editor
                .update(ctx, |editor, ctx| editor.clear_buffer(ctx));
            return;
        }
        let mut kept = tokens
            .into_iter()
            .map(|token| &text[token.range])
            .collect::<Vec<_>>()
            .join(" ");
        kept.push(' ');
        self.set_text(&kept, ctx);
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
                self.keep_tokens_whole(ctx);
                self.link_attachments(ctx);
                self.is_editor_empty = self.editor.as_ref(ctx).is_empty(ctx);
                self.style_tokens(ctx);
                self.refresh_menu(ctx);
                ctx.notify();
            }
            EditorEvent::SelectionChanged => self.snap_selection(ctx),
            EditorEvent::Paste => self.on_paste(ctx),
            EditorEvent::Copy => ctx.emit(ComposerEvent::CopyWithoutSelection),
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
            EditorEvent::DroppedImageFiles(paths) | EditorEvent::DroppedFiles(paths) => {
                self.attach_paths(paths.clone(), ctx)
            }
            EditorEvent::Focused => {
                self.picker = None;
                self.is_mode_menu_open = false;
                self.is_git_menu_open = false;
                ctx.notify();
            }
            _ => {}
        }
    }

    fn on_session_event(&mut self, event: &ClaudeChatSessionEvent, ctx: &mut ViewContext<Self>) {
        match event {
            ClaudeChatSessionEvent::Changed(changes) => {
                if changes
                    .iter()
                    .any(|change| matches!(change, ConversationChange::Status))
                    && !self.is_working(ctx)
                {
                    self.refresh_git(ctx);
                }
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
        if self.picker.is_some() || self.is_mode_menu_open || self.is_git_menu_open {
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
        if text.trim().is_empty() && !self.has_attachments() {
            return;
        }
        self.send(text, ctx);
        self.clear_text(ctx);
    }

    /// Sends `text` with the attachments its tokens place. A slash command goes alone, leaving
    /// them for the next message.
    fn send(&mut self, text: String, ctx: &mut ViewContext<Self>) {
        let is_busy = matches!(
            self.status(ctx),
            ConversationStatus::Working { .. } | ConversationStatus::AwaitingUser
        );
        let is_command = text.trim_start().starts_with('/');
        let (images, files, mentions, pasted, artifact_kind) = if is_command {
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), None)
        } else {
            self.detached.clear();
            self.link_history.clear();
            (
                std::mem::take(&mut self.images),
                std::mem::take(&mut self.files),
                std::mem::take(&mut self.mentions),
                std::mem::take(&mut self.pasted),
                self.artifact_kind.take(),
            )
        };
        let uuid = if is_command {
            let command = text.trim().to_owned();
            self.session.update(ctx, |session, ctx| {
                session.send_message(command, Vec::new(), ctx)
            })
        } else {
            let listed: Vec<ListedFile> = files.iter().map(FileChip::listed).collect();
            let mut draft: Vec<DraftAttachment> = images
                .iter()
                .map(|attachment| DraftAttachment {
                    id: TokenId {
                        kind: TokenKind::Image,
                        number: attachment.number,
                    },
                    content: DraftContent::Image(&attachment.image),
                })
                .collect();
            draft.extend(
                files
                    .iter()
                    .zip(&listed)
                    .map(|(file, listed)| DraftAttachment {
                        id: TokenId {
                            kind: TokenKind::File,
                            number: file.number,
                        },
                        content: DraftContent::File {
                            file: listed,
                            document: file.document.as_ref(),
                        },
                    }),
            );
            draft.extend(mentions.iter().map(|mention| DraftAttachment {
                id: TokenId {
                    kind: TokenKind::Mention,
                    number: mention.number,
                },
                content: DraftContent::Mention(&mention.text),
            }));
            draft.extend(pasted.iter().map(|pasted| DraftAttachment {
                id: TokenId {
                    kind: TokenKind::Pasted,
                    number: pasted.number,
                },
                content: DraftContent::Pasted(&pasted.text),
            }));
            let message = inline::compose(&text, &draft);
            let instructions = artifact_kind.map(logic::artifact_instructions);
            self.session.update(ctx, |session, ctx| {
                session.send_composed(instructions.as_deref(), message, ctx)
            })
        };
        let conversation = self.session.as_ref(ctx).conversation();
        let is_queued = if conversation.tracks_queue() {
            conversation.is_queued(&uuid)
        } else {
            is_busy
        };
        if is_queued {
            let preview = if !text.trim().is_empty() {
                logic::preview(&text, QUEUED_PREVIEW_CHARS)
            } else if let Some(file) = files.first() {
                file.name.clone()
            } else if !images.is_empty() {
                "Imagem".to_owned()
            } else {
                "Menção".to_owned()
            };
            self.queued.push(QueuedMessage {
                uuid,
                text,
                preview,
                images,
                files,
                mentions,
                pasted,
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
            // Numbers above the draft's own keep the restored tokens from taking its attachments.
            let image_shift = self.next_number(TokenKind::Image) - 1;
            let file_shift = self.next_number(TokenKind::File) - 1;
            let mention_shift = self.next_number(TokenKind::Mention) - 1;
            let pasted_shift = self.next_number(TokenKind::Pasted) - 1;
            let shift = |kind: TokenKind| match kind {
                TokenKind::Image => image_shift,
                TokenKind::File => file_shift,
                TokenKind::Mention => mention_shift,
                TokenKind::Pasted => pasted_shift,
            };
            let restored =
                inline::renumber_tokens(&message.text, |id| Some(id.number + shift(id.kind)));
            let current = self.buffer_text(ctx);
            let text = if current.trim().is_empty() {
                restored
            } else {
                format!("{restored}\n\n{current}")
            };
            for mut attachment in message.images {
                if self.images.len() < MAX_CHIPS {
                    attachment.number += image_shift;
                    self.images.push(attachment);
                }
            }
            for mut file in message.files {
                if self.files.len() < MAX_CHIPS {
                    file.number += file_shift;
                    self.files.push(file);
                }
            }
            for mut mention in message.mentions {
                if self.mentions.len() < MAX_CHIPS {
                    mention.number += mention_shift;
                    self.mentions.push(mention);
                }
            }
            for mut pasted in message.pasted {
                pasted.number += pasted_shift;
                self.pasted.push(pasted);
            }
            self.set_text(&text, ctx);
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
        if let Some(paths) = content.paths.as_ref().filter(|paths| !paths.is_empty()) {
            self.attach_paths(paths.clone(), ctx);
            return;
        }
        let text = self
            .editor
            .read(ctx, |editor, _| editor.clipboard_text_content(content));
        if inline::is_large_paste(&text) {
            self.add_pasted(text, ctx);
            return;
        }
        self.editor.update(ctx, |editor, ctx| {
            editor.user_initiated_insert(&text, PlainTextEditorViewAction::Paste, ctx)
        });
    }

    fn pick_files(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.open_file_picker(
            move |result, ctx| {
                let Ok(paths) = result else { return };
                if let Some(handle) = ctx.handle().upgrade(ctx) {
                    handle.update(ctx, |me, ctx| me.attach_paths(paths, ctx));
                }
            },
            FilePickerConfiguration::new()
                .allow_folder()
                .allow_multi_select(),
        );
    }

    /// Attaches files and folders: small images as thumbnails, everything else as file chips.
    fn attach_paths(&mut self, paths: Vec<String>, ctx: &mut ViewContext<Self>) {
        ctx.spawn(
            async move {
                let mut too_large = Vec::new();
                let loaded = paths
                    .into_iter()
                    .filter_map(|path| {
                        let loaded = load_path(path)?;
                        if let LoadedPath::File(file) = &loaded
                            && file.delivery == FileDelivery::Path
                            && matches!(file.kind, AttachmentKind::Image | AttachmentKind::Pdf)
                        {
                            too_large.push(file.name.clone());
                        }
                        Some(loaded)
                    })
                    .collect::<Vec<_>>();
                (loaded, too_large)
            },
            |me, (loaded, too_large), ctx| {
                for loaded in loaded {
                    match loaded {
                        LoadedPath::Image { media_type, bytes } => {
                            me.add_image(media_type, bytes, ctx)
                        }
                        LoadedPath::File(file) => me.add_file(file, ctx),
                    }
                }
                if !too_large.is_empty() {
                    me.show_notice(
                        &format!(
                            "{} passa de {} MB: vai como caminho, e o Claude o lê com as \
                             ferramentas dele.",
                            too_large.join(", "),
                            attachments::MAX_INLINE_BYTES / (1024 * 1024)
                        ),
                        ctx,
                    );
                }
            },
        );
    }

    fn add_file(&mut self, mut file: FileChip, ctx: &mut ViewContext<Self>) {
        if self.files.len() >= MAX_CHIPS || self.files.iter().any(|chip| chip.path == file.path) {
            return;
        }
        file.number = self.next_number(TokenKind::File);
        file.key = self.new_key();
        let id = TokenId {
            kind: TokenKind::File,
            number: file.number,
        };
        let name = file.name.clone();
        self.files.push(file);
        self.insert_token(id, &name, ctx);
        self.focus(ctx);
        ctx.notify();
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
        let number = self.next_number(TokenKind::Image);
        let key = self.new_key();
        self.images.push(Attachment {
            key,
            number,
            image: OutgoingImage {
                media_type,
                data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            },
            asset_id,
            remove: Default::default(),
        });
        self.insert_token(
            TokenId {
                kind: TokenKind::Image,
                number,
            },
            "",
            ctx,
        );
        self.focus(ctx);
        ctx.notify();
    }

    // --- git ---------------------------------------------------------------------------------

    /// The git status of each repository the chat works in, the chat's folder first.
    pub fn git_statuses(&self) -> &[git_status::GitStatus] {
        &self.git
    }

    fn refresh_git(&mut self, ctx: &mut ViewContext<Self>) {
        if self.is_loading_git {
            return;
        }
        self.is_loading_git = true;
        let dirs = self
            .session
            .as_ref(ctx)
            .dirs()
            .into_iter()
            .map(Path::to_path_buf)
            .collect();
        ctx.spawn(git_status::load_all(dirs), |me, statuses, ctx| {
            me.is_loading_git = false;
            if me.git != statuses {
                me.git = statuses;
                if me.git.len() < 2 {
                    me.is_git_menu_open = false;
                }
                ctx.emit(ComposerEvent::GitChanged);
                ctx.notify();
            }
        });
    }

    /// Reads the git status again every [`GIT_POLL_INTERVAL`] while the folder is a repository;
    /// the end of each of Claude's turns reads it too.
    fn poll_git(&self, ctx: &mut ViewContext<Self>) {
        ctx.spawn(Timer::after(GIT_POLL_INTERVAL), |me, _, ctx| {
            if !me.git.is_empty() {
                me.refresh_git(ctx);
            }
            me.poll_git(ctx);
        });
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
        if self.picker.is_some()
            || self.is_mode_menu_open
            || self.is_artifact_kind_menu_open
            || self.is_git_menu_open
        {
            self.popovers_closed_at = Some(Instant::now());
        }
        self.picker = None;
        self.provider_picker.reset();
        self.is_mode_menu_open = false;
        self.is_artifact_kind_menu_open = false;
        self.is_git_menu_open = false;
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

        if !self.images.is_empty() || !self.files.is_empty() || !self.pasted.is_empty() {
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
        let can_send = !self.is_editor_empty || self.has_attachments();
        let mut left = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(2.)
            .with_child(widgets::icon_button(
                look,
                self.mouse.attach.clone(),
                WarpIcon::Plus,
                sizes.button_size,
                ComposerAction::AttachFiles,
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
                    format!(
                        "{} · {}",
                        mention.number,
                        logic::preview(&mention.text, MENTION_PREVIEW_CHARS)
                    ),
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
        let mut column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_spacing(8.);
        if !self.images.is_empty() {
            column.add_child(self.render_thumbnails(look));
        }
        if !self.files.is_empty() || !self.pasted.is_empty() {
            column.add_child(self.render_file_chips(look));
        }
        column.finish()
    }

    /// Each attached file as a pill: its kind's icon, its number and name and a remove button.
    /// Long pastes follow, opening back into the editor when clicked.
    fn render_file_chips(&self, look: &Look) -> Box<dyn Element> {
        let palette = &look.palette;
        let sizes = &look.sizes;
        let mut row = warpui::elements::Wrap::row()
            .with_spacing(6.)
            .with_run_spacing(6.);
        for (index, file) in self.files.iter().enumerate() {
            let content = Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(6.)
                .with_child(icon(
                    attachment_icon(file.kind),
                    palette.text_muted.0,
                    sizes.small_icon_size,
                ))
                .with_child(look.text(
                    file.number.to_string(),
                    sizes.small_font_size,
                    palette.text_faint.0,
                ))
                .with_child(
                    ConstrainedBox::new(look.text(
                        file.name.clone(),
                        sizes.small_font_size,
                        palette.text.0,
                    ))
                    .with_max_width(FILE_CHIP_MAX_NAME_WIDTH)
                    .finish(),
                )
                .with_child(self.render_remove_button(
                    look,
                    file.remove.clone(),
                    ComposerAction::RemoveFile(index),
                ))
                .finish();
            row.add_child(
                Container::new(
                    ConstrainedBox::new(content)
                        .with_height(sizes.chip_height + 4.)
                        .finish(),
                )
                .with_padding_left(10.)
                .with_padding_right(6.)
                .with_background_color(palette.surface_hover.0)
                .with_border(Border::all(1.).with_border_color(palette.border.0))
                .with_corner_radius(look.radius(sizes.chip_radius + 2.))
                .finish(),
            );
        }
        for (index, pasted) in self.pasted.iter().enumerate() {
            let label = format!(
                "Texto colado {} · {}",
                pasted.number,
                inline::pasted_detail(&pasted.text)
            );
            let open = {
                let look = look.clone();
                Hoverable::new(pasted.open.clone(), move |state| {
                    let palette = &look.palette;
                    let sizes = &look.sizes;
                    let color = if state.is_hovered() {
                        palette.text.0
                    } else {
                        palette.text_muted.0
                    };
                    Flex::row()
                        .with_main_axis_size(MainAxisSize::Min)
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_spacing(6.)
                        .with_child(icon(WarpIcon::FileCopy, color, sizes.small_icon_size))
                        .with_child(look.text(label.clone(), sizes.small_font_size, palette.text.0))
                        .finish()
                })
                .with_cursor(Cursor::PointingHand)
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(ComposerAction::ExpandPasted(index))
                })
                .finish()
            };
            let content = Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(6.)
                .with_child(open)
                .with_child(self.render_remove_button(
                    look,
                    pasted.remove.clone(),
                    ComposerAction::RemovePasted(index),
                ))
                .finish();
            row.add_child(
                Container::new(
                    ConstrainedBox::new(content)
                        .with_height(sizes.chip_height + 4.)
                        .finish(),
                )
                .with_padding_left(10.)
                .with_padding_right(6.)
                .with_background_color(palette.surface_hover.0)
                .with_border(Border::all(1.).with_border_color(palette.border.0))
                .with_corner_radius(look.radius(sizes.chip_radius + 2.))
                .finish(),
            );
        }
        row.finish()
    }

    fn render_thumbnails(&self, look: &Look) -> Box<dyn Element> {
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
                Container::new(look.text(
                    attachment.number.to_string(),
                    sizes.tiny_font_size,
                    palette.text.0,
                ))
                .with_horizontal_padding(5.)
                .with_vertical_padding(1.)
                .with_background_color(with_alpha(palette.page.0, 0xd9))
                .with_corner_radius(look.radius(6.))
                .finish(),
                OffsetPositioning::offset_from_parent(
                    vec2f(4., -4.),
                    warpui::elements::ParentOffsetBounds::Unbounded,
                    warpui::elements::ParentAnchor::BottomLeft,
                    ChildAnchor::BottomLeft,
                ),
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
        let mode_chip = || {
            SavePosition::new(
                widgets::footer_control(
                    look,
                    self.mouse.mode.clone(),
                    Some((mode_icon, mode_color)),
                    vec![(mode.label.to_owned(), mode_color)],
                    ComposerAction::ToggleModeMenu,
                ),
                &self.position_id("mode_chip"),
            )
            .finish()
        };

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
        let model_chip = || {
            SavePosition::new(
                widgets::footer_control(
                    look,
                    self.mouse.model.clone(),
                    None,
                    parts.clone(),
                    ComposerAction::TogglePicker,
                ),
                &self.position_id("model_chip"),
            )
            .finish()
        };

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
        let ring = || self.render_context_ring(look, fraction, ring_label.clone());
        let git_chip = || {
            (!self.git.is_empty()).then(|| {
                SavePosition::new(self.render_git_chip(look), &self.position_id("git_chip"))
                    .finish()
            })
        };

        let right = || {
            Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(2.)
                .with_child(model_chip())
                .with_child(ring())
                .finish()
        };
        let mut left = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(2.)
            .with_child(mode_chip());
        left.extend(git_chip());
        let inline = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(Shrinkable::new(1., left.finish()).finish())
            .with_child(right())
            .finish();

        // When the branch chip doesn't fit beside the others (a long branch, a workspace's
        // "+N repos"), it moves to its own line under the mode instead of pushing the model past
        // the pane's edge.
        let mut stacked = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_spacing(2.)
            .with_child(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(mode_chip())
                    .with_child(right())
                    .finish(),
            );
        stacked.extend(git_chip());
        let inline_width = self.footer_inline_width(look, mode.label, &parts);

        // In narrow panes the chips flow onto more lines instead of running over each other.
        let mut wrapped = warpui::elements::Wrap::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(2.)
            .with_run_spacing(2.);
        wrapped.extend(
            std::iter::once(mode_chip())
                .chain(git_chip())
                .chain([model_chip(), ring()]),
        );

        Container::new(
            Clipped::new(
                warpui::elements::SizeConstraintSwitch::new(
                    inline,
                    [
                        (
                            warpui::elements::SizeConstraintCondition::WidthLessThan(
                                FOOTER_WRAP_WIDTH,
                            ),
                            wrapped.finish(),
                        ),
                        (
                            warpui::elements::SizeConstraintCondition::WidthLessThan(inline_width),
                            stacked.finish(),
                        ),
                    ],
                )
                .finish(),
            )
            .finish(),
        )
        .with_padding_top(6.)
        .finish()
    }

    /// Roughly how wide the footer's chips are side by side, to tell when the branch chip has to
    /// move to its own line.
    fn footer_inline_width(
        &self,
        look: &Look,
        mode: &str,
        model_parts: &[(String, ColorU)],
    ) -> f32 {
        let size = look.sizes.small_font_size;
        let glyphs = |text: &str| text.chars().count() as f32 * size * FOOTER_GLYPH_WIDTH_RATIO;
        let icon = look.sizes.small_icon_size + 5.;
        let mode_width = FOOTER_CHIP_PADDING + icon + glyphs(mode) + icon;
        let model_width = FOOTER_CHIP_PADDING
            + model_parts
                .iter()
                .map(|(text, _)| glyphs(text) + 5.)
                .sum::<f32>()
            + icon
            + FOOTER_RING_WIDTH;
        let git_width = self.git.first().map_or(0., |status| {
            let (added, removed) = git_status::total_changes(&self.git);
            let (added, removed) = git_status::diff_parts(added, removed);
            let other_repos = self.git.len().saturating_sub(1);
            let mut width =
                FOOTER_CHIP_PADDING + icon + glyphs(&status.branch).min(GIT_BRANCH_MAX_WIDTH);
            if other_repos > 0 {
                width += glyphs(&format!("+{other_repos} repos")) + 5. + icon;
            }
            for part in added.iter().chain(removed.iter()) {
                width += glyphs(part) + 5.;
            }
            width
        });
        mode_width + git_width + model_width + FOOTER_SPACING_SLACK
    }

    /// The branch of the chat's folder and the uncommitted changes of every repository it works
    /// in; a click shows the diff, or lists the repositories when there are several.
    fn render_git_chip(&self, look: &Look) -> Box<dyn Element> {
        let look = look.clone();
        let branch = self
            .git
            .first()
            .map(|status| status.branch.clone())
            .unwrap_or_default();
        let other_repos = self.git.len().saturating_sub(1);
        let (added, removed) = git_status::total_changes(&self.git);
        let (added, removed) = git_status::diff_parts(added, removed);
        Hoverable::new(self.mouse.git.clone(), move |state| {
            let palette = &look.palette;
            let sizes = &look.sizes;
            let hovered = state.is_hovered();
            let mut row = Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(5.)
                .with_child(icon(
                    WarpIcon::GitBranch,
                    palette.text_faint.0,
                    sizes.small_icon_size,
                ))
                .with_child(
                    ConstrainedBox::new(
                        Text::new_inline(branch.clone(), look.ui_font, sizes.small_font_size)
                            .with_color(if hovered {
                                palette.text.0
                            } else {
                                palette.text_muted.0
                            })
                            .with_clip(ClipConfig::ellipsis())
                            .finish(),
                    )
                    .with_max_width(GIT_BRANCH_MAX_WIDTH)
                    .finish(),
                );
            if other_repos > 0 {
                row.add_child(look.text(
                    format!(
                        "+{other_repos} {}",
                        if other_repos == 1 { "repo" } else { "repos" }
                    ),
                    sizes.small_font_size,
                    palette.text_faint.0,
                ));
            }
            if let Some(added) = &added {
                row.add_child(look.medium_text(
                    added.clone(),
                    sizes.small_font_size,
                    palette.diff_added.0,
                ));
            }
            if let Some(removed) = &removed {
                row.add_child(look.medium_text(
                    removed.clone(),
                    sizes.small_font_size,
                    palette.diff_removed.0,
                ));
            }
            if other_repos > 0 {
                row.add_child(icon(
                    WarpIcon::ChevronDown,
                    palette.text_faint.0,
                    sizes.small_icon_size - 3.,
                ));
            }
            let mut container = Container::new(
                ConstrainedBox::new(row.finish())
                    .with_height(sizes.chip_height - 2.)
                    .finish(),
            )
            .with_horizontal_padding(8.)
            .with_corner_radius(look.radius(sizes.chip_radius));
            if hovered {
                container = container.with_background_color(palette.surface_hover.0);
            }
            container.finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| ctx.dispatch_typed_action(ComposerAction::OpenDiff))
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

/// Reads what an attachment at `path` needs; runs off the main thread.
fn load_path(path: String) -> Option<LoadedPath> {
    let metadata = std::fs::metadata(&path).ok()?;
    let kind = attachments::classify(&path, metadata.is_dir());
    let is_text = match kind {
        AttachmentKind::Folder
        | AttachmentKind::Image
        | AttachmentKind::Pdf
        | AttachmentKind::Document
        | AttachmentKind::Presentation => false,
        AttachmentKind::Text | AttachmentKind::Spreadsheet => {
            attachments::has_text_extension(&path)
        }
        AttachmentKind::Other => sniffs_as_text(Path::new(&path)),
    };
    let delivery = attachments::delivery(kind, metadata.len(), is_text);
    let document = match delivery {
        FileDelivery::Image => {
            let media_type = attachments::image_media_type(&path)?.to_owned();
            let bytes = std::fs::read(&path).ok()?;
            return Some(LoadedPath::Image { media_type, bytes });
        }
        FileDelivery::Document => {
            let bytes = std::fs::read(&path).ok()?;
            Some(OutgoingDocument {
                media_type: "application/pdf".to_owned(),
                data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
                title: file_name(&path),
            })
        }
        FileDelivery::Mention | FileDelivery::Path => None,
    };
    Some(LoadedPath::File(FileChip {
        name: file_name(&path).unwrap_or_else(|| path.clone()),
        path,
        kind,
        key: 0,
        number: 0,
        delivery,
        document,
        remove: Default::default(),
    }))
}

/// The characters before and after the cursor at `row` and `column` (in characters) of `text`.
fn chars_around(text: &str, row: usize, column: usize) -> (Option<char>, Option<char>) {
    let mut lines = text.split('\n').skip(row);
    let Some(line) = lines.next() else {
        return (None, None);
    };
    let before = column
        .checked_sub(1)
        .and_then(|index| line.chars().nth(index));
    let after = line
        .chars()
        .nth(column)
        .or_else(|| lines.next().map(|_| '\n'));
    (before, after)
}

fn file_name(path: &str) -> Option<String> {
    Path::new(path.trim_end_matches('/'))
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
}

/// Whether the file's first bytes look like text, for files whose extension doesn't say.
fn sniffs_as_text(path: &Path) -> bool {
    use std::io::Read as _;
    let mut head = [0u8; 8192];
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(read) = file.read(&mut head) else {
        return false;
    };
    attachments::looks_like_text(&head[..read])
}

fn attachment_icon(kind: AttachmentKind) -> WarpIcon {
    match kind {
        AttachmentKind::Image => WarpIcon::Image,
        AttachmentKind::Pdf | AttachmentKind::Document => WarpIcon::File,
        AttachmentKind::Text => WarpIcon::Code2,
        AttachmentKind::Spreadsheet => WarpIcon::Grid,
        AttachmentKind::Presentation => WarpIcon::LayoutAlt01,
        AttachmentKind::Folder => WarpIcon::Folder,
        AttachmentKind::Other => WarpIcon::Paperclip,
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
        if self.is_git_menu_open && self.git.len() > 1 {
            stack.add_positioned_overlay_child(
                self.render_git_menu(&look),
                OffsetPositioning::offset_from_save_position_element(
                    self.position_id("git_chip"),
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
            ComposerAction::AttachFiles => self.pick_files(ctx),
            ComposerAction::RemoveImage(index) => {
                if let Some(key) = self.images.get(*index).map(|image| image.key) {
                    self.remove_attachment(key, ctx);
                }
            }
            ComposerAction::RemoveFile(index) => {
                if let Some(key) = self.files.get(*index).map(|file| file.key) {
                    self.remove_attachment(key, ctx);
                }
            }
            ComposerAction::OpenDiff => match self.git.as_slice() {
                [] => {}
                [status] => ctx.emit(ComposerEvent::OpenDiff {
                    repo_root: status.repo_root.clone(),
                }),
                [_, _, ..] => {
                    if self.is_git_menu_open {
                        self.close_popovers(ctx);
                    } else if !self.just_closed_popovers() {
                        self.picker = None;
                        self.is_mode_menu_open = false;
                        self.is_artifact_kind_menu_open = false;
                        self.is_git_menu_open = true;
                        ctx.notify();
                    }
                }
            },
            ComposerAction::OpenRepoDiff(index) => {
                if let Some(status) = self.git.get(*index) {
                    ctx.emit(ComposerEvent::OpenDiff {
                        repo_root: status.repo_root.clone(),
                    });
                }
                self.close_popovers(ctx);
            }
            ComposerAction::RemoveMention(index) => {
                if let Some(key) = self.mentions.get(*index).map(|mention| mention.key) {
                    self.remove_attachment(key, ctx);
                }
            }
            ComposerAction::RemovePasted(index) => {
                if let Some(key) = self.pasted.get(*index).map(|pasted| pasted.key) {
                    self.remove_attachment(key, ctx);
                }
            }
            ComposerAction::ExpandPasted(index) => self.expand_pasted(*index, ctx),
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
            ComposerAction::BrowseProvider(provider) => self.browse_provider(*provider, ctx),
            ComposerAction::ChooseProviderModel { provider, index } => {
                self.choose_provider_model(*provider, *index, ctx)
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

/// The height of an attachment's token in an editor whose lines are `line_height_ratio` font sizes
/// tall, as a percentage of the font size. It keeps clear of the tokens of the lines around and
/// tall enough to hold its own text.
fn token_height_percent(line_height_ratio: f32) -> u8 {
    ((line_height_ratio - TOKEN_LINE_GAP).clamp(1.2, 1.4) * 100.).round() as u8
}

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
