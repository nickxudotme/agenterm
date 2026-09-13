use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Display;

use markdown_parser::{FormattedText, FormattedTextFragment, FormattedTextLine};
use regex::Regex;
use settings::{Setting, ToggleableSetting};
use strum::IntoEnumIterator;
use warp_core::features::FeatureFlag;
use warp_errors::report_if_error;
use warpui::elements::{
    Container, Flex, FormattedTextElement, HighlightedHyperlink, MouseStateHandle, ParentElement,
};
use warpui::keymap::ContextPredicate;
use warpui::presenter::ChildView;
use warpui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use warpui::ui_components::switch::SwitchStateHandle;
use warpui::{
    Action, AppContext, Element, Entity, ModelHandle, SingletonEntity, TypedActionView, View,
    ViewContext, ViewHandle,
};

use super::settings_page::{
    Category, CategoryHeader, HEADER_FONT_SIZE, HEADER_PADDING, LocalOnlyIconState, MatchData,
    PageType, SettingsPageEvent, SettingsPageMeta, SettingsPageViewHandle, SettingsWidget,
    ToggleState, add_setting, render_alternating_color_list, render_body_item,
    render_body_item_label, render_dropdown_item, render_page_title,
};
use super::{SettingsAction, SettingsSection, ToggleSettingActionPair, flags};
use crate::appearance::Appearance;
use crate::editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions, TextOptions};
use crate::send_telemetry_from_ctx;
use crate::server::telemetry::TelemetryEvent;
use crate::settings::{ReuseExistingSshControlMaster, SshSettings};
use crate::terminal::warpify::settings::{
    EnableSshWarpification, SshExtensionInstallMode, SshExtensionInstallModeSetting,
    WarpifySettings, WarpifySettingsChangedEvent,
};
use crate::terminal::zmodem_settings::{
    ZmodemDownloadDirectory, ZmodemOverwritePolicy, ZmodemOverwritePolicySetting, ZmodemSettings,
    ZmodemSettingsChangedEvent, ZmodemUploadCommand,
};
use crate::ui_components::blended_colors;
use crate::view_components::dropdown::{Dropdown, DropdownItem};
use crate::view_components::{SubmittableTextInput, SubmittableTextInputEvent};

pub fn init_actions_from_parent_view<T: Action + Clone>(
    app: &mut AppContext,
    context: &ContextPredicate,
    builder: fn(SettingsAction) -> T,
) {
    // Add all of the toggle settings from the Warpify Page that you want to show up on the Command Palette here.
    let mut toggle_binding_pairs = vec![];
    if WarpifySettings::as_ref(app)
        .enable_ssh_warpification
        .is_supported_on_current_platform()
    {
        toggle_binding_pairs.push(ToggleSettingActionPair::new(
            "SSH Warpification",
            builder(SettingsAction::WarpifyPageToggle(
                WarpifyPageAction::ToggleSshWarpification,
            )),
            context,
            flags::SSH_WARPIFICATION_CONTEXT_FLAG,
        ));
    }

    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(toggle_binding_pairs, app);
}

const CONTENT_FONT_SIZE: f32 = 12.;
const ITEM_VERTICAL_SPACING: f32 = 24.;
/// There's a built-in 10px margin below the text input.
const BUILT_IN_TEXT_INPUT_MARGIN: f32 = 10.;
const SPACE_AFTER_TEXT_INPUT: f32 = ITEM_VERTICAL_SPACING - BUILT_IN_TEXT_INPUT_MARGIN;

const SSH_REUSE_CONTROL_MASTER_DESCRIPTION: &str = "Attach to a live SSH ControlMaster you already have configured for the destination host instead of creating a Warp-owned one. Takes effect in new tabs.";

const SSH_EXTENSION_INSTALL_MODE_DESCRIPTION: &str = "Controls the installation behavior for Warp's SSH extension when a remote host doesn't have it installed.";

/// This page lets users configure when they get asked to warpify a session. Some shell commands
/// are recognized by default. Users can add new shell commands, or prevent the default ones from
/// asking. Users can also enable the SSH wrapper, and add hosts to a denylist.
/// This page is essentially the View for the SubshellSettings model, as well as the SshSettings
/// related to warpification.
pub struct WarpifyPageView {
    page: PageType<Self>,
    /// This needs to mirror the length of SubshellSettings::added_remove_button_states.
    remove_added_command_button_states: Vec<MouseStateHandle>,
    add_added_commands_editor: ViewHandle<SubmittableTextInput>,
    /// This needs to mirror the length of SubshellSettings::denylisted_remove_button_states.
    remove_denylisted_command_button_states: Vec<MouseStateHandle>,
    add_denylisted_commands_editor: ViewHandle<SubmittableTextInput>,

    ssh_extension_install_mode_dropdown: ViewHandle<Dropdown<WarpifyPageAction>>,
    zmodem_download_directory_editor: ViewHandle<EditorView>,
    zmodem_upload_command_editor: ViewHandle<EditorView>,
    zmodem_overwrite_policy_dropdown: ViewHandle<Dropdown<WarpifyPageAction>>,
}

impl WarpifyPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let warpify_settings_handle = WarpifySettings::handle(ctx);

        ctx.observe(&warpify_settings_handle, Self::update_button_states);
        ctx.subscribe_to_model(&warpify_settings_handle, move |me, model, event, ctx| {
            me.update_button_states(model, ctx);
            if matches!(
                event,
                WarpifySettingsChangedEvent::SshExtensionInstallModeSetting { .. }
            ) {
                me.update_dropdown(ctx);
            }
            ctx.notify();
        });

        // Added commands can be specified by regex, while denied commands are strictly exact
        // match.
        let add_added_commands_editor = ctx.add_typed_action_view(|ctx| {
            let mut input =
                SubmittableTextInput::new(ctx).validate_on_edit(|regex| Regex::new(regex).is_ok());
            input.set_placeholder_text("command (supports regex)", ctx);
            input
        });

        ctx.subscribe_to_view(
            &add_added_commands_editor,
            Self::handle_added_command_editor_event,
        );

        let add_denylisted_commands_editor = ctx.add_typed_action_view(|ctx| {
            let mut input = SubmittableTextInput::new(ctx);
            input.set_placeholder_text("command (supports regex)", ctx);
            input
        });

        ctx.subscribe_to_view(
            &add_denylisted_commands_editor,
            Self::handle_denylisted_command_editor_event,
        );

        let ssh_extension_install_mode_dropdown =
            Self::create_ssh_extension_install_mode_dropdown(ctx);

        let zmodem_download_directory_editor = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::single_line(
                SingleLineEditorOptions {
                    text: TextOptions::ui_font_size(Appearance::as_ref(ctx)),
                    ..Default::default()
                },
                ctx,
            );
            let directory = ZmodemSettings::as_ref(ctx).download_directory.to_string();
            editor.set_buffer_text(&directory, ctx);
            editor.set_placeholder_text("Downloads", ctx);
            editor
        });
        ctx.subscribe_to_view(&zmodem_download_directory_editor, |me, _, event, ctx| {
            if matches!(event, EditorEvent::Enter | EditorEvent::Blurred) {
                let directory = me
                    .zmodem_download_directory_editor
                    .as_ref(ctx)
                    .buffer_text(ctx);
                ZmodemSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.download_directory.set_value(directory, ctx));
                });
            } else if matches!(event, EditorEvent::Escape) {
                me.update_zmodem_download_directory_editor(ctx);
                ctx.emit(SettingsPageEvent::FocusModal);
            }
        });

        let zmodem_upload_command_editor = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::single_line(
                SingleLineEditorOptions {
                    text: TextOptions::ui_font_size(Appearance::as_ref(ctx)),
                    ..Default::default()
                },
                ctx,
            );
            let command = ZmodemSettings::as_ref(ctx).upload_command.to_string();
            editor.set_buffer_text(&command, ctx);
            editor.set_placeholder_text("rz", ctx);
            editor
        });
        ctx.subscribe_to_view(&zmodem_upload_command_editor, |me, _, event, ctx| {
            if matches!(event, EditorEvent::Enter | EditorEvent::Blurred) {
                let command = me.zmodem_upload_command_editor.as_ref(ctx).buffer_text(ctx);
                let command = if command.trim().is_empty() {
                    "rz".to_owned()
                } else {
                    command.trim().to_owned()
                };
                ZmodemSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.upload_command.set_value(command, ctx));
                });
                me.update_zmodem_upload_command_editor(ctx);
            } else if matches!(event, EditorEvent::Escape) {
                me.update_zmodem_upload_command_editor(ctx);
                ctx.emit(SettingsPageEvent::FocusModal);
            }
        });

        let zmodem_overwrite_policy_dropdown = ctx.add_typed_action_view(|ctx| {
            let policy = *ZmodemSettings::as_ref(ctx).overwrite_policy.value();
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(SSH_EXTENSION_DROPDOWN_WIDTH);
            dropdown.set_menu_width(SSH_EXTENSION_DROPDOWN_WIDTH, ctx);
            dropdown.add_items(
                ZmodemOverwritePolicy::iter()
                    .map(|policy| {
                        DropdownItem::new(
                            policy.display_name(),
                            WarpifyPageAction::SetZmodemOverwritePolicy(policy),
                        )
                    })
                    .collect(),
                ctx,
            );
            dropdown
                .set_selected_by_action(WarpifyPageAction::SetZmodemOverwritePolicy(policy), ctx);
            dropdown
        });
        ctx.subscribe_to_model(&ZmodemSettings::handle(ctx), |me, _, event, ctx| {
            match event {
                ZmodemSettingsChangedEvent::ZmodemDownloadDirectory { .. } => {
                    me.update_zmodem_download_directory_editor(ctx);
                }
                ZmodemSettingsChangedEvent::ZmodemUploadCommand { .. } => {
                    me.update_zmodem_upload_command_editor(ctx);
                }
                ZmodemSettingsChangedEvent::ZmodemOverwritePolicySetting { .. } => {
                    let policy = *ZmodemSettings::as_ref(ctx).overwrite_policy.value();
                    me.zmodem_overwrite_policy_dropdown
                        .update(ctx, |dropdown, ctx| {
                            dropdown.set_selected_by_action(
                                WarpifyPageAction::SetZmodemOverwritePolicy(policy),
                                ctx,
                            );
                        });
                }
                ZmodemSettingsChangedEvent::ZmodemEnabled { .. }
                | ZmodemSettingsChangedEvent::ZmodemAskDownloadDirectory { .. }
                | ZmodemSettingsChangedEvent::ZmodemLastDirectory { .. }
                | ZmodemSettingsChangedEvent::ZmodemDragEnabled { .. }
                | ZmodemSettingsChangedEvent::ZmodemCrossTransferEnabled { .. } => {}
            }
            ctx.notify();
        });

        let mut instance = Self {
            page: Self::build_page(ctx),
            remove_added_command_button_states: Default::default(),
            add_added_commands_editor,
            remove_denylisted_command_button_states: Default::default(),
            add_denylisted_commands_editor,
            ssh_extension_install_mode_dropdown,
            zmodem_download_directory_editor,
            zmodem_upload_command_editor,
            zmodem_overwrite_policy_dropdown,
        };

        instance.update_button_states(warpify_settings_handle, ctx);
        instance
    }

    fn update_zmodem_download_directory_editor(&mut self, ctx: &mut ViewContext<Self>) {
        let directory = ZmodemSettings::as_ref(ctx).download_directory.to_string();
        self.zmodem_download_directory_editor
            .update(ctx, |editor, ctx| {
                editor.set_buffer_text(&directory, ctx);
            });
    }

    fn update_zmodem_upload_command_editor(&mut self, ctx: &mut ViewContext<Self>) {
        let command = ZmodemSettings::as_ref(ctx).upload_command.to_string();
        self.zmodem_upload_command_editor
            .update(ctx, |editor, ctx| {
                editor.set_buffer_text(&command, ctx);
            });
    }

    fn build_page(ctx: &mut ViewContext<Self>) -> PageType<Self> {
        let mut categories = vec![
            Category::new("", vec![Box::new(TitleWidget::default())]),
            Category::with_header(
                CategoryHeader::new("Subshells")
                    .with_subtitle("Subshells supported: bash, zsh, and fish."),
                vec![Box::new(SubshellsWidget::default())],
            ),
        ];

        let warpify_settings = WarpifySettings::as_ref(ctx);
        if warpify_settings
            .enable_ssh_warpification
            .is_supported_on_current_platform()
        {
            categories.push(Category::with_header(
                CategoryHeader::new("SSH").with_subtitle("Warpify your interactive SSH sessions."),
                vec![Box::new(SSHWidget::default())],
            ));
        }
        if ZmodemSettings::as_ref(ctx)
            .enabled
            .is_supported_on_current_platform()
        {
            categories.push(Category::with_header(
                CategoryHeader::new("ZMODEM"),
                vec![Box::new(ZmodemWidget::default())],
            ));
        }
        PageType::new_categorized(categories, None)
    }

    /// This method ensures each command in the SubshellSettings has a matching button state for
    /// its delete button in the View.
    fn update_button_states(
        &mut self,
        warpify_settings_handle: ModelHandle<WarpifySettings>,
        ctx: &mut ViewContext<Self>,
    ) {
        let warpify_settings = warpify_settings_handle.as_ref(ctx);
        self.remove_denylisted_command_button_states = warpify_settings
            .subshell_command_denylist
            .iter()
            .map(|_| Default::default())
            .collect();
        self.remove_added_command_button_states = warpify_settings
            .added_subshell_commands
            .iter()
            .map(|_| Default::default())
            .collect();
        ctx.notify();
    }

    /// Syncs the install-mode dropdown selection with the current
    /// `WarpifySettings::ssh_extension_install_mode` value (e.g. after it
    /// was changed from the SSH remote server choice view).
    fn update_dropdown(&mut self, ctx: &mut ViewContext<Self>) {
        let current_mode = *WarpifySettings::as_ref(ctx)
            .ssh_extension_install_mode
            .value();
        self.ssh_extension_install_mode_dropdown
            .update(ctx, |dropdown, ctx| {
                dropdown.set_selected_by_action(
                    WarpifyPageAction::SetSshExtensionInstallMode(current_mode),
                    ctx,
                );
            });
    }

    fn handle_added_command_editor_event(
        &mut self,
        _handle: ViewHandle<SubmittableTextInput>,
        event: &SubmittableTextInputEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            SubmittableTextInputEvent::Submit(new_command) => {
                WarpifySettings::handle(ctx).update(ctx, |warpify_settings, ctx| {
                    warpify_settings.add_subshell_command(new_command, ctx);
                });

                send_telemetry_from_ctx!(TelemetryEvent::AddAddedSubshellCommand, ctx);
            }
            SubmittableTextInputEvent::Escape => ctx.emit(SettingsPageEvent::FocusModal),
        }
    }

    fn handle_denylisted_command_editor_event(
        &mut self,
        _handle: ViewHandle<SubmittableTextInput>,
        event: &SubmittableTextInputEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            SubmittableTextInputEvent::Submit(new_command) => {
                WarpifySettings::handle(ctx).update(ctx, |warpify_settings, ctx| {
                    warpify_settings.denylist_subshell_command(new_command, ctx);
                });

                send_telemetry_from_ctx!(TelemetryEvent::AddDenylistedSubshellCommand, ctx);
            }
            SubmittableTextInputEvent::Escape => ctx.emit(SettingsPageEvent::FocusModal),
        }
    }

    fn remove_denylisted_command(&self, index: usize, ctx: &mut ViewContext<Self>) {
        send_telemetry_from_ctx!(TelemetryEvent::RemoveDenylistedSubshellCommand, ctx);
        WarpifySettings::handle(ctx).update(ctx, |warpify, ctx| {
            warpify.remove_denylisted_subshell_command(index, ctx)
        });
    }

    fn remove_added_command(&self, index: usize, ctx: &mut ViewContext<Self>) {
        send_telemetry_from_ctx!(TelemetryEvent::RemoveAddedSubshellCommand, ctx);
        WarpifySettings::handle(ctx).update(ctx, |warpify, ctx| {
            warpify.remove_added_subshell_command(index, ctx)
        });
    }
}

impl Entity for WarpifyPageView {
    type Event = SettingsPageEvent;
}

fn build_sub_sub_title(title: &str, appearance: &Appearance) -> Container {
    appearance
        .ui_builder()
        .span(title.to_string())
        .with_style(UiComponentStyles {
            font_size: Some(CONTENT_FONT_SIZE),
            ..Default::default()
        })
        .build()
}

const SSH_EXTENSION_DROPDOWN_WIDTH: f32 = 250.;

impl WarpifyPageView {
    fn create_ssh_extension_install_mode_dropdown(
        ctx: &mut ViewContext<Self>,
    ) -> ViewHandle<Dropdown<WarpifyPageAction>> {
        let items: Vec<DropdownItem<WarpifyPageAction>> = SshExtensionInstallMode::iter()
            .map(|mode| {
                DropdownItem::new(
                    mode.display_name(),
                    WarpifyPageAction::SetSshExtensionInstallMode(mode),
                )
            })
            .collect();

        let current_mode = *WarpifySettings::as_ref(ctx)
            .ssh_extension_install_mode
            .value();
        let enable_ssh_warpification = *WarpifySettings::as_ref(ctx)
            .enable_ssh_warpification
            .value();

        ctx.add_typed_action_view(move |ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(SSH_EXTENSION_DROPDOWN_WIDTH);
            dropdown.set_menu_width(SSH_EXTENSION_DROPDOWN_WIDTH, ctx);
            dropdown.add_items(items, ctx);
            dropdown.set_selected_by_action(
                WarpifyPageAction::SetSshExtensionInstallMode(current_mode),
                ctx,
            );
            if !enable_ssh_warpification {
                dropdown.set_disabled(ctx);
            }
            dropdown
        })
    }

    /// Renders a title, a list of items that can be removed, and an input field to add new items.
    fn build_input_list<
        ListItem: Display,
        SettingsPageAction: Action + Clone,
        F: Fn(usize) -> SettingsPageAction,
        T: View,
    >(
        &self,
        title: &str,
        patterns: &[ListItem],
        mouse_states: &[MouseStateHandle],
        create_action: F,
        handle: &ViewHandle<T>,
        appearance: &Appearance,
    ) -> Container {
        let mut column = Flex::column();
        let mut title = build_sub_sub_title(title, appearance);

        if !patterns.is_empty() {
            title = title.with_padding_bottom(BUILT_IN_TEXT_INPUT_MARGIN);
        }

        column.add_child(title.finish());

        render_alternating_color_list(
            &mut column,
            patterns,
            mouse_states,
            create_action,
            appearance,
        );

        Container::new(
            column
                .with_child(
                    Container::new(ChildView::new(handle).finish())
                        .with_margin_bottom(SPACE_AFTER_TEXT_INPUT)
                        .finish(),
                )
                .finish(),
        )
    }
}

impl View for WarpifyPageView {
    fn ui_name() -> &'static str {
        "WarpifyPageView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum WarpifyPageAction {
    RemoveAddedCommand(usize),
    RemoveDenylistedCommand(usize),
    ToggleSshWarpification,
    /// Toggles whether the legacy SSH wrapper attaches to an existing
    /// ControlMaster for the destination host instead of creating its own.
    ToggleReuseSshControlMaster,
    /// Set the SSH extension installation mode (always ask / always install / always skip).
    SetSshExtensionInstallMode(SshExtensionInstallMode),
    ToggleZmodem,
    ToggleZmodemAskDownloadDirectory,
    ToggleZmodemDrag,
    ToggleZmodemCrossTransfer,
    SetZmodemOverwritePolicy(ZmodemOverwritePolicy),
    OpenUrl(String),
}

impl TypedActionView for WarpifyPageView {
    type Action = WarpifyPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        use WarpifyPageAction::*;
        match action {
            RemoveDenylistedCommand(index) => self.remove_denylisted_command(*index, ctx),
            RemoveAddedCommand(index) => self.remove_added_command(*index, ctx),
            ToggleSshWarpification => {
                WarpifySettings::handle(ctx).update(ctx, |ssh_settings, ctx| {
                    report_if_error!(
                        ssh_settings
                            .enable_ssh_warpification
                            .toggle_and_save_value(ctx)
                    );
                    send_telemetry_from_ctx!(
                        TelemetryEvent::ToggleSshWarpification {
                            enabled: *ssh_settings.enable_ssh_warpification.value(),
                        },
                        ctx
                    );
                });
                let enabled = *WarpifySettings::as_ref(ctx)
                    .enable_ssh_warpification
                    .value();
                self.ssh_extension_install_mode_dropdown
                    .update(ctx, |dropdown, ctx| {
                        if enabled {
                            dropdown.set_enabled(ctx);
                        } else {
                            dropdown.set_disabled(ctx);
                        }
                    });
            }
            ToggleReuseSshControlMaster => {
                SshSettings::handle(ctx).update(ctx, |ssh_settings, ctx| {
                    report_if_error!(
                        ssh_settings
                            .reuse_existing_control_master
                            .toggle_and_save_value(ctx)
                    );
                    send_telemetry_from_ctx!(
                        TelemetryEvent::FeaturesPageAction {
                            action: "ToggleSshReuseControlMaster".to_string(),
                            value: ssh_settings
                                .reuse_existing_control_master
                                .value()
                                .to_string(),
                        },
                        ctx
                    );
                });
            }
            SetSshExtensionInstallMode(mode) => {
                WarpifySettings::handle(ctx).update(ctx, |warpify_settings, ctx| {
                    report_if_error!(
                        warpify_settings
                            .ssh_extension_install_mode
                            .set_value(*mode, ctx)
                    );
                    send_telemetry_from_ctx!(
                        TelemetryEvent::SetSshExtensionInstallMode {
                            mode: mode.display_name(),
                        },
                        ctx
                    );
                });
            }
            ToggleZmodem => {
                ZmodemSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.enabled.toggle_and_save_value(ctx));
                });
            }
            ToggleZmodemAskDownloadDirectory => {
                ZmodemSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.ask_download_directory.toggle_and_save_value(ctx));
                });
            }
            ToggleZmodemDrag => {
                ZmodemSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.drag_enabled.toggle_and_save_value(ctx));
                });
            }
            ToggleZmodemCrossTransfer => {
                ZmodemSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.cross_transfer_enabled.toggle_and_save_value(ctx));
                });
            }
            SetZmodemOverwritePolicy(policy) => {
                ZmodemSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.overwrite_policy.set_value(*policy, ctx));
                });
            }
            OpenUrl(url) => {
                ctx.open_url(url.as_str());
            }
        }
    }
}

impl SettingsPageMeta for WarpifyPageView {
    fn section() -> SettingsSection {
        SettingsSection::Warpify
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        true
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<WarpifyPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<WarpifyPageView>) -> Self {
        SettingsPageViewHandle::Warpify(view_handle)
    }
}

#[derive(Default)]
struct TitleWidget {
    learn_more_highlight_index: HighlightedHyperlink,
}

impl TitleWidget {
    fn render_top_of_page(&self, appearance: &Appearance, _app: &AppContext) -> Box<dyn Element> {
        let warpify_description = vec![
            FormattedTextFragment::plain_text(
                "Configure whether Warp attempts to “Warpify” (add support for blocks, \
                    input modes, etc) certain shells. ",
            ),
            FormattedTextFragment::hyperlink(
                "Learn more",
                "https://docs.warp.dev/terminal/warpify/subshells",
            ),
        ];

        let warpify_description = FormattedTextElement::new(
            FormattedText::new([FormattedTextLine::Line(warpify_description)]),
            CONTENT_FONT_SIZE,
            appearance.ui_font_family(),
            appearance.ui_font_family(),
            blended_colors::text_sub(appearance.theme(), appearance.theme().surface_1()),
            self.learn_more_highlight_index.clone(),
        )
        .with_hyperlink_font_color(appearance.theme().accent().into_solid())
        .register_default_click_handlers(|url, _, ctx| {
            ctx.open_url(&url.url);
        })
        .finish();

        Flex::column()
            .with_child(render_page_title("Warpify", HEADER_FONT_SIZE, appearance))
            .with_child(warpify_description)
            .finish()
    }
}

impl SettingsWidget for TitleWidget {
    type View = WarpifyPageView;

    fn search_terms(&self) -> &str {
        "ssh subshell warpify session"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        Container::new(self.render_top_of_page(appearance, app))
            .with_margin_bottom(ITEM_VERTICAL_SPACING)
            .finish()
    }
}

#[derive(Default)]
struct SubshellsWidget {}

impl SubshellsWidget {
    fn render_subshells_section(
        &self,
        view: &WarpifyPageView,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let mut column = Flex::column();

        let warpify_settings = WarpifySettings::as_ref(app);

        column.add_child(
            view.build_input_list(
                "Added commands",
                &warpify_settings.added_subshell_commands,
                &view.remove_added_command_button_states,
                WarpifyPageAction::RemoveAddedCommand,
                &view.add_added_commands_editor,
                appearance,
            )
            .finish(),
        );

        column.add_child(
            view.build_input_list(
                "Denylisted commands",
                &warpify_settings.subshell_command_denylist,
                &view.remove_denylisted_command_button_states,
                WarpifyPageAction::RemoveDenylistedCommand,
                &view.add_denylisted_commands_editor,
                appearance,
            )
            .with_margin_bottom(-BUILT_IN_TEXT_INPUT_MARGIN)
            .finish(),
        );

        column.finish()
    }
}

impl SettingsWidget for SubshellsWidget {
    type View = WarpifyPageView;

    fn search_terms(&self) -> &str {
        "warpify subshell"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        Container::new(self.render_subshells_section(view, appearance, app))
            .with_margin_bottom(ITEM_VERTICAL_SPACING)
            .finish()
    }
}

#[derive(Default)]
struct SSHWidget {
    enable_ssh_warpification_switch_state: SwitchStateHandle,
    reuse_control_master_switch_state: SwitchStateHandle,
    local_only_icon_tooltip_states: RefCell<HashMap<String, MouseStateHandle>>,
}

impl SettingsWidget for SSHWidget {
    type View = WarpifyPageView;

    fn search_terms(&self) -> &str {
        "warpify ssh"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let mut column = Flex::column();
        let ui_builder = appearance.ui_builder();
        let description_text_color = appearance
            .theme()
            .sub_text_color(appearance.theme().surface_2());

        let enable_ssh_warpification = *WarpifySettings::as_ref(app)
            .enable_ssh_warpification
            .value();

        add_setting(
            &mut column,
            &WarpifySettings::as_ref(app).enable_ssh_warpification,
            move || {
                render_body_item::<WarpifyPageAction>(
                    "Warpify SSH Sessions".into(),
                    None,
                    LocalOnlyIconState::for_setting(
                        EnableSshWarpification::storage_key(),
                        EnableSshWarpification::sync_to_cloud(),
                        &mut self.local_only_icon_tooltip_states.borrow_mut(),
                        app,
                    ),
                    ToggleState::Enabled,
                    appearance,
                    ui_builder
                        .switch(self.enable_ssh_warpification_switch_state.clone())
                        .check(enable_ssh_warpification)
                        .build()
                        .on_click(move |ctx, _, _| {
                            ctx.dispatch_typed_action(WarpifyPageAction::ToggleSshWarpification);
                        })
                        .finish(),
                    None,
                )
            },
        );

        if FeatureFlag::SshRemoteServer.is_enabled() {
            let label_color_override = if !enable_ssh_warpification {
                Some(appearance.theme().disabled_ui_text_color())
            } else {
                None
            };
            add_setting(
                &mut column,
                &WarpifySettings::as_ref(app).ssh_extension_install_mode,
                move || {
                    Container::new(render_dropdown_item(
                        appearance,
                        "Install SSH extension",
                        Some(SSH_EXTENSION_INSTALL_MODE_DESCRIPTION),
                        None,
                        LocalOnlyIconState::for_setting(
                            SshExtensionInstallModeSetting::storage_key(),
                            SshExtensionInstallModeSetting::sync_to_cloud(),
                            &mut self.local_only_icon_tooltip_states.borrow_mut(),
                            app,
                        ),
                        label_color_override,
                        &view.ssh_extension_install_mode_dropdown,
                    ))
                    .with_padding_bottom(HEADER_PADDING)
                    .finish()
                },
            );
        }

        let reuse_existing_control_master = *SshSettings::as_ref(app)
            .reuse_existing_control_master
            .value();
        add_setting(
            &mut column,
            &SshSettings::as_ref(app).reuse_existing_control_master,
            move || {
                let mut column = Flex::column();
                column.add_child(render_body_item::<WarpifyPageAction>(
                    "Reuse existing SSH ControlMaster".into(),
                    None,
                    LocalOnlyIconState::for_setting(
                        ReuseExistingSshControlMaster::storage_key(),
                        ReuseExistingSshControlMaster::sync_to_cloud(),
                        &mut self.local_only_icon_tooltip_states.borrow_mut(),
                        app,
                    ),
                    enable_ssh_warpification.into(),
                    appearance,
                    ui_builder
                        .switch(self.reuse_control_master_switch_state.clone())
                        .check(reuse_existing_control_master)
                        .with_disabled(!enable_ssh_warpification)
                        .build()
                        .on_click(move |ctx, _, _| {
                            if !enable_ssh_warpification {
                                return;
                            }
                            ctx.dispatch_typed_action(
                                WarpifyPageAction::ToggleReuseSshControlMaster,
                            );
                        })
                        .finish(),
                    None,
                ));
                column.add_child(
                    ui_builder
                        .paragraph(SSH_REUSE_CONTROL_MASTER_DESCRIPTION.to_owned())
                        .with_style(UiComponentStyles {
                            font_color: Some(description_text_color.into_solid()),
                            margin: Some(
                                Coords::default()
                                    .top(styles::DESCRIPTION_NEGATIVE_MARGIN_OFFSET)
                                    .bottom(styles::DESCRIPTION_LINE_MARGIN_BOTTOM),
                            ),
                            ..Default::default()
                        })
                        .build()
                        .finish(),
                );
                column.finish()
            },
        );

        column.finish()
    }
}

#[derive(Default)]
struct ZmodemWidget {
    enabled_switch_state: SwitchStateHandle,
    ask_directory_switch_state: SwitchStateHandle,
    drag_switch_state: SwitchStateHandle,
    cross_transfer_switch_state: SwitchStateHandle,
    local_only_icon_tooltip_states: RefCell<HashMap<String, MouseStateHandle>>,
}

impl ZmodemWidget {
    fn local_only_icon<S: Setting>(&self, app: &AppContext) -> LocalOnlyIconState {
        LocalOnlyIconState::for_setting(
            S::storage_key(),
            S::sync_to_cloud(),
            &mut self.local_only_icon_tooltip_states.borrow_mut(),
            app,
        )
    }

    fn render_toggle<S: Setting<Value = bool>>(
        &self,
        setting: &S,
        label: &str,
        control: (&SwitchStateHandle, WarpifyPageAction),
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let (state, action) = control;
        render_body_item::<WarpifyPageAction>(
            label.to_owned(),
            None,
            self.local_only_icon::<S>(app),
            ToggleState::Enabled,
            appearance,
            appearance
                .ui_builder()
                .switch(state.clone())
                .check(*setting.value())
                .build()
                .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
                .finish(),
            None,
        )
    }

    fn render_text_editor<S: Setting>(
        &self,
        label: &str,
        editor: &ViewHandle<EditorView>,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        Container::new(
            Flex::column()
                .with_child(render_body_item_label::<WarpifyPageAction>(
                    label.to_owned(),
                    None,
                    None,
                    self.local_only_icon::<S>(app),
                    ToggleState::Enabled,
                    appearance,
                ))
                .with_child(
                    appearance
                        .ui_builder()
                        .text_input(editor.clone())
                        .with_style(UiComponentStyles {
                            margin: Some(Coords::default().top(BUILT_IN_TEXT_INPUT_MARGIN)),
                            ..Default::default()
                        })
                        .build()
                        .finish(),
                )
                .finish(),
        )
        .with_margin_bottom(ITEM_VERTICAL_SPACING)
        .finish()
    }
}

impl SettingsWidget for ZmodemWidget {
    type View = WarpifyPageView;

    fn search_terms(&self) -> &str {
        "zmodem rz sz upload download directory overwrite skip rename drag cross terminal transfer"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let settings = ZmodemSettings::as_ref(app);
        Flex::column()
            .with_child(self.render_toggle(
                &settings.enabled,
                "Enable ZMODEM",
                (&self.enabled_switch_state, WarpifyPageAction::ToggleZmodem),
                appearance,
                app,
            ))
            .with_child(self.render_toggle(
                &settings.ask_download_directory,
                "Ask for download directory",
                (
                    &self.ask_directory_switch_state,
                    WarpifyPageAction::ToggleZmodemAskDownloadDirectory,
                ),
                appearance,
                app,
            ))
            .with_child(self.render_text_editor::<ZmodemDownloadDirectory>(
                "Download directory",
                &view.zmodem_download_directory_editor,
                appearance,
                app,
            ))
            .with_child(render_dropdown_item(
                appearance,
                "Existing files",
                None,
                None,
                self.local_only_icon::<ZmodemOverwritePolicySetting>(app),
                None,
                &view.zmodem_overwrite_policy_dropdown,
            ))
            .with_child(self.render_toggle(
                &settings.drag_enabled,
                "Upload dropped files with ZMODEM",
                (&self.drag_switch_state, WarpifyPageAction::ToggleZmodemDrag),
                appearance,
                app,
            ))
            .with_child(self.render_text_editor::<ZmodemUploadCommand>(
                "Upload command",
                &view.zmodem_upload_command_editor,
                appearance,
                app,
            ))
            .with_child(self.render_toggle(
                &settings.cross_transfer_enabled,
                "Cross-terminal transfers",
                (
                    &self.cross_transfer_switch_state,
                    WarpifyPageAction::ToggleZmodemCrossTransfer,
                ),
                appearance,
                app,
            ))
            .finish()
    }
}

mod styles {
    // Apply a negative margin to the description text so it appears closer to the main
    // settings option text.
    pub const DESCRIPTION_NEGATIVE_MARGIN_OFFSET: f32 = -8.;

    /// The space after a description.
    pub const DESCRIPTION_LINE_MARGIN_BOTTOM: f32 = 18.;
}
