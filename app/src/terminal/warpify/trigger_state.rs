use warpui::r#async::SpawnedFutureHandle;
use warpui::{EntityId, ViewContext, ViewHandle};

use super::success_block::WarpifySuccessBlock;
use crate::terminal::TerminalView;
use crate::terminal::model::block::BlockId;
use crate::terminal::model::session::SessionId;
use crate::terminal::shell::ShellType;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshWarpifyOffer {
    pub block_id: BlockId,
    pub session_id: SessionId,
    pub host: String,
}

pub struct SshWarpifyEligibility {
    pub running: bool,
    pub prompt_detected: bool,
    pub enabled: bool,
    pub denylisted: bool,
    pub agent: bool,
    pub viewer: bool,
}

impl SshWarpifyEligibility {
    pub fn rejection(&self) -> Option<&'static str> {
        if !self.running {
            Some("not-running")
        } else if !self.prompt_detected {
            Some("no-shell-prompt")
        } else if !self.enabled {
            Some("disabled")
        } else if self.denylisted {
            Some("denylisted")
        } else if self.agent {
            Some("agent")
        } else if self.viewer {
            Some("viewer")
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub enum SshBlockState {
    WarpifySuccess {
        handle: ViewHandle<WarpifySuccessBlock>,
    },
}

impl SshBlockState {
    pub fn should_prevent_input(&self) -> bool {
        true
    }

    pub fn get_block_view_id(&self) -> EntityId {
        match self {
            SshBlockState::WarpifySuccess { handle, .. } => handle.id(),
        }
    }

    pub fn on_warpified_session_complete(
        &self,
        ctx: &mut ViewContext<TerminalView>,
    ) -> Option<EntityId> {
        match self {
            SshBlockState::WarpifySuccess { handle } => {
                handle.update(ctx, |block, ctx| {
                    block.on_warpified_session_complete(ctx);
                });
            }
        }
        None
    }
}

/// Temporary state used to trigger Warpification.
#[derive(Default)]
struct WarpifyTriggerState {
    block_id: Option<BlockId>,

    /// Lets us abort an attempt to auto warpify if the subshell command
    /// hasn't completed.
    auto_warpify_abort_handle: Option<SpawnedFutureHandle>,

    /// The subshell banner waits 1s before showing. This is to see that the command stays running
    /// for a while without exiting. We store the abort handle here so that the
    /// TerminalEvent::BlockCompleted event can abort the banner.
    subshell_banner_abort_handle: Option<SpawnedFutureHandle>,

    /// The command which may trigger ssh Warpification
    pending_command: Option<String>,
    /// The Host which may trigger ssh Warpification
    pending_warpify_ssh_host: Option<String>,

    /// Which, if any, SSH block is currently added to the blocklist.
    ssh_block_state: Option<SshBlockState>,

    ssh_warpify_timeout_handle: Option<SpawnedFutureHandle>,

    shell_type: Option<ShellType>,

    is_shell_detection_in_progress: bool,
}

#[derive(Default)]
pub struct WarpifyState {
    ssh_offer: Option<SshWarpifyOffer>,
    ssh_offer_consumed: bool,
    ssh_started_block: Option<BlockId>,
    session_id: Option<SessionId>,

    pending_state: Option<WarpifyTriggerState>,
    /// A unique-enough ID that is used to validate that a timeout is still valid.
    timeout_id: u8,
}

impl WarpifyState {
    pub fn offer_ssh(&mut self, offer: SshWarpifyOffer) {
        if self.ssh_offer.as_ref() == Some(&offer) {
            return;
        }
        self.ssh_offer = Some(offer);
        self.ssh_offer_consumed = false;
    }

    pub fn ssh_offer(&self) -> Option<&SshWarpifyOffer> {
        self.ssh_offer.as_ref()
    }

    pub fn reject_ssh_offer(
        &self,
        offer: &SshWarpifyOffer,
        current_block: &BlockId,
        current_session: Option<SessionId>,
        eligibility: &SshWarpifyEligibility,
    ) -> Option<&'static str> {
        if self.ssh_offer.as_ref() != Some(offer)
            || &offer.block_id != current_block
            || Some(offer.session_id) != current_session
        {
            return Some("stale-source");
        }
        if self.ssh_offer_consumed || self.ssh_started_block.as_ref() == Some(current_block) {
            return Some("consumed");
        }
        eligibility.rejection()
    }

    pub fn consume_ssh_offer(&mut self) {
        self.ssh_offer_consumed = true;
    }

    pub fn mark_ssh_bootstrap_started(&mut self, block_id: BlockId) {
        self.ssh_started_block = Some(block_id);
        self.consume_ssh_offer();
    }

    pub fn delete_state(&mut self) {
        self.ssh_offer = None;
        self.pending_state.take();
    }

    pub fn is_shell_detection_in_progress(&self) -> bool {
        self.pending_state
            .as_ref()
            .map(|state| state.is_shell_detection_in_progress)
            .unwrap_or_default()
    }

    pub fn set_shell_detection_in_progress(&mut self) {
        if let Some(ref mut pending_state) = self.pending_state.as_mut() {
            pending_state.is_shell_detection_in_progress = true;
        }
    }

    pub fn set_shell_type(&mut self, shell_type: &ShellType) {
        let pending_state = self.pending_state.get_or_insert_with(Default::default);
        pending_state.shell_type = Some(shell_type.to_owned());
        pending_state.is_shell_detection_in_progress = false;
    }

    pub fn get_shell_type(&self) -> Option<ShellType> {
        self.pending_state
            .as_ref()
            .and_then(|state| state.shell_type)
    }

    pub fn add_subshell_banner_abort_handle(&mut self, spawned_future_handle: SpawnedFutureHandle) {
        let pending_state = self.pending_state.get_or_insert_with(Default::default);
        pending_state.subshell_banner_abort_handle = Some(spawned_future_handle);
    }

    pub fn take_subshell_banner_abort_handle(&mut self) -> Option<SpawnedFutureHandle> {
        self.pending_state
            .as_mut()
            .and_then(|state| state.subshell_banner_abort_handle.take())
    }

    pub fn add_auto_warpify_abort_handle(&mut self, spawned_future_handle: SpawnedFutureHandle) {
        let pending_state = self.pending_state.get_or_insert_with(Default::default);
        pending_state.auto_warpify_abort_handle = Some(spawned_future_handle);
    }

    pub fn abort_auto_warpify(&mut self) {
        if let Some(abort_handle) = self
            .pending_state
            .as_mut()
            .and_then(|state| state.auto_warpify_abort_handle.take())
        {
            abort_handle.abort();
        };
    }

    pub fn add_ssh_warpify_timeout_handle(&mut self, spawned_future_handle: SpawnedFutureHandle) {
        let pending_state = self.pending_state.get_or_insert_with(Default::default);
        pending_state.ssh_warpify_timeout_handle = Some(spawned_future_handle);
    }

    pub fn abort_ssh_warpify_timeout(&mut self) {
        self.replace_timeout_id();
        if let Some(handle) = self
            .pending_state
            .as_mut()
            .and_then(|state| state.ssh_warpify_timeout_handle.take())
        {
            handle.abort();
        };
    }

    pub fn clear_ssh_block_state(&mut self) {
        if let Some(ref mut pending_state) = self.pending_state.as_mut() {
            pending_state.ssh_block_state = None;
        }
    }

    pub fn set_ssh_block_state(&mut self, ssh_block_state: SshBlockState) {
        let pending_state = self.pending_state.get_or_insert_with(Default::default);
        pending_state.ssh_block_state = Some(ssh_block_state);
    }

    pub fn ssh_block_state(&self) -> Option<&SshBlockState> {
        self.pending_state
            .as_ref()
            .and_then(|state| state.ssh_block_state.as_ref())
    }

    pub fn get_pending_ssh_host(&self) -> Option<String> {
        self.pending_state
            .as_ref()
            .and_then(|state: &WarpifyTriggerState| state.pending_warpify_ssh_host.clone())
    }

    pub fn get_pending_ssh_command(&self) -> Option<String> {
        self.pending_state
            .as_ref()
            .and_then(|state: &WarpifyTriggerState| state.pending_command.clone())
    }

    pub fn take_pending_ssh_host(&mut self) -> Option<String> {
        self.pending_state
            .as_mut()
            .and_then(|state: &mut WarpifyTriggerState| state.pending_warpify_ssh_host.take())
    }

    pub fn clear_pending_ssh_host(&mut self) {
        if let Some(ref mut pending_state) = self.pending_state.as_mut() {
            pending_state.pending_warpify_ssh_host = None;
        }
    }

    pub fn set_pending_ssh_host(&mut self, command: String, ssh_host: Option<String>) {
        let pending_state = self.pending_state.get_or_insert_with(Default::default);
        pending_state.pending_command = Some(command);
        pending_state.pending_warpify_ssh_host = ssh_host;
    }

    pub fn set_block_id(&mut self, block_id: BlockId) {
        let pending_state = self.pending_state.get_or_insert_with(Default::default);
        pending_state.block_id = Some(block_id);
    }

    pub fn block_id(&self) -> Option<BlockId> {
        self.pending_state
            .as_ref()
            .and_then(|state| state.block_id.clone())
    }

    pub fn timeout_id(&self) -> u8 {
        self.timeout_id
    }

    /// Generates a new timeout ID. This is used to validate that a timeout is still valid.
    /// Call this to get a new timeout ID before starting a new timeout, or to invalidate
    /// an existing timeout.
    pub fn replace_timeout_id(&mut self) -> u8 {
        self.timeout_id = self.timeout_id.wrapping_add(1);
        self.timeout_id
    }

    /// The terminal view should prevent typing
    pub fn should_prevent_input(&self) -> bool {
        let Some(state) = self.ssh_block_state() else {
            return false;
        };
        state.should_prevent_input()
    }

    /// Called once whenever we get a local block completed, as opposed to a remote ssh block
    /// and we have a Warpify Success block.
    fn on_warpified_session_complete(
        &mut self,
        state: WarpifyTriggerState,
        ctx: &mut ViewContext<TerminalView>,
    ) -> Option<EntityId> {
        self.clear_ssh_block_state();
        ctx.notify();
        let Some(block) = &state.ssh_block_state else {
            return None;
        };
        block.on_warpified_session_complete(ctx)
    }

    pub fn on_warpify_start(&mut self, active_session_id: Option<SessionId>) {
        self.consume_ssh_offer();
        self.session_id = active_session_id;
    }

    /// Called whenever a block is completed, to determine whether a Warpified session
    /// has been completed.
    pub fn get_completed_warpify_session_id(
        &mut self,
        active_session_id: Option<SessionId>,
        ctx: &mut ViewContext<TerminalView>,
    ) -> Option<EntityId> {
        if self.session_id.is_none() || active_session_id == self.session_id {
            return None;
        }
        if let Some(state) = self.pending_state.take() {
            return self.on_warpified_session_complete(state, ctx);
        };
        None
    }
}

#[cfg(test)]
#[path = "trigger_state_tests.rs"]
mod tests;
