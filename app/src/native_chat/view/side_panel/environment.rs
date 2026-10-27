//! "Ambiente": the machine and folder Claude runs in, and Remote Control.

use claude_chat::reducer::ConversationStatus;
use warpui::AppContext;
use warpui::elements::{
    Container, CrossAxisAlignment, Element, Empty, Expanded, Flex, MainAxisSize, ParentElement,
    Shrinkable,
};

use super::format::abbreviate_home;
use super::widgets::{self, ICON_GAP, Look, ROW_PADDING_X};
use super::{SidePanelAction, SidePanelSection, SidePanelView};
use crate::native_chat::session::ProcessState;

const LAPTOP_ICON: &str = "bundled/svg/picolas-laptop.svg";
const FOLDER_ICON: &str = "bundled/svg/picolas-folder.svg";
const FOLDER_OPEN_ICON: &str = "bundled/svg/picolas-folder-open.svg";
const PHONE_ICON: &str = "bundled/svg/picolas-phone.svg";
const EXTERNAL_ICON: &str = "bundled/svg/picolas-external.svg";
const COPY_ICON: &str = "bundled/svg/picolas-copy.svg";
const CHECK_ICON: &str = "bundled/svg/picolas-check.svg";

impl SidePanelView {
    pub(super) fn render_environment(&self, look: Look<'_>, app: &AppContext) -> Box<dyn Element> {
        let section = SidePanelSection::Environment;
        let is_expanded = self.is_expanded(section);
        let mut children = vec![widgets::card_header(
            "Ambiente",
            is_expanded,
            None,
            self.mouse("header:environment"),
            SidePanelAction::ToggleSection(section),
            look,
        )];
        if !is_expanded {
            return widgets::card(children);
        }

        let palette = look.palette;
        let icon_size = look.sizes.icon_size;
        let session = self.session.as_ref(app);
        children.push(widgets::plain_row(
            widgets::row_content(
                Some(widgets::svg(LAPTOP_ICON, palette.text_muted.0, icon_size)),
                look.label("Este computador", palette.text.0),
                None,
            ),
            look,
        ));

        let home = dirs::home_dir();
        let folder = abbreviate_home(session.cwd(), home.as_deref());
        let folder_label = widgets::path_label(
            folder,
            look.sizes.small_font_size,
            palette.text_muted.0,
            look,
        );
        children.push(widgets::plain_row(
            widgets::row_content(
                Some(widgets::svg(FOLDER_ICON, palette.text_muted.0, icon_size)),
                folder_label,
                None,
            ),
            look,
        ));

        if !session.has_transcript() {
            children.push(widgets::clickable_row(
                self.mouse("pick-folder"),
                SidePanelAction::PickFolder,
                look,
                move |is_hovered| {
                    let color = if is_hovered {
                        palette.text.0
                    } else {
                        palette.text_muted.0
                    };
                    widgets::row_content(
                        Some(widgets::svg(FOLDER_OPEN_ICON, color, icon_size)),
                        look.label("Escolher pasta…", color),
                        None,
                    )
                },
            ));
        }

        children.extend(self.render_remote_control(look, app));
        widgets::card(children)
    }

    fn render_remote_control(&self, look: Look<'_>, app: &AppContext) -> Vec<Box<dyn Element>> {
        let palette = look.palette;
        let icon_size = look.sizes.icon_size;
        let session = self.session.as_ref(app);
        let conversation = session.conversation();
        let remote = conversation.remote_control();
        let is_on = remote.enabled;
        let is_available = remote.available || is_on;

        let toggle_row = |is_hovered: bool| {
            let color = if is_available {
                palette.text.0
            } else {
                palette.text_faint.0
            };
            let icon_color = if is_hovered {
                palette.text.0
            } else if is_available {
                palette.text_muted.0
            } else {
                palette.text_faint.0
            };
            widgets::row_content(
                Some(widgets::svg(PHONE_ICON, icon_color, icon_size)),
                look.label("Remote Control", color),
                Some(widgets::switch(is_on, is_available, look)),
            )
        };
        let mut rows = vec![if is_available {
            widgets::clickable_row(
                self.mouse("remote-control"),
                SidePanelAction::SetRemoteControl(!is_on),
                look,
                toggle_row,
            )
        } else {
            widgets::plain_row(toggle_row(false), look)
        }];

        // The CLI only reports availability once it has started.
        let has_started = *session.process_state() == ProcessState::Running
            && conversation.status() != ConversationStatus::Starting;
        let detail = if is_on {
            Some(match &remote.session_url {
                Some(url) => self.render_remote_link(url, look),
                None => look.meta("Conectando…", palette.text_faint.0),
            })
        } else if !is_available && has_started {
            Some(look.meta("Requer login com uma conta claude.ai", palette.text_faint.0))
        } else {
            None
        };
        if let Some(detail) = detail {
            rows.push(
                Container::new(detail)
                    .with_padding_left(ROW_PADDING_X + icon_size + ICON_GAP)
                    .with_padding_right(ROW_PADDING_X - 4.)
                    .with_padding_bottom(4.)
                    .finish(),
            );
        }
        rows
    }

    /// "Abrir no claude.ai ↗" and a copy button for the session's Remote Control page.
    fn render_remote_link(&self, url: &str, look: Look<'_>) -> Box<dyn Element> {
        let palette = look.palette;
        let link = widgets::text_link(
            "Abrir no claude.ai",
            palette.link.0,
            self.mouse("remote-open"),
            SidePanelAction::OpenUrl(url.to_owned()),
            look,
        );
        let copy_icon = if self.copied { CHECK_ICON } else { COPY_ICON };
        let copy = widgets::icon_button(
            copy_icon,
            look.sizes.small_icon_size - 1.,
            self.mouse("remote-copy"),
            SidePanelAction::Copy(url.to_owned()),
            look,
        );
        Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(4.)
            .with_child(Shrinkable::new(1., link).finish())
            .with_child(widgets::svg(
                EXTERNAL_ICON,
                palette.link.0,
                look.sizes.small_icon_size - 2.,
            ))
            .with_child(Expanded::new(1., Empty::new().finish()).finish())
            .with_child(copy)
            .finish()
    }
}
