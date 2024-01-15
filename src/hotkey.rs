use crate::reactor::{Command, Event, Sender};

use livesplit_hotkey::Hook;
pub use livesplit_hotkey::{Hotkey, KeyCode, Modifiers};

pub struct HotkeyManager {
    hook: Hook,
    events_tx: Sender<Event>,
}

impl HotkeyManager {
    pub fn new(events_tx: Sender<Event>) -> Self {
        let hook = Hook::new_consuming().unwrap();
        HotkeyManager { hook, events_tx }
    }

    pub fn register(&self, modifiers: Modifiers, key_code: KeyCode, cmd: Command) {
        let events_tx = self.events_tx.clone();
        self.hook
            .register(Hotkey { modifiers, key_code }, move || {
                events_tx.send(Event::Command(cmd.clone())).unwrap()
            })
            .unwrap();
    }
}