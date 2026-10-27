//! "Conectores": the MCP servers, their status, sign-in and on/off switch.

use claude_chat::artifacts;
use claude_chat::protocol::McpServerStatus;
use warpui::AppContext;
use warpui::elements::{
    Align, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element, Flex, Hoverable,
    MainAxisSize, ParentElement, Radius,
};
use warpui::fonts::Weight;

use super::format::{ServerState, avatar_letter, display_server_name};
use super::widgets::{self, Look};
use super::{SidePanelAction, SidePanelSection, SidePanelView};
use crate::native_chat::session::ProcessState;

const RETRY_ICON: &str = "bundled/svg/picolas-reset.svg";
/// Connectors listed before "Mostrar mais".
const VISIBLE_CONNECTORS: usize = 6;
const AVATAR_SIZE: f32 = 20.;
const AVATAR_RADIUS: f32 = 6.;
const STATUS_DOT_SIZE: f32 = 6.;

impl SidePanelView {
    pub(super) fn render_connectors(&self, look: Look<'_>, app: &AppContext) -> Box<dyn Element> {
        let palette = look.palette;
        let section = SidePanelSection::Connectors;
        let is_expanded = self.is_expanded(section);
        let session = self.session.as_ref(app);
        let mut servers: Vec<&McpServerStatus> = session
            .conversation()
            .mcp_servers()
            .iter()
            .filter(|server| server.name != artifacts::SERVER_NAME)
            .collect();
        servers.sort_by_key(|server| ServerState::from_status(&server.status).rank());

        let count = (!servers.is_empty())
            .then(|| look.meta(servers.len().to_string(), palette.text_faint.0));
        let mut children = vec![widgets::card_header(
            "Conectores",
            is_expanded,
            count,
            self.mouse("header:connectors"),
            SidePanelAction::ToggleSection(section),
            look,
        )];
        if !is_expanded {
            return widgets::card(children);
        }

        if servers.is_empty() {
            let note = match session.process_state() {
                ProcessState::Starting | ProcessState::Running
                    if session.conversation().mcp_servers().is_empty() =>
                {
                    "Carregando conectores…"
                }
                ProcessState::Starting | ProcessState::Running | ProcessState::Exited { .. } => {
                    "Nenhum conector configurado"
                }
            };
            children.push(widgets::plain_row(
                look.meta(note, palette.text_faint.0),
                look,
            ));
            return widgets::card(children);
        }

        let visible = if self.show_all_connectors {
            servers.len()
        } else {
            VISIBLE_CONNECTORS
        };
        children.extend(
            servers
                .iter()
                .take(visible)
                .map(|server| self.render_server(server, look)),
        );
        if servers.len() > VISIBLE_CONNECTORS {
            let label = if self.show_all_connectors {
                "Mostrar menos".to_owned()
            } else {
                format!("Mostrar mais ({})", servers.len() - VISIBLE_CONNECTORS)
            };
            children.push(widgets::clickable_row(
                self.mouse("connectors:more"),
                SidePanelAction::ToggleAllConnectors,
                look,
                move |is_hovered| {
                    let color = if is_hovered {
                        palette.text.0
                    } else {
                        palette.text_muted.0
                    };
                    look.meta(label, color)
                },
            ));
        }
        widgets::card(children)
    }

    fn render_server(&self, server: &McpServerStatus, look: Look<'_>) -> Box<dyn Element> {
        let palette = look.palette;
        let state = ServerState::from_status(&server.status);
        let name = display_server_name(&server.name);
        let is_on = state != ServerState::Disabled;
        let key = &server.name;
        let switch_mouse = self.mouse(format!("server-switch:{key}"));
        let action_mouse = self.mouse(format!("server-action:{key}"));
        let server_name = server.name.clone();

        Hoverable::new(self.mouse(format!("server:{key}")), move |hover| {
            let is_hovered = hover.is_hovered();
            let name_color = if is_on {
                palette.text.0
            } else {
                palette.text_faint.0
            };
            let mut trailing = Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(8.);
            match state {
                ServerState::NeedsAuth => trailing.add_child(widgets::text_link(
                    "Autenticar",
                    palette.accent.0,
                    action_mouse,
                    SidePanelAction::AuthenticateServer(server_name.clone()),
                    look,
                )),
                ServerState::Failed if is_hovered => trailing.add_child(widgets::icon_button(
                    RETRY_ICON,
                    look.sizes.small_icon_size - 1.,
                    action_mouse,
                    SidePanelAction::ReconnectServer(server_name.clone()),
                    look,
                )),
                ServerState::Failed => {
                    trailing.add_child(look.meta("Falhou", palette.error.0));
                }
                ServerState::Connected if !is_hovered => trailing.add_child(widgets::swatch(
                    palette.success.0,
                    STATUS_DOT_SIZE,
                    STATUS_DOT_SIZE,
                    STATUS_DOT_SIZE / 2.,
                )),
                ServerState::Pending if !is_hovered => {
                    trailing.add_child(look.meta("Conectando…", palette.text_faint.0));
                }
                ServerState::Disabled if !is_hovered => {
                    trailing.add_child(look.meta("Desativado", palette.text_faint.0));
                }
                ServerState::Connected | ServerState::Pending | ServerState::Disabled => {}
            }
            if is_hovered {
                trailing.add_child(widgets::switch_button(
                    is_on,
                    true,
                    switch_mouse,
                    SidePanelAction::SetServerEnabled {
                        name: server_name,
                        enabled: !is_on,
                    },
                    look,
                ));
            }
            let row = widgets::row_content(
                Some(render_avatar(&name, is_on, look)),
                look.label(name.clone(), name_color),
                Some(trailing.finish()),
            );
            widgets::row_container(row, is_hovered, look)
        })
        .finish()
    }
}

/// A rounded square with the connector's initial.
fn render_avatar(name: &str, is_on: bool, look: Look<'_>) -> Box<dyn Element> {
    let palette = look.palette;
    let color = if is_on {
        palette.text.0
    } else {
        palette.text_faint.0
    };
    ConstrainedBox::new(
        Container::new(
            Align::new(look.text(
                avatar_letter(name),
                look.sizes.tiny_font_size,
                color,
                Weight::Semibold,
            ))
            .finish(),
        )
        .with_background_color(palette.surface_strong.0)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(AVATAR_RADIUS)))
        .finish(),
    )
    .with_width(AVATAR_SIZE)
    .with_height(AVATAR_SIZE)
    .finish()
}
