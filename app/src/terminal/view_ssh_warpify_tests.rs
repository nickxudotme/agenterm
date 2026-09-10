use std::cell::RefCell;
use std::rc::Rc;

use warpui::App;

use super::*;
use crate::test_util::terminal::{
    add_window_with_id_and_terminal, initialize_app_for_terminal_view,
};

fn prepare_ssh_offer(
    view: &mut TerminalView,
    ctx: &mut ViewContext<TerminalView>,
) -> SshWarpifyOffer {
    let block_id = {
        let mut model = view.model.lock();
        model.simulate_long_running_block("ssh target", "user@target:~$ ");
        model
            .block_list_mut()
            .active_block_mut()
            .set_session_id(SessionId::from(42));
        view.active_block_metadata = Some(model.block_list().active_block().metadata());
        model.start_notify_on_end_of_ssh_login();
        model.active_block_id().clone()
    };
    view.warpify_state
        .set_pending_ssh_host("ssh target".into(), Some("target".into()));
    view.handle_detected_end_of_ssh_login(&SshLoginStatus::ReadyToWarpify { block_id }, ctx);
    view.warpify_state.ssh_offer().cloned().unwrap()
}

#[derive(Clone, Copy)]
enum SshEntry {
    Banner,
    Footer,
    Shortcut,
}

fn invoke_ssh_entry(
    view: &mut TerminalView,
    offer: &SshWarpifyOffer,
    entry: SshEntry,
    ctx: &mut ViewContext<TerminalView>,
) {
    match entry {
        SshEntry::Banner => {
            let mut banner = WarpifyBannerState::new("ssh target".into(), None);
            banner.ssh_offer = Some(offer.clone());
            view.handle_action(&banner.action(), ctx);
        }
        SshEntry::Footer => {
            view.use_agent_footer.update(ctx, |_, ctx| {
                ctx.emit(super::use_agent_footer::UseAgentToolbarEvent::WarpifySsh(
                    offer.clone(),
                ));
            });
        }
        SshEntry::Shortcut => view.handle_action(&TerminalAction::TriggerSubshellBootstrap, ctx),
    }
}

fn assert_ssh_entry_writes_once(entry: SshEntry) {
    App::test((), move |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let (_, terminal) = add_window_with_id_and_terminal(&mut app, None);
        let writes = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
        let captured = writes.clone();
        app.update(|ctx| {
            ctx.subscribe_to_view(&terminal, move |_, event, _| {
                if let Event::WriteBytesToPty { bytes } = event {
                    captured.borrow_mut().push(bytes.to_vec());
                }
            });
        });
        let offer = terminal.update(&mut app, prepare_ssh_offer);
        assert!(writes.borrow().is_empty());
        terminal.update(&mut app, |view, ctx| {
            invoke_ssh_entry(view, &offer, entry, ctx)
        });
        let first_write_count = writes.borrow().len();
        assert!(
            first_write_count > 0,
            "accepted entry must emit real bootstrap PTY writes"
        );
        terminal.update(&mut app, |view, ctx| {
            invoke_ssh_entry(view, &offer, entry, ctx)
        });
        assert_eq!(
            writes.borrow().len(),
            first_write_count,
            "duplicate entry must not write again"
        );
    });
}

#[test]
fn ssh_banner_action_bootstraps_once() {
    assert_ssh_entry_writes_once(SshEntry::Banner);
}

#[test]
fn ssh_footer_event_bootstraps_once() {
    assert_ssh_entry_writes_once(SshEntry::Footer);
}

#[test]
fn ssh_shortcut_bootstraps_once() {
    assert_ssh_entry_writes_once(SshEntry::Shortcut);
}

#[test]
fn stale_ssh_offer_cannot_bootstrap_new_block() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let (_, terminal) = add_window_with_id_and_terminal(&mut app, None);
        let writes = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
        let captured = writes.clone();
        app.update(|ctx| {
            ctx.subscribe_to_view(&terminal, move |_, event, _| {
                if let Event::WriteBytesToPty { bytes } = event {
                    captured.borrow_mut().push(bytes.to_vec());
                }
            });
        });
        let old_offer = terminal.update(&mut app, prepare_ssh_offer);
        let new_offer = terminal.update(&mut app, |view, ctx| {
            view.model.lock().finish_block();
            prepare_ssh_offer(view, ctx)
        });
        assert_ne!(old_offer.block_id, new_offer.block_id);
        terminal.update(&mut app, |view, ctx| {
            invoke_ssh_entry(view, &old_offer, SshEntry::Banner, ctx);
            invoke_ssh_entry(view, &old_offer, SshEntry::Footer, ctx);
        });
        assert!(
            writes.borrow().is_empty(),
            "old UI actions must not transfer to the new SSH session"
        );
    });
}

#[test]
fn non_ssh_subshell_action_still_writes_bootstrap() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let (_, terminal) = add_window_with_id_and_terminal(&mut app, None);
        let writes = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
        let captured = writes.clone();
        app.update(|ctx| {
            ctx.subscribe_to_view(&terminal, move |_, event, _| {
                if let Event::WriteBytesToPty { bytes } = event {
                    captured.borrow_mut().push(bytes.to_vec());
                }
            });
        });
        terminal.update(&mut app, |view, ctx| {
            view.model.lock().simulate_long_running_block("bash", "$ ");
            assert!(!view.model.lock().is_ssh_block());
            view.handle_action(&TerminalAction::TriggerSubshellBootstrap, ctx);
        });
        assert!(
            !writes.borrow().is_empty(),
            "non-SSH subshell behavior must remain available"
        );
    });
}

#[test]
fn ssh_prompt_offer_does_not_write_to_pty() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let (_, terminal) = add_window_with_id_and_terminal(&mut app, None);
        let writes = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
        let captured = writes.clone();
        app.update(|ctx| {
            ctx.subscribe_to_view(&terminal, move |_, event, _| {
                if let Event::WriteBytesToPty { bytes } = event {
                    captured.borrow_mut().push(bytes.to_vec());
                }
            });
        });
        terminal.update(&mut app, |view, ctx| {
            let block_id = {
                let mut model = view.model.lock();
                model.simulate_long_running_block(
                    "ssh target",
                    "Last login: today\r\nuser@target:~$ ",
                );
                model
                    .block_list_mut()
                    .active_block_mut()
                    .set_session_id(SessionId::from(42));
                view.active_block_metadata = Some(model.block_list().active_block().metadata());
                model.start_notify_on_end_of_ssh_login();
                model.active_block_id().clone()
            };
            view.warpify_state
                .set_pending_ssh_host("ssh target".into(), Some("target".into()));
            view.handle_detected_end_of_ssh_login(
                &SshLoginStatus::ReadyToWarpify { block_id },
                ctx,
            );
            let offer = view
                .warpify_state
                .ssh_offer()
                .cloned()
                .expect("offer created");
            assert_eq!(view.ssh_warpify_rejection(&offer, ctx), None);
            assert!(
                view.model
                    .lock()
                    .block_list()
                    .active_block()
                    .block_banner()
                    .is_some()
                    || view.use_agent_footer.as_ref(ctx).is_warpify_active(ctx)
            );
        });
        assert!(
            writes.borrow().is_empty(),
            "offering SSH integration must not write PTY bytes"
        );
        terminal.update(&mut app, |view, ctx| {
            let offer = view.warpify_state.ssh_offer().cloned().unwrap();
            view.model.lock().process_bytes("\r\nPin+Token: ");
            view.trigger_offered_ssh_bootstrap(&offer, ctx);
            assert_eq!(
                view.ssh_warpify_rejection(&offer, ctx),
                Some("no-shell-prompt")
            );
            view.model.lock().finish_block();
            view.trigger_offered_ssh_bootstrap(&offer, ctx);
        });
        assert!(
            writes.borrow().is_empty(),
            "authentication and exited-block clicks must not write PTY bytes"
        );
    });
}
