//! Checks for a newer Claude Code at startup and every few hours, offers it in the notifications
//! mailbox, and installs it only when the user asks.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use claude_chat::process;
use claude_chat::update::{self, UpdateMethod, Version};
use warpui::r#async::Timer;
use warpui::{Entity, ModelContext, SingletonEntity};

use crate::ai::agent_management::AgentNotificationsModel;
use crate::ai::agent_management::notifications::ClaudeCodeUpdateNotice;
use crate::terminal::local_shell::LocalShellState;
use crate::view_components::DismissibleToast;
use crate::workspace::{ToastStack, WorkspaceAction};

/// Leaves startup alone before the first check.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(20);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const DEV_UPDATE_DURATION: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
struct AvailableUpdate {
    installed: Version,
    latest: Version,
}

pub struct ClaudeCodeUpdates {
    available: Option<AvailableUpdate>,
    is_updating: bool,
    /// The latest version the user turned down or that `claude update` couldn't reach; it isn't
    /// offered again.
    dismissed: Option<Version>,
    /// `PICOLAS_FAKE_CLAUDE_UPDATE` offers a made-up update and pretends to install it.
    is_dev_fake: bool,
}

impl Entity for ClaudeCodeUpdates {
    type Event = ();
}

impl SingletonEntity for ClaudeCodeUpdates {}

impl ClaudeCodeUpdates {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let mut me = Self {
            available: None,
            is_updating: false,
            dismissed: None,
            is_dev_fake: cfg!(debug_assertions)
                && std::env::var_os("PICOLAS_FAKE_CLAUDE_UPDATE").is_some(),
        };
        if me.is_dev_fake {
            me.on_checked(Version::parse("2.1.291"), Version::parse("2.1.300"), ctx);
        } else {
            ctx.spawn(Timer::after(FIRST_CHECK_DELAY), |me, _, ctx| me.check(ctx));
        }
        me
    }

    /// Installs the latest version, reporting progress and the result in toasts.
    pub fn start_update(&mut self, ctx: &mut ModelContext<Self>) {
        let Some(available) = self.available.clone() else {
            return;
        };
        if self.is_updating {
            return;
        }
        self.is_updating = true;
        self.publish(ctx);
        show_toast(
            DismissibleToast::default(format!(
                "Atualizando o Claude Code para {}…",
                available.latest
            )),
            ctx,
        );
        if self.is_dev_fake {
            let latest = available.latest.clone();
            ctx.spawn(Timer::after(DEV_UPDATE_DURATION), move |me, _, ctx| {
                me.on_updated(available, Ok(latest), ctx)
            });
            return;
        }
        let tools = find_tools(ctx);
        ctx.spawn(
            async move {
                let (Some(claude), path_env) = tools.await else {
                    return Err("o Claude Code não foi encontrado".to_owned());
                };
                let path_env = path_env.as_deref();
                if let Err(update_error) =
                    update::run_update(UpdateMethod::ClaudeUpdate, &claude, path_env).await
                {
                    log::warn!("`claude update` failed: {update_error}");
                    update::run_update(UpdateMethod::Npm, &claude, path_env)
                        .await
                        .map_err(|npm_error| {
                            log::warn!("Updating Claude Code with npm failed: {npm_error}");
                            update_error
                        })?;
                }
                let claude = process::find_claude(path_env).unwrap_or(claude);
                update::installed_version(&claude, path_env)
                    .await
                    .ok_or_else(|| "não foi possível ler a versão instalada".to_owned())
            },
            move |me, result, ctx| me.on_updated(available, result, ctx),
        );
    }

    /// Hides the offer until an even newer version comes out.
    pub fn dismiss(&mut self, ctx: &mut ModelContext<Self>) {
        if self.is_updating {
            return;
        }
        if let Some(available) = self.available.take() {
            self.dismissed = Some(available.latest);
            self.publish(ctx);
        }
    }

    fn check(&mut self, ctx: &mut ModelContext<Self>) {
        ctx.spawn(Timer::after(CHECK_INTERVAL), |me, _, ctx| me.check(ctx));
        if self.is_updating {
            return;
        }
        let tools = find_tools(ctx);
        ctx.spawn(
            async move {
                let (Some(claude), path_env) = tools.await else {
                    return (None, None);
                };
                let Some(installed) = update::installed_version(&claude, path_env.as_deref()).await
                else {
                    return (None, None);
                };
                (Some(installed), fetch_latest_version().await)
            },
            |me, (installed, latest), ctx| me.on_checked(installed, latest, ctx),
        );
    }

    fn on_checked(
        &mut self,
        installed: Option<Version>,
        latest: Option<Version>,
        ctx: &mut ModelContext<Self>,
    ) {
        let (Some(installed), Some(latest)) = (installed, latest) else {
            log::info!("Couldn't check for a Claude Code update (offline or not installed)");
            return;
        };
        let is_offered = latest > installed && self.dismissed.as_ref() != Some(&latest);
        self.available = is_offered.then_some(AvailableUpdate { installed, latest });
        self.publish(ctx);
    }

    fn on_updated(
        &mut self,
        available: AvailableUpdate,
        result: Result<Version, String>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.is_updating = false;
        let toast = match result {
            Ok(installed) if installed > available.installed => {
                self.available = None;
                if installed < available.latest {
                    self.dismissed = Some(available.latest);
                }
                DismissibleToast::success(format!(
                    "Claude Code atualizado para {installed}. Novas conversas já usam a nova \
                     versão."
                ))
            }
            Ok(installed) => {
                // `claude update` follows its release channel, which can lag the npm registry.
                self.available = None;
                self.dismissed = Some(available.latest);
                DismissibleToast::default(format!(
                    "O Claude Code continua na versão {installed}: não há versão mais nova no seu \
                     canal de atualização."
                ))
            }
            Err(error) => DismissibleToast::error(format!(
                "Não foi possível atualizar o Claude Code: {error}"
            )),
        };
        self.publish(ctx);
        show_toast(toast, ctx);
    }

    /// Mirrors the offer into the notifications mailbox.
    fn publish(&self, ctx: &mut ModelContext<Self>) {
        let notice = self
            .available
            .as_ref()
            .map(|available| ClaudeCodeUpdateNotice {
                installed: available.installed.to_string(),
                latest: available.latest.to_string(),
                is_updating: self.is_updating,
                is_read: false,
            });
        AgentNotificationsModel::handle(ctx).update(ctx, |notifications, ctx| {
            notifications.set_claude_code_update(notice, ctx)
        });
    }
}

/// The `claude` executable and the login shell's `PATH` it runs with.
fn find_tools(
    ctx: &mut ModelContext<ClaudeCodeUpdates>,
) -> impl Future<Output = (Option<PathBuf>, Option<String>)> + Send + 'static {
    let path_future = LocalShellState::handle(ctx)
        .update(ctx, |shell, ctx| shell.get_interactive_path_env_var(ctx));
    async move {
        let path_env = path_future.await.or_else(|| std::env::var("PATH").ok());
        (process::find_claude(path_env.as_deref()), path_env)
    }
}

async fn fetch_latest_version() -> Option<Version> {
    let client = http_client::Client::new();
    let response = client
        .get(update::LATEST_VERSION_URL)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .ok()?;
    update::parse_registry_latest(&response.text().await.ok()?)
}

fn show_toast(toast: DismissibleToast<WorkspaceAction>, ctx: &mut ModelContext<ClaudeCodeUpdates>) {
    let window_id = ctx
        .windows()
        .active_window()
        .or_else(|| ctx.windows().ordered_window_ids().first().copied());
    let Some(window_id) = window_id else {
        return;
    };
    ToastStack::handle(ctx).update(ctx, |toasts, ctx| {
        toasts.add_ephemeral_toast(toast, window_id, ctx)
    });
}

/// `PICOLAS_OPEN_NOTIFICATIONS` opens the notifications mailbox once the first window settles.
/// Debug builds only.
pub fn take_dev_open_notifications_action() -> Option<WorkspaceAction> {
    use std::sync::atomic::{AtomicBool, Ordering};

    static TAKEN: AtomicBool = AtomicBool::new(false);
    if !cfg!(debug_assertions) || TAKEN.swap(true, Ordering::Relaxed) {
        return None;
    }
    std::env::var_os("PICOLAS_OPEN_NOTIFICATIONS")?;
    Some(WorkspaceAction::ToggleNotificationMailbox {
        select_first: false,
    })
}
