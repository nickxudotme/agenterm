use std::sync::Arc;

use parking_lot::FairMutex;
use warpui::elements::{
    ChildView, Container, CrossAxisAlignment, Expanded, Flex, MainAxisSize, ParentElement,
};
use warpui::prelude::Empty;
use warpui::{AppContext, Element, Entity, TypedActionView, View, ViewContext, ViewHandle};

use super::{AgentFooterButtonTheme, USE_AGENT_KEYSTROKE};
use crate::terminal::view::{PADDING_LEFT, TerminalModel};
use crate::terminal::warpify::trigger_state::SshWarpifyOffer;
use crate::ui_components::icons::Icon;
use crate::view_components::action_button::{
    ActionButton, ButtonSize, KeystrokeSource, TooltipAlignment,
};

/// Footer view rendered for detected subshell commands, offering both
/// "Warpify" and "Use agent" buttons in a horizontal row.
pub(super) struct WarpifyFooterView {
    terminal_model: Arc<FairMutex<TerminalModel>>,
    warpify_button: ViewHandle<ActionButton>,
    use_agent_button: ViewHandle<ActionButton>,
    dismiss_button: ViewHandle<ActionButton>,
    /// Whether the footer is currently offering subshell warpification.
    is_active: bool,
    ssh_offer: Option<SshWarpifyOffer>,
}

impl WarpifyFooterView {
    pub fn new(terminal_model: Arc<FairMutex<TerminalModel>>, ctx: &mut ViewContext<Self>) -> Self {
        let button_size = ButtonSize::XSmall;

        let warpify_button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new("Warpify subshell", AgentFooterButtonTheme::new(None))
                .with_icon(Icon::Warp)
                .with_size(button_size)
                .with_tooltip("Enable Warp shell integration in this session")
                .with_tooltip_alignment(TooltipAlignment::Left)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(WarpifyFooterViewAction::Warpify);
                })
        });

        let use_agent_button = ctx.add_typed_action_view(|ctx| {
            ActionButton::new("Use agent", AgentFooterButtonTheme::new(None))
                .with_icon(Icon::Agent)
                .with_keybinding(KeystrokeSource::Fixed(USE_AGENT_KEYSTROKE.clone()), ctx)
                .with_size(button_size)
                .with_tooltip("Ask the Warp agent to assist")
                .with_tooltip_alignment(TooltipAlignment::Left)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(WarpifyFooterViewAction::UseAgent);
                })
        });

        let dismiss_button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new("Dismiss", AgentFooterButtonTheme::new(None))
                .with_size(button_size)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(WarpifyFooterViewAction::Dismiss);
                })
        });

        Self {
            terminal_model,
            warpify_button,
            use_agent_button,
            dismiss_button,
            is_active: false,
            ssh_offer: None,
        }
    }

    /// Activates the footer so it offers subshell warpification.
    pub fn show(&mut self, ctx: &mut ViewContext<Self>) {
        self.clear(ctx);
        self.warpify_button.update(ctx, |button, ctx| {
            button.set_keybinding(
                Some(KeystrokeSource::Binding("terminal:warpify_subshell")),
                ctx,
            );
        });
        self.is_active = true;
        ctx.notify();
    }

    pub fn show_ssh(&mut self, offer: SshWarpifyOffer, ctx: &mut ViewContext<Self>) {
        self.show(ctx);
        self.ssh_offer = Some(offer.clone());
        self.warpify_button.update(ctx, |button, ctx| {
            button.set_label("Confirm target shell and Warpify SSH", ctx);
            button.set_on_click(
                move |ctx| {
                    ctx.dispatch_typed_action(WarpifyFooterViewAction::WarpifySsh(offer.clone()));
                },
                ctx,
            );
        });
    }

    /// Returns whether the footer is currently offering subshell warpification.
    pub fn is_active(&self) -> bool {
        self.is_active
    }

    /// Deactivates the footer.
    pub fn clear(&mut self, ctx: &mut ViewContext<Self>) {
        self.is_active = false;
        self.ssh_offer = None;
        self.warpify_button.update(ctx, |button, ctx| {
            button.set_keybinding(None, ctx);
            button.set_label("Warpify subshell", ctx);
            button.set_on_click(
                |ctx| ctx.dispatch_typed_action(WarpifyFooterViewAction::Warpify),
                ctx,
            );
        });
        ctx.notify();
    }
}

#[derive(Debug, Clone)]
pub enum WarpifyFooterViewAction {
    Warpify,
    WarpifySsh(SshWarpifyOffer),
    UseAgent,
    Dismiss,
}

pub enum WarpifyFooterViewEvent {
    Warpify,
    WarpifySsh(SshWarpifyOffer),
    UseAgent,
    Dismiss,
}

impl Entity for WarpifyFooterView {
    type Event = WarpifyFooterViewEvent;
}

impl View for WarpifyFooterView {
    fn ui_name() -> &'static str {
        "WarpifyFooterView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        let terminal_model = self.terminal_model.lock();

        let mut button_row = Flex::row()
            .with_spacing(4.)
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(ChildView::new(&self.warpify_button).finish());
        if self.ssh_offer.is_none() {
            button_row = button_row.with_child(ChildView::new(&self.use_agent_button).finish());
        }
        let button_row = button_row
            .with_child(Expanded::new(1., Empty::new().finish()).finish())
            .with_child(ChildView::new(&self.dismiss_button).finish());

        let mut container = Container::new(button_row.finish())
            .with_horizontal_padding(*PADDING_LEFT)
            .with_vertical_padding(4.);

        if terminal_model.is_alt_screen_active()
            && let Some(bg_color) = terminal_model.alt_screen().inferred_bg_color()
        {
            container = container.with_background(bg_color);
        }

        container.finish()
    }
}

impl TypedActionView for WarpifyFooterView {
    type Action = WarpifyFooterViewAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            WarpifyFooterViewAction::WarpifySsh(offer) => {
                if self.is_active && self.ssh_offer.as_ref() == Some(offer) {
                    ctx.emit(WarpifyFooterViewEvent::WarpifySsh(offer.clone()));
                }
            }
            WarpifyFooterViewAction::Warpify => {
                if self.is_active {
                    self.clear(ctx);
                    ctx.emit(WarpifyFooterViewEvent::Warpify);
                }
            }
            WarpifyFooterViewAction::UseAgent => {
                self.clear(ctx);
                ctx.emit(WarpifyFooterViewEvent::UseAgent);
            }
            WarpifyFooterViewAction::Dismiss => {
                self.clear(ctx);
                ctx.emit(WarpifyFooterViewEvent::Dismiss);
            }
        }
    }
}
