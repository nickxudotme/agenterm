use super::*;

fn eligibility() -> SshWarpifyEligibility {
    SshWarpifyEligibility {
        running: true,
        prompt_detected: true,
        enabled: true,
        denylisted: false,
        agent: false,
        viewer: false,
    }
}

fn offer() -> SshWarpifyOffer {
    SshWarpifyOffer {
        block_id: BlockId::new(),
        session_id: SessionId::from(42),
        host: "target".into(),
    }
}

#[test]
fn ssh_offer_cannot_move_to_another_block_or_session() {
    let mut state = WarpifyState::default();
    let offer = offer();
    state.offer_ssh(offer.clone());
    assert_eq!(
        state.reject_ssh_offer(
            &offer,
            &offer.block_id,
            Some(offer.session_id),
            &eligibility()
        ),
        None
    );
    assert_eq!(
        state.reject_ssh_offer(
            &offer,
            &BlockId::new(),
            Some(offer.session_id),
            &eligibility()
        ),
        Some("stale-source")
    );
    assert_eq!(
        state.reject_ssh_offer(
            &offer,
            &offer.block_id,
            Some(SessionId::from(43)),
            &eligibility()
        ),
        Some("stale-source")
    );
    state.offer_ssh(super::tests::offer());
    assert_eq!(
        state.reject_ssh_offer(
            &offer,
            &offer.block_id,
            Some(offer.session_id),
            &eligibility()
        ),
        Some("stale-source")
    );
}

#[test]
fn consumed_ssh_offer_is_not_rearmed_by_duplicate_ready() {
    let mut state = WarpifyState::default();
    let offer = offer();
    state.offer_ssh(offer.clone());
    state.consume_ssh_offer();
    state.offer_ssh(offer.clone());
    assert_eq!(
        state.reject_ssh_offer(
            &offer,
            &offer.block_id,
            Some(offer.session_id),
            &eligibility()
        ),
        Some("consumed")
    );
}

#[test]
fn manual_bootstrap_blocks_late_offer() {
    let mut state = WarpifyState::default();
    let offer = offer();
    state.mark_ssh_bootstrap_started(offer.block_id.clone());
    state.offer_ssh(offer.clone());
    assert_eq!(
        state.reject_ssh_offer(
            &offer,
            &offer.block_id,
            Some(offer.session_id),
            &eligibility()
        ),
        Some("consumed")
    );
}

#[test]
fn ssh_offer_rechecks_all_policy_guards() {
    let mut state = WarpifyState::default();
    let offer = offer();
    state.offer_ssh(offer.clone());
    for (snapshot, reason) in [
        (
            SshWarpifyEligibility {
                running: false,
                ..eligibility()
            },
            "not-running",
        ),
        (
            SshWarpifyEligibility {
                prompt_detected: false,
                ..eligibility()
            },
            "no-shell-prompt",
        ),
        (
            SshWarpifyEligibility {
                enabled: false,
                ..eligibility()
            },
            "disabled",
        ),
        (
            SshWarpifyEligibility {
                denylisted: true,
                ..eligibility()
            },
            "denylisted",
        ),
        (
            SshWarpifyEligibility {
                agent: true,
                ..eligibility()
            },
            "agent",
        ),
        (
            SshWarpifyEligibility {
                viewer: true,
                ..eligibility()
            },
            "viewer",
        ),
    ] {
        assert_eq!(
            state.reject_ssh_offer(&offer, &offer.block_id, Some(offer.session_id), &snapshot),
            Some(reason)
        );
    }
    state.delete_state();
    assert_eq!(
        state.reject_ssh_offer(
            &offer,
            &offer.block_id,
            Some(offer.session_id),
            &eligibility()
        ),
        Some("stale-source")
    );
}
