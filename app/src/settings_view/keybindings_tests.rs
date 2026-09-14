use warpui::keymap::{BindingDescription, BindingId};

use super::should_show_binding_in_channel;
use crate::channel::Channel;
use crate::util::bindings::{BindingGroup, CommandBinding};

fn binding(group: Option<BindingGroup>) -> CommandBinding {
    CommandBinding {
        name: "some_action".to_owned(),
        description: BindingDescription::new("Some action"),
        trigger: None,
        action: None,
        group,
        id: BindingId(0),
    }
}

#[test]
fn oss_shows_bindings_that_need_no_agent_or_cloud() {
    for group in [
        BindingGroup::Settings,
        BindingGroup::Close,
        BindingGroup::Navigation,
        BindingGroup::KeyboardShortcuts,
        BindingGroup::Terminal,
        BindingGroup::AutoUpdate,
        BindingGroup::Notifications,
    ] {
        assert!(
            should_show_binding_in_channel(&binding(Some(group)), Channel::Oss),
            "{group:?} bindings should be visible in Agenterm"
        );
    }
}

#[test]
fn oss_hides_agent_and_cloud_binding_groups() {
    for group in [
        BindingGroup::WarpAi,
        BindingGroup::Workflow,
        BindingGroup::Notebooks,
        BindingGroup::Folders,
        BindingGroup::EnvVarCollection,
    ] {
        assert!(
            !should_show_binding_in_channel(&binding(Some(group)), Channel::Oss),
            "{group:?} bindings should stay hidden in Agenterm"
        );
    }
}

#[test]
fn non_oss_channels_show_every_group() {
    for group in [
        BindingGroup::WarpAi,
        BindingGroup::Workflow,
        BindingGroup::AutoUpdate,
        BindingGroup::Notifications,
    ] {
        assert!(should_show_binding_in_channel(
            &binding(Some(group)),
            Channel::Stable
        ));
    }
}
