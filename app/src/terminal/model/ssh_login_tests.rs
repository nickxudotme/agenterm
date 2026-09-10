use super::*;

#[test]
fn bounded_ssh_detection_retains_final_prompt_after_large_history() {
    let mut model = TerminalModel::mock(None, None);
    model.simulate_long_running_block("ssh target", "");
    model.start_notify_on_end_of_ssh_login();
    model.process_bytes("banner\r\n".repeat(500).as_str());
    model.process_bytes("user@target:~$ ");
    assert_eq!(
        ssh::util::check_ssh_login_grid(model.block_list().active_block().output_grid()),
        SshLoginState::PromptDetected
    );
}

#[test]
fn bounded_ssh_detection_rejects_truncated_soft_wrapped_authentication() {
    for keyword in ["Password", "Pin+Token"] {
        let mut model = TerminalModel::mock(None, None);
        model.simulate_long_running_block("ssh target", "");
        model.start_notify_on_end_of_ssh_login();
        model.process_bytes(format!("{keyword}: {}$ ", "a".repeat(500)).as_str());
        assert_eq!(
            ssh::util::check_ssh_login_grid(model.block_list().active_block().output_grid()),
            SshLoginState::Authenticating
        );
        model.process_bytes("\r\nuser@target:~$ ");
        assert_eq!(
            ssh::util::check_ssh_login_grid(model.block_list().active_block().output_grid()),
            SshLoginState::PromptDetected
        );
    }
}

#[test]
fn ssh_login_observes_prompt_after_banners_and_reauthentication() {
    let mut model = TerminalModel::mock(None, None);
    model.simulate_long_running_block("ssh target", "authz success\r\n");
    model.start_notify_on_end_of_ssh_login();
    model.process_bytes("Last login: today\r\nWelcome\r\n");
    model.check_for_end_of_ssh_login(true);
    assert_ne!(
        model
            .notify_on_end_of_ssh_login
            .as_ref()
            .unwrap()
            .notification_state,
        SshLoginNotificationState::Completed
    );
    model.process_bytes("Pin+Token: ");
    assert_ne!(
        model
            .notify_on_end_of_ssh_login
            .as_ref()
            .unwrap()
            .notification_state,
        SshLoginNotificationState::Completed
    );
    model.process_bytes("\r\nuser@target:~$ ");
    assert_eq!(
        model
            .notify_on_end_of_ssh_login
            .as_ref()
            .unwrap()
            .notification_state,
        SshLoginNotificationState::Completed
    );
    model.process_bytes("\r\nPassword: ");
    assert_ne!(
        model
            .notify_on_end_of_ssh_login
            .as_ref()
            .unwrap()
            .notification_state,
        SshLoginNotificationState::Completed
    );
    model.process_bytes("\r\nuser@target:~$ ");
    assert_eq!(
        model
            .notify_on_end_of_ssh_login
            .as_ref()
            .unwrap()
            .notification_state,
        SshLoginNotificationState::Completed
    );
}
