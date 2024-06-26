//! The WM Controller handles major events like enabling and disabling the
//! window manager on certain spaces and launching app threads. It also
//! controls hotkey registration.

use std::{collections::HashSet, path::PathBuf};

use accessibility_sys::pid_t;
use tracing::{debug, instrument, Span};

pub type Sender = tokio::sync::mpsc::UnboundedSender<(Span, WmEvent)>;
type WeakSender = tokio::sync::mpsc::WeakUnboundedSender<(Span, WmEvent)>;
type Receiver = tokio::sync::mpsc::UnboundedReceiver<(Span, WmEvent)>;

use crate::{
    actor::{self, app::AppInfo, reactor},
    sys::{hotkey::HotkeyManager, screen::SpaceId},
};

#[derive(Debug)]
pub enum WmEvent {
    AppEventsRegistered,
    AppLaunch(pid_t, AppInfo),
    ReactorEvent(reactor::Event),
    Command(WmCommand),
}

#[derive(Debug, Clone)]
pub enum WmCommand {
    ToggleSpaceActivated,
    ReactorCommand(reactor::Command),
}

pub struct Config {
    pub one_space: bool,
    pub restore_file: PathBuf,
}

pub struct WmController {
    config: Config,
    events_tx: reactor::Sender,
    receiver: Receiver,
    sender: WeakSender,
    starting_space: Option<SpaceId>,
    cur_space: Vec<Option<SpaceId>>,
    disabled_spaces: HashSet<SpaceId>,
    hotkeys: Option<HotkeyManager>,
}

impl WmController {
    pub fn new(config: Config, events_tx: reactor::Sender) -> (Self, Sender) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let this = Self {
            config,
            events_tx,
            receiver,
            sender: sender.downgrade(),
            starting_space: None,
            cur_space: Vec::new(),
            disabled_spaces: HashSet::new(),
            hotkeys: None,
        };
        (this, sender)
    }

    pub async fn run(mut self) {
        while let Some((span, event)) = self.receiver.recv().await {
            let _guard = span.enter();
            self.handle_event(event);
        }
    }

    #[instrument(skip(self))]
    pub fn handle_event(&mut self, event: WmEvent) {
        debug!("handle_event");
        use self::WmCommand::*;
        use self::WmEvent::*;
        use reactor::Event;
        match event {
            AppEventsRegistered => {
                actor::app::spawn_initial_app_threads(self.events_tx.clone());
            }
            AppLaunch(pid, info) => {
                actor::app::spawn_app_thread(pid, info, self.events_tx.clone());
            }
            ReactorEvent(mut event) => {
                if let Event::SpaceChanged(spaces) | Event::ScreenParametersChanged(_, spaces) =
                    &mut event
                {
                    self.handle_space_changed(spaces);
                    self.apply_space_activation(spaces);
                }
                self.send_event(event);
            }
            Command(ToggleSpaceActivated) => {
                for space in &self.cur_space {
                    let Some(space) = space else { return };
                    if !self.disabled_spaces.remove(space) {
                        self.disabled_spaces.insert(*space);
                    }
                }
                let mut spaces = self.cur_space.clone();
                self.apply_space_activation(&mut spaces);
                self.send_event(Event::SpaceChanged(spaces));
            }
            Command(ReactorCommand(cmd)) => {
                self.send_event(Event::Command(cmd));
            }
        }
    }

    fn handle_space_changed(&mut self, spaces: &[Option<SpaceId>]) {
        self.cur_space = spaces.iter().copied().collect();
        let Some(&Some(space)) = spaces.first() else { return };
        if self.starting_space.is_none() {
            self.starting_space = Some(space);
            self.register_hotkeys();
        } else if self.config.one_space {
            let Some(&Some(space)) = spaces.first() else { return };
            if Some(space) == self.starting_space {
                self.register_hotkeys();
            } else {
                self.unregister_hotkeys();
            }
        }
    }

    fn apply_space_activation(&self, spaces: &mut [Option<SpaceId>]) {
        for space in spaces {
            match space {
                Some(_) if self.config.one_space && *space != self.starting_space => *space = None,
                Some(sp) if self.disabled_spaces.contains(sp) => *space = None,
                _ => (),
            }
        }
    }

    fn send_event(&mut self, event: reactor::Event) {
        _ = self.events_tx.send((Span::current().clone(), event));
    }

    fn register_hotkeys(&mut self) {
        debug!("register_hotkeys");
        use crate::metrics::MetricsCommand::*;
        use crate::model::Direction::*;
        use crate::model::Orientation;
        use crate::sys::hotkey::{KeyCode, Modifiers};
        use actor::layout::LayoutCommand::*;
        use actor::reactor::Command;

        use KeyCode::*;
        const ALT: Modifiers = Modifiers::ALT;
        const SHIFT: Modifiers = Modifiers::SHIFT;

        let mgr = HotkeyManager::new(self.sender.upgrade().unwrap());
        mgr.register(ALT, KeyW, Command::Hello);
        //mgr.register(ALT, KeyS, Command::Layout(Shuffle));
        mgr.register(ALT, KeyA, Command::Layout(Ascend));
        mgr.register(ALT, KeyD, Command::Layout(Descend));
        mgr.register(ALT, KeyH, Command::Layout(MoveFocus(Left)));
        mgr.register(ALT, KeyJ, Command::Layout(MoveFocus(Down)));
        mgr.register(ALT, KeyK, Command::Layout(MoveFocus(Up)));
        mgr.register(ALT, KeyL, Command::Layout(MoveFocus(Right)));
        mgr.register(ALT | SHIFT, KeyH, Command::Layout(MoveNode(Left)));
        mgr.register(ALT | SHIFT, KeyJ, Command::Layout(MoveNode(Down)));
        mgr.register(ALT | SHIFT, KeyK, Command::Layout(MoveNode(Up)));
        mgr.register(ALT | SHIFT, KeyL, Command::Layout(MoveNode(Right)));
        mgr.register(ALT, Equal, Command::Layout(Split(Orientation::Vertical)));
        mgr.register(
            ALT,
            Backslash,
            Command::Layout(Split(Orientation::Horizontal)),
        );
        mgr.register(ALT, KeyS, Command::Layout(Group(Orientation::Vertical)));
        mgr.register(ALT, KeyT, Command::Layout(Group(Orientation::Horizontal)));
        mgr.register(ALT, KeyE, Command::Layout(Ungroup));
        mgr.register(ALT, KeyM, Command::Metrics(ShowTiming));
        mgr.register(ALT | SHIFT, KeyD, Command::Layout(Debug));
        mgr.register(ALT | SHIFT, KeyS, Command::Layout(Serialize));
        mgr.register(
            ALT | SHIFT,
            KeyE,
            Command::Layout(SaveAndExit(self.config.restore_file.clone())),
        );
        mgr.register_wm(ALT, KeyZ, WmCommand::ToggleSpaceActivated);

        self.hotkeys = Some(mgr);
    }

    fn unregister_hotkeys(&mut self) {
        debug!("unregister_hotkeys");
        self.hotkeys = None;
    }
}
