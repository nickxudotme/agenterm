//! Cross-terminal staging and target selection. Runtime events contain metadata, never file bytes.
//!
//! Register with `ZmodemCrossTransfer::register`. Terminal input paths must consult `blocks_input`,
//! and ordinary upload entry points must consult `is_reserved`. Feed runtime events to `on_event`
//! after updating per-view metadata, but before showing ordinary pickers or completion toasts.
//! Feed real close/disconnect and explicit cancellation to `cancel_terminal`. `worker_released`
//! requires a PTY acknowledgement that no worker can access the staged paths, not merely `Finished`.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use tempfile::TempDir;
use warp_terminal::zmodem::runtime::{
    Control, FileOutcome, MAX_FILES, MAX_PATH_BYTES, OverwritePolicy, Role, TransferEvent,
    TransferId, TransferOutcome, next_transfer_id,
};
use warpui::elements::{
    ChildView, Container, CrossAxisAlignment, Flex, MainAxisAlignment, ParentElement,
};
use warpui::r#async::Timer;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::{
    AppContext, Element, Entity, EntityId, ModelContext, SingletonEntity, TypedActionView, View,
    ViewContext, ViewHandle, WeakViewHandle,
};

use super::TerminalView;
use super::model::BlockId;
use super::model::session::SessionId;
use super::zmodem_settings::ZmodemSettings;
use super::zmodem_transfer::display_text;
use crate::appearance::Appearance;
use crate::modal::{Modal, ModalEvent};
use crate::view_components::action_button::{ActionButton, NakedTheme, PrimaryTheme};
use crate::view_components::{
    DismissibleToast, DropdownItem, FilterableDropdown, FilterableDropdownEvent,
};
use crate::workspace::{ToastStack, WorkspaceRegistry};

const CHOICE_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_BINDINGS: usize = 8;
type ShellIdentity = (SessionId, BlockId);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CrossTransferId(TransferId);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    AwaitingSource,
    Receiving,
    PreparingTarget,
    AwaitingTarget,
    Forwarding,
    Finished,
}

struct Flow {
    source: EntityId,
    target: EntityId,
    source_transfer: Option<TransferId>,
    target_transfer: Option<TransferId>,
    phase: Phase,
    workers: HashSet<(EntityId, TransferId)>,
    skipped: usize,
}

impl Flow {
    fn new(source: EntityId, target: EntityId) -> Self {
        Self {
            source,
            target,
            source_transfer: None,
            target_transfer: None,
            phase: Phase::AwaitingSource,
            workers: HashSet::new(),
            skipped: 0,
        }
    }

    fn blocks_input(&self, terminal: EntityId) -> bool {
        terminal == self.target
            && matches!(
                self.phase,
                Phase::AwaitingSource | Phase::Receiving | Phase::PreparingTarget | Phase::Forwarding
            )
    }

    fn matches(&self, terminal: EntityId, id: TransferId, role: Role) -> bool {
        match role {
            Role::Download => terminal == self.source && self.source_transfer == Some(id),
            Role::Upload => terminal == self.target && self.target_transfer == Some(id),
        }
    }

    fn bind_source(&mut self, id: TransferId) -> bool {
        if self.phase != Phase::AwaitingSource {
            return false;
        }
        self.source_transfer = Some(id);
        self.workers.insert((self.source, id));
        self.phase = Phase::Receiving;
        true
    }

    fn bind_target(&mut self, id: TransferId) -> bool {
        if !matches!(self.phase, Phase::PreparingTarget | Phase::AwaitingTarget) {
            return false;
        }
        self.target_transfer = Some(id);
        self.workers.insert((self.target, id));
        self.phase = Phase::Forwarding;
        true
    }

    fn can_cleanup(&self) -> bool {
        self.phase == Phase::Finished && self.workers.is_empty()
    }
}

/// An unexpected owner drop must not remove a directory still used by a detached worker.
struct StagingDirectory(Option<TempDir>);

impl StagingDirectory {
    fn create() -> std::io::Result<Self> {
        tempfile::Builder::new()
            .prefix("agenterm-zmodem-")
            .tempdir()
            .map(|directory| Self(Some(directory)))
    }

    fn path(&self) -> &Path {
        self.0.as_ref().expect("live staging directory").path()
    }

    fn close(mut self) -> std::io::Result<()> {
        self.0.take().expect("live staging directory").close()
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if let Some(directory) = self.0.take() {
            // Process teardown may precede worker teardown. OS temporary storage owns the residue.
            let _ = directory.keep();
        }
    }
}

struct Binding {
    source: WeakViewHandle<TerminalView>,
    target: WeakViewHandle<TerminalView>,
    source_title: String,
    target_title: String,
    target_identity: Option<ShellIdentity>,
    flow: Flow,
    staging: Option<StagingDirectory>,
    paths: Vec<PathBuf>,
    deadline: Instant,
    result: Option<String>,
}

/// Effects are applied after the coordinator is checked back into the entity store.
#[derive(Clone, Debug)]
pub(crate) enum CrossTransferEvent {
    Changed,
    Receive(CrossTransferId),
    PrepareTarget(CrossTransferId),
    UploadRequested(CrossTransferId, TransferId),
    Cancel(WeakViewHandle<TerminalView>, TransferId),
    Result(WeakViewHandle<TerminalView>, String, bool),
}

#[derive(Default)]
pub(crate) struct ZmodemCrossTransfer {
    bindings: HashMap<CrossTransferId, Binding>,
    reservations: HashMap<EntityId, CrossTransferId>,
}

impl Entity for ZmodemCrossTransfer {
    type Event = CrossTransferEvent;
}

impl SingletonEntity for ZmodemCrossTransfer {}

impl ZmodemCrossTransfer {
    pub fn register(ctx: &mut AppContext) {
        let handle = ctx.add_singleton_model(|_| Self::default());
        ctx.subscribe_to_model(&handle, |_, event, ctx| dispatch(event, ctx));
        handle.update(ctx, |_, ctx| Self::schedule_tick(ctx));
    }

    pub fn is_reserved(&self, terminal: EntityId) -> bool {
        self.reservations.contains_key(&terminal)
    }

    pub fn blocks_input(&self, terminal: EntityId) -> bool {
        self.binding_for(terminal)
            .is_some_and(|binding| binding.flow.blocks_input(terminal))
    }

    pub fn status_text(&self, terminal: EntityId) -> Option<String> {
        let binding = self.binding_for(terminal)?;
        Some(match binding.flow.phase {
            Phase::AwaitingSource => format!("Waiting for source: {}", binding.source_title),
            Phase::Receiving => format!("Receiving for {}", binding.target_title),
            Phase::PreparingTarget => format!("Preparing upload to {}", binding.target_title),
            Phase::AwaitingTarget => format!("Waiting for rz in {}", binding.target_title),
            Phase::Forwarding => format!("Forwarding to {}", binding.target_title),
            Phase::Finished => binding.result.clone().unwrap_or_default(),
        })
    }

    fn binding_for(&self, terminal: EntityId) -> Option<&Binding> {
        self.bindings.get(self.reservations.get(&terminal)?)
    }

    /// Invoke from the picker after the source view's update has returned.
    fn begin(
        &mut self,
        source: &TargetChoice,
        target: &TargetChoice,
        ctx: &mut ModelContext<Self>,
    ) -> Result<CrossTransferId> {
        if !*ZmodemSettings::as_ref(ctx).enabled
            || !*ZmodemSettings::as_ref(ctx).cross_transfer_enabled
        {
            bail!("Cross-terminal transfers are disabled");
        }
        if self.bindings.len() >= MAX_BINDINGS {
            bail!("Too many cross-terminal transfers are still active");
        }
        if source.terminal.id() == target.terminal.id()
            || self.is_reserved(source.terminal.id())
            || self.is_reserved(target.terminal.id())
        {
            bail!("A selected terminal is already reserved");
        }
        let live = visible_terminals(ctx);
        let source_handle = live.iter().find(|view| view.id() == source.terminal.id());
        let target_handle = live.iter().find(|view| view.id() == target.terminal.id());
        let source_view = source_handle.and_then(|view| view.try_as_ref(ctx));
        let target_view = target_handle.and_then(|view| view.try_as_ref(ctx));
        let (Some(source_view), Some(target_view)) = (source_view, target_view) else {
            bail!("A selected terminal is no longer available");
        };
        if !source_view.zmodem_available(ctx)
            || source_view.zmodem_is_busy()
            || !target_view.zmodem_available(ctx)
            || target_view.zmodem_is_busy()
        {
            bail!("A selected terminal is busy or unavailable");
        }
        let target_identity = target_view.zmodem_shell_identity(ctx);
        let id = CrossTransferId(next_transfer_id());
        self.reservations.insert(source.terminal.id(), id);
        self.reservations.insert(target.terminal.id(), id);
        self.bindings.insert(id, Binding {
            source: source.terminal.clone(),
            target: target.terminal.clone(),
            source_title: source.label.clone(),
            target_title: target.label.clone(),
            target_identity,
            flow: Flow::new(source.terminal.id(), target.terminal.id()),
            staging: None,
            paths: Vec::new(),
            deadline: Instant::now() + CHOICE_TIMEOUT,
            result: None,
        });
        ctx.emit(CrossTransferEvent::Changed);
        ctx.spawn(async { StagingDirectory::create() }, move |me, result, ctx| {
            match result {
                Ok(directory) => {
                    if let Some(binding) = me.bindings.get_mut(&id)
                        && binding.flow.phase != Phase::Finished
                    {
                        binding.staging = Some(directory);
                        if binding.flow.phase == Phase::Receiving {
                            ctx.emit(CrossTransferEvent::Receive(id));
                        }
                    } else {
                        Self::cleanup_directory(directory, ctx);
                    }
                }
                Err(error) => me.finish(id, format!("Cannot create staging directory: {error}"), false, ctx),
            }
        });
        Ok(id)
    }

    /// True means this pair owns the event; suppress ordinary pickers and completion toasts.
    pub fn on_event(
        &mut self,
        terminal: EntityId,
        event: &TransferEvent,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let Some(&binding_id) = self.reservations.get(&terminal) else { return false };
        let binding = self.bindings.get_mut(&binding_id).expect("reserved binding");
        if let TransferEvent::Requested { id, role } = event {
            if binding.flow.matches(terminal, *id, *role) {
                return true;
            }
            if terminal == binding.flow.source && *role == Role::Download && binding.flow.bind_source(*id) {
                binding.deadline = Instant::now() + CHOICE_TIMEOUT;
                if binding.staging.is_some() {
                    ctx.emit(CrossTransferEvent::Receive(binding_id));
                }
            } else if terminal == binding.flow.target && *role == Role::Upload
                && binding.flow.phase == Phase::AwaitingTarget && binding.flow.bind_target(*id)
            {
                ctx.emit(CrossTransferEvent::UploadRequested(binding_id, *id));
            } else {
                let endpoint = if terminal == binding.flow.source { &binding.source } else { &binding.target };
                ctx.emit(CrossTransferEvent::Cancel(endpoint.clone(), *id));
                self.finish(binding_id, "Unexpected transfer in a reserved terminal".into(), false, ctx);
            }
            ctx.emit(CrossTransferEvent::Changed);
            return true;
        }
        let (id, role) = match event {
            TransferEvent::Requested { .. } => unreachable!(),
            TransferEvent::FileStarted { id, role, .. }
            | TransferEvent::Progress { id, role, .. }
            | TransferEvent::FileResult { id, role, .. }
            | TransferEvent::Finished { id, role, .. } => (*id, *role),
        };
        if !binding.flow.matches(terminal, id, role) || binding.flow.phase == Phase::Finished {
            return true;
        }
        match event {
            TransferEvent::FileResult { outcome: FileOutcome::Skipped, .. } if role == Role::Upload => {
                binding.flow.skipped += 1;
            }
            TransferEvent::Finished { outcome, committed_paths, .. } => {
                if *outcome != TransferOutcome::Completed {
                    self.finish(binding_id, format!("Cross-terminal transfer: {outcome:?}"), false, ctx);
                } else if role == Role::Download && binding.flow.phase == Phase::Receiving {
                    let safe = binding.staging.as_ref().is_some_and(|directory| {
                        valid_staged_paths(directory.path(), committed_paths)
                    });
                    if !safe {
                        self.finish(binding_id, "Source returned no valid staged files".into(), false, ctx);
                    } else {
                        binding.paths = committed_paths.clone();
                        binding.flow.phase = Phase::PreparingTarget;
                        binding.deadline = Instant::now() + CHOICE_TIMEOUT;
                        ctx.emit(CrossTransferEvent::PrepareTarget(binding_id));
                        ctx.emit(CrossTransferEvent::Changed);
                    }
                } else if role == Role::Upload && binding.flow.phase == Phase::Forwarding {
                    let message = format!("Transfer to {} complete: {} skipped", binding.target_title, binding.flow.skipped);
                    self.finish(binding_id, message, true, ctx);
                }
            }
            TransferEvent::Requested { .. }
            | TransferEvent::FileStarted { .. }
            | TransferEvent::Progress { .. }
            | TransferEvent::FileResult { .. } => {}
        }
        true
    }

    pub fn cancel_terminal(&mut self, terminal: EntityId, ctx: &mut ModelContext<Self>) -> bool {
        let Some(&id) = self.reservations.get(&terminal) else { return false };
        self.finish(id, "Cross-terminal transfer cancelled".into(), false, ctx);
        true
    }

    /// Also required when a PTY is disposed and its normal event stream has already disconnected.
    pub fn worker_released(&mut self, terminal: EntityId, id: TransferId, ctx: &mut ModelContext<Self>) {
        let Some(&binding_id) = self.reservations.get(&terminal) else { return };
        if let Some(binding) = self.bindings.get_mut(&binding_id) {
            binding.flow.workers.remove(&(terminal, id));
        }
        self.reap(binding_id, ctx);
    }

    fn finish(&mut self, id: CrossTransferId, message: String, success: bool, ctx: &mut ModelContext<Self>) {
        let Some(binding) = self.bindings.get_mut(&id) else { return };
        if binding.flow.phase == Phase::Finished { return }
        binding.flow.phase = Phase::Finished;
        binding.result = Some(display_text(&message));
        for &(terminal, transfer) in &binding.flow.workers {
            let endpoint = if terminal == binding.flow.source { &binding.source } else { &binding.target };
            ctx.emit(CrossTransferEvent::Cancel(endpoint.clone(), transfer));
        }
        ctx.emit(CrossTransferEvent::Result(binding.source.clone(), display_text(&message), success));
        ctx.emit(CrossTransferEvent::Changed);
        self.reap(id, ctx);
    }

    fn reap(&mut self, id: CrossTransferId, ctx: &mut ModelContext<Self>) {
        if !self.bindings.get(&id).is_some_and(|binding| binding.flow.can_cleanup()) { return }
        let binding = self.bindings.remove(&id).expect("finished binding");
        self.reservations.remove(&binding.flow.source);
        self.reservations.remove(&binding.flow.target);
        if let Some(directory) = binding.staging {
            Self::cleanup_directory(directory, ctx);
        }
        ctx.emit(CrossTransferEvent::Changed);
    }

    fn cleanup_directory(directory: StagingDirectory, ctx: &mut ModelContext<Self>) {
        ctx.spawn(async move { directory.close() }, |_, result, _| {
            if let Err(error) = result {
                log::warn!("Could not remove ZMODEM staging directory: {error}");
            }
        });
    }

    fn schedule_tick(ctx: &mut ModelContext<Self>) {
        ctx.spawn(async { Timer::after(Duration::from_secs(1)).await }, |me, _, ctx| {
            let live: HashSet<_> = visible_terminals(ctx).iter().map(ViewHandle::id).collect();
            let enabled = *ZmodemSettings::as_ref(ctx).enabled
                && *ZmodemSettings::as_ref(ctx).cross_transfer_enabled;
            let expired: Vec<_> = me.bindings.iter().filter_map(|(&id, binding)| {
                let waiting = matches!(binding.flow.phase, Phase::AwaitingSource | Phase::PreparingTarget | Phase::AwaitingTarget);
                (binding.flow.phase != Phase::Finished && (!enabled
                    || !live.contains(&binding.flow.source) || !live.contains(&binding.flow.target)
                    || (waiting && Instant::now() >= binding.deadline))).then_some(id)
            }).collect();
            for id in expired {
                me.finish(id, "Cross-terminal transfer ended: terminal unavailable or selection timed out".into(), false, ctx);
            }
            Self::schedule_tick(ctx);
        });
    }
}

fn valid_staged_paths(directory: &Path, paths: &[PathBuf]) -> bool {
    !paths.is_empty() && paths.len() <= MAX_FILES
        && paths.iter().map(|path| path.as_os_str().len()).sum::<usize>() <= MAX_PATH_BYTES
        && paths.iter().all(|path| {
            path.strip_prefix(directory).ok().is_some_and(|relative| {
                let mut components = relative.components();
                matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
            })
        })
}

fn visible_terminals(ctx: &AppContext) -> Vec<ViewHandle<TerminalView>> {
    let mut terminals = Vec::new();
    for (_, workspace) in WorkspaceRegistry::as_ref(ctx).all_workspaces(ctx) {
        let Some(workspace) = workspace.try_as_ref(ctx) else { continue };
        for tab in workspace.tab_views() {
            let Some(tab) = tab.try_as_ref(ctx) else { continue };
            terminals.extend(tab.visible_terminal_views(ctx));
        }
    }
    terminals
}

fn dispatch(event: &CrossTransferEvent, ctx: &mut AppContext) {
    match event {
        CrossTransferEvent::Changed => {
            for terminal in visible_terminals(ctx) {
                let _ = terminal.try_update(ctx, |_, ctx| ctx.notify());
            }
        }
        CrossTransferEvent::Receive(id) => {
            let Some(binding) = ZmodemCrossTransfer::as_ref(ctx).bindings.get(id) else { return };
            if binding.flow.phase != Phase::Receiving { return }
            let (Some(directory), Some(transfer)) = (&binding.staging, binding.flow.source_transfer) else { return };
            let directory = directory.path().to_owned();
            let source = binding.source.clone();
            let result = source.upgrade(ctx).and_then(|source| source.try_update(ctx, |view, ctx| {
                if view.zmodem_active_id() != Some(transfer) || !view.zmodem_available(ctx) { return false }
                view.accept_zmodem_download(transfer, directory, OverwritePolicy::Skip, ctx);
                true
            }).ok());
            if result != Some(true) {
                ZmodemCrossTransfer::handle(ctx).update(ctx, |me, ctx| { me.cancel_terminal(source.id(), ctx); });
            }
        }
        CrossTransferEvent::PrepareTarget(id) => prepare_target(*id, ctx),
        CrossTransferEvent::UploadRequested(id, transfer) => {
            let Some(binding) = ZmodemCrossTransfer::as_ref(ctx).bindings.get(id) else { return };
            if binding.flow.phase != Phase::Forwarding || binding.flow.target_transfer != Some(*transfer) { return }
            let target = binding.target.clone();
            let paths = binding.paths.clone();
            let transfer = *transfer;
            let accepted = target.upgrade(ctx).and_then(|target| target.try_update(ctx, |view, ctx| {
                if !view.zmodem_available(ctx) || view.zmodem_active_id() != Some(transfer) { return false }
                view.zmodem_transfer.start(transfer);
                view.send_zmodem_control(Control::Upload { id: transfer, paths }, ctx);
                true
            }).ok());
            if accepted != Some(true) {
                ZmodemCrossTransfer::handle(ctx).update(ctx, |me, ctx| { me.cancel_terminal(target.id(), ctx); });
            }
        }
        CrossTransferEvent::Cancel(terminal, id) => {
            if let Some(terminal) = terminal.upgrade(ctx) {
                let _ = terminal.try_update(ctx, |view, ctx| {
                    view.zmodem_transfer.cancel(*id);
                    view.send_zmodem_control(Control::Cancel { id: *id }, ctx);
                    ctx.notify();
                });
            }
        }
        CrossTransferEvent::Result(terminal, message, success) => {
            if let Some(terminal) = terminal.upgrade(ctx) {
                let window = terminal.window_id(ctx);
                let toast = if *success { DismissibleToast::success(message.clone()) }
                    else { DismissibleToast::error(message.clone()) };
                ToastStack::handle(ctx).update(ctx, |stack, ctx| {
                    stack.add_ephemeral_toast(toast, window, ctx);
                });
            }
        }
    }
}

fn prepare_target(id: CrossTransferId, ctx: &mut AppContext) {
    let Some(binding) = ZmodemCrossTransfer::as_ref(ctx).bindings.get(&id) else { return };
    if binding.flow.phase != Phase::PreparingTarget { return }
    let target = binding.target.clone();
    let identity = binding.target_identity.clone();
    let paths = binding.paths.clone();
    let result = target.upgrade(ctx).and_then(|target| target.try_update(ctx, |view, ctx| {
        if !view.zmodem_available(ctx) || view.zmodem_is_busy() { return Err(()) }
        if identity.is_none() || view.zmodem_shell_identity(ctx) != identity {
            return Ok(None);
        }
        view.start_explicit_zmodem_upload(paths, ctx).map(|id| Ok(Some(id))).unwrap_or(Err(()))
    }).ok());
    ZmodemCrossTransfer::handle(ctx).update(ctx, |me, ctx| {
        let Some(binding) = me.bindings.get_mut(&id) else { return };
        match result {
            Some(Ok(Some(transfer))) => { binding.flow.bind_target(transfer); }
            Some(Ok(None)) => {
                binding.flow.phase = Phase::AwaitingTarget;
                binding.deadline = Instant::now() + CHOICE_TIMEOUT;
            }
            Some(Err(())) | None => {
                me.finish(id, "Target terminal is no longer available".into(), false, ctx);
            }
        }
        ctx.emit(CrossTransferEvent::Changed);
    });
}

#[derive(Clone)]
struct TargetChoice {
    terminal: WeakViewHandle<TerminalView>,
    label: String,
}

fn target_choices(source: EntityId, ctx: &AppContext) -> Vec<TargetChoice> {
    let mut choices: Vec<_> = visible_terminals(ctx).into_iter().filter_map(|terminal| {
        if terminal.id() == source || ZmodemCrossTransfer::as_ref(ctx).is_reserved(terminal.id()) { return None }
        let view = terminal.try_as_ref(ctx)?;
        if !view.zmodem_available(ctx) || view.zmodem_is_busy() { return None }
        Some(TargetChoice { terminal: terminal.downgrade(), label: terminal_label(view, ctx) })
    }).collect();
    choices.sort_by(|left, right| left.label.cmp(&right.label).then(left.terminal.id().cmp(&right.terminal.id())));
    choices
}

fn terminal_label(view: &TerminalView, ctx: &AppContext) -> String {
    let title = view.pane_configuration().as_ref(ctx).title().to_owned();
    let host = view.active_block_session_id()
        .and_then(|id| view.sessions_model().as_ref(ctx).get(id))
        .map(|session| format!("{}@{}", session.user(), session.hostname()));
    display_text(&match (title.is_empty(), host) {
        (false, Some(host)) => format!("{title} ({host})"),
        (false, None) => title,
        (true, Some(host)) => host,
        (true, None) => "Terminal".to_owned(),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CrossTransferPickerAction {
    Select(usize),
    Confirm,
    Cancel,
}

pub(crate) enum CrossTransferPickerEvent {
    Close,
    Armed(CrossTransferId),
}

pub(crate) struct CrossTransferPickerBody {
    source: TargetChoice,
    choices: Vec<TargetChoice>,
    selected: usize,
    dropdown: ViewHandle<FilterableDropdown<CrossTransferPickerAction>>,
    confirm: ViewHandle<ActionButton>,
    cancel: ViewHandle<ActionButton>,
    error: Option<String>,
}

impl CrossTransferPickerBody {
    fn new(source: WeakViewHandle<TerminalView>, source_title: String, ctx: &mut ViewContext<Self>) -> Self {
        let choices = target_choices(source.id(), ctx);
        let dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = FilterableDropdown::new(ctx);
            dropdown.set_top_bar_max_width(360.);
            dropdown.set_menu_width(360., ctx);
            dropdown.set_items(choices.iter().enumerate().map(|(index, choice)| {
                DropdownItem::new(choice.label.clone(), CrossTransferPickerAction::Select(index))
            }).collect(), ctx);
            if choices.is_empty() { dropdown.set_disabled(ctx); }
            else { dropdown.set_selected_by_index(0, ctx); }
            dropdown
        });
        ctx.subscribe_to_view(&dropdown, |_, _, event, ctx| {
            if let FilterableDropdownEvent::Close = event { ctx.focus_self(); }
        });
        let confirm = ctx.add_typed_action_view(|ctx| {
            let mut button = ActionButton::new("Select target", PrimaryTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(CrossTransferPickerAction::Confirm);
            });
            button.set_disabled(choices.is_empty(), ctx);
            button
        });
        let cancel = ctx.add_typed_action_view(|_| {
            ActionButton::new("Cancel", NakedTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(CrossTransferPickerAction::Cancel);
            })
        });
        Self { source: TargetChoice { terminal: source, label: source_title }, choices, selected: 0,
            dropdown, confirm, cancel, error: None }
    }
}

impl Entity for CrossTransferPickerBody { type Event = CrossTransferPickerEvent; }

impl TypedActionView for CrossTransferPickerBody {
    type Action = CrossTransferPickerAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            CrossTransferPickerAction::Select(index) => { self.selected = *index; ctx.notify(); }
            CrossTransferPickerAction::Cancel => ctx.emit(CrossTransferPickerEvent::Close),
            CrossTransferPickerAction::Confirm => {
                let Some(target) = self.choices.get(self.selected) else { return };
                let result = ZmodemCrossTransfer::handle(ctx).update(ctx, |me, ctx| {
                    me.begin(&self.source, target, ctx)
                });
                match result {
                    Ok(id) => ctx.emit(CrossTransferPickerEvent::Armed(id)),
                    Err(error) => { self.error = Some(error.to_string()); ctx.notify(); }
                }
            }
        }
    }
}

impl View for CrossTransferPickerBody {
    fn ui_name() -> &'static str { "CrossTransferPickerBody" }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let mut column = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        column.add_child(appearance.ui_builder().label(format!("From: {}", self.source.label)).build().finish());
        column.add_child(Container::new(ChildView::new(&self.dropdown).finish()).with_vertical_margin(20.).finish());
        if self.choices.is_empty() || self.error.is_some() {
            column.add_child(appearance.ui_builder().label(self.error.clone()
                .unwrap_or_else(|| "No available target terminals".to_owned())).build().finish());
        }
        column.add_child(Flex::row().with_main_axis_alignment(MainAxisAlignment::End)
            .with_child(ChildView::new(&self.cancel).finish())
            .with_child(ChildView::new(&self.confirm).finish()).finish());
        column.finish()
    }
}

/// The owner renders this view as an overlay and closes it on either event variant.
pub(crate) struct CrossTransferPicker {
    modal: ViewHandle<Modal<CrossTransferPickerBody>>,
}

impl CrossTransferPicker {
    pub fn new(source: WeakViewHandle<TerminalView>, source_title: String, ctx: &mut ViewContext<Self>) -> Self {
        let body = ctx.add_typed_action_view(|ctx| CrossTransferPickerBody::new(source, display_text(&source_title), ctx));
        ctx.subscribe_to_view(&body, |_, _, event, ctx| match event {
            CrossTransferPickerEvent::Close => ctx.emit(CrossTransferPickerEvent::Close),
            CrossTransferPickerEvent::Armed(id) => ctx.emit(CrossTransferPickerEvent::Armed(*id)),
        });
        let modal = ctx.add_typed_action_view(|ctx| {
            Modal::new(Some("Transfer to terminal".to_owned()), body, ctx)
                .with_modal_style(UiComponentStyles { height: Some(300.), ..Default::default() })
                .with_body_style(UiComponentStyles { height: Some(230.), ..Default::default() })
        });
        ctx.subscribe_to_view(&modal, |_, _, event, ctx| match event {
            ModalEvent::Close => ctx.emit(CrossTransferPickerEvent::Close),
        });
        Self { modal }
    }
}

impl Entity for CrossTransferPicker { type Event = CrossTransferPickerEvent; }

impl View for CrossTransferPicker {
    fn ui_name() -> &'static str { "CrossTransferPicker" }
    fn render(&self, _: &AppContext) -> Box<dyn Element> { ChildView::new(&self.modal).finish() }
}

#[cfg(test)]
#[path = "zmodem_cross_transfer_tests.rs"]
mod tests;
