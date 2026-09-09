use super::*;

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
