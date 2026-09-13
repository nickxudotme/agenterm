use super::*;

#[test]
fn copy_menu_uses_native_macos_shortcut() {
    let menu = edit_menu();
    let copy = menu.menu_items.iter().find_map(|item| match item {
        MenuItem::Custom(item) if item.properties.name == "Copy" => Some(item),
        _ => None,
    });
    let copy = copy.expect("Edit menu must include Copy");
    assert_eq!(
        copy.properties.keystroke,
        Some(Keystroke::parse("cmd-c").expect("valid keystroke"))
    );
}

#[test]
fn paste_menu_uses_custom_action_and_shortcut() {
    let menu = edit_menu();
    assert!(
        !menu
            .menu_items
            .iter()
            .any(|item| matches!(item, MenuItem::Standard(StandardAction::Paste)))
    );
    let paste = menu.menu_items.iter().find_map(|item| match item {
        MenuItem::Custom(item) if item.properties.name == "Paste" => Some(item),
        _ => None,
    });
    let paste = paste.expect("Edit menu must include Paste");
    assert_eq!(
        paste.properties.keystroke,
        trigger_to_keystroke(&Trigger::Custom(CustomAction::Paste.into()))
    );
}

#[test]
fn edit_menu_includes_context_aware_editing_actions() {
    let menu = edit_menu();
    let expected = [
        ("Undo", CustomAction::Undo),
        ("Redo", CustomAction::Redo),
        ("Cut", CustomAction::Cut),
        ("Copy", CustomAction::Copy),
        ("Paste", CustomAction::Paste),
        ("Select All", CustomAction::SelectAll),
        ("Clear Editor", CustomAction::ClearEditor),
        ("Find", CustomAction::Find),
    ];

    for (name, action) in expected {
        let item = menu.menu_items.iter().find_map(|item| match item {
            MenuItem::Custom(item) if item.properties.name == name => Some(item),
            _ => None,
        });
        let item = item.unwrap_or_else(|| panic!("Edit menu must include {name}"));
        assert_eq!(
            item.properties.keystroke,
            trigger_to_keystroke(&Trigger::Custom(action.into()))
        );
    }
}
