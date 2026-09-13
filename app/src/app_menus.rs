use warpui::actions::StandardAction;
use warpui::keymap::{Keystroke, Trigger};
use warpui::platform::menu::{
    CustomMenuItem, Menu, MenuBar, MenuItem, MenuItemProperties, MenuItemPropertyChanges,
};
use warpui::windowing::WindowManager;
use warpui::{AppContext, SingletonEntity};

use crate::util::bindings::{CustomAction, trigger_to_keystroke};

pub fn menu_bar(_: &mut AppContext) -> MenuBar {
    MenuBar::new(vec![
        app_menu(),
        file_menu(),
        edit_menu(),
        view_menu(),
        blocks_menu(),
        Menu::new("Window", vec![]),
    ])
}

fn app_menu() -> Menu {
    Menu::new(
        "Agenterm",
        vec![
            action_item("Settings…", CustomAction::ShowSettings),
            MenuItem::Separator,
            MenuItem::Standard(StandardAction::Hide),
            MenuItem::Standard(StandardAction::HideOtherApps),
            MenuItem::Standard(StandardAction::ShowAllApps),
            MenuItem::Separator,
            MenuItem::Standard(StandardAction::Quit),
        ],
    )
}

fn file_menu() -> Menu {
    Menu::new(
        "File",
        vec![
            MenuItem::Custom(CustomMenuItem::new(
                "New Window",
                |ctx| ctx.dispatch_global_action("root_view:open_new", &()),
                no_updates,
                Some(Keystroke::parse("cmd-n").expect("valid keystroke")),
            )),
            MenuItem::Custom(CustomMenuItem::new(
                "New Tab",
                dispatch_action(CustomAction::NewTab),
                no_updates,
                Some(Keystroke::parse("cmd-t").expect("valid keystroke")),
            )),
            MenuItem::Separator,
            MenuItem::Custom(CustomMenuItem::new(
                "Close Tab",
                dispatch_action(CustomAction::CloseTab),
                no_updates,
                Some(Keystroke::parse("cmd-w").expect("valid keystroke")),
            )),
            action_item("Close Window", CustomAction::CloseWindow),
        ],
    )
}

fn edit_menu() -> Menu {
    Menu::new(
        "Edit",
        vec![
            action_item("Undo", CustomAction::Undo),
            action_item("Redo", CustomAction::Redo),
            MenuItem::Separator,
            action_item("Cut", CustomAction::Cut),
            action_item("Copy", CustomAction::Copy),
            action_item("Paste", CustomAction::Paste),
            action_item("Select All", CustomAction::SelectAll),
            action_item("Clear Editor", CustomAction::ClearEditor),
            MenuItem::Separator,
            action_item("Find", CustomAction::Find),
        ],
    )
}

fn view_menu() -> Menu {
    Menu::new(
        "View",
        vec![
            action_item("Zoom In", CustomAction::IncreaseZoom),
            action_item("Zoom Out", CustomAction::DecreaseZoom),
            action_item("Actual Size", CustomAction::ResetZoom),
        ],
    )
}

fn blocks_menu() -> Menu {
    Menu::new(
        "Blocks",
        vec![
            action_item("Clear Blocks", CustomAction::ClearBlocks),
            MenuItem::Separator,
            action_item("Select Block Above", CustomAction::SelectBlockAbove),
            action_item("Select Block Below", CustomAction::SelectBlockBelow),
            action_item("Copy Block", CustomAction::CopyBlock),
            action_item("Copy Block Command", CustomAction::CopyBlockCommand),
            action_item("Copy Block Output", CustomAction::CopyBlockOutput),
        ],
    )
}

pub fn dock_menu() -> Menu {
    Menu::new(
        "New Window",
        vec![MenuItem::Custom(CustomMenuItem::new(
            "New Window",
            |ctx| {
                ctx.dispatch_global_action("root_view:open_new", &());
                ctx.dispatch_global_action("workspace:save_app", &());
            },
            no_updates,
            Some(Keystroke::parse("cmd-n").expect("valid keystroke")),
        ))],
    )
}

fn action_item(name: &'static str, action: CustomAction) -> MenuItem {
    MenuItem::Custom(CustomMenuItem::new(
        name,
        dispatch_action(action),
        action_updates(action),
        trigger_to_keystroke(&Trigger::Custom(action.into())),
    ))
}

fn dispatch_action(action: CustomAction) -> impl Fn(&mut AppContext) + 'static {
    move |ctx| {
        if let Some(window_id) = WindowManager::handle(ctx).as_ref(ctx).active_window() {
            ctx.dispatch_custom_action(action, window_id);
        }
    }
}

fn action_updates(
    action: CustomAction,
) -> impl Fn(&MenuItemProperties, &mut AppContext) -> MenuItemPropertyChanges + 'static {
    move |_, ctx| {
        let mut changes = MenuItemPropertyChanges::default();
        ctx.update_custom_action_binding(action.into(), |binding| {
            changes.disabled = Some(binding.is_none());
            if let Some(binding) = binding {
                changes.keystroke = Some(trigger_to_keystroke(binding.trigger));
            }
        });
        changes
    }
}

fn no_updates(_: &MenuItemProperties, _: &mut AppContext) -> MenuItemPropertyChanges {
    Default::default()
}

#[cfg(test)]
#[path = "app_menus_tests.rs"]
mod tests;
