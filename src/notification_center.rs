use std::{mem, sync::mpsc::Sender};

use core_foundation::runloop::CFRunLoopRun;
use icrate::{
    objc2::{
        declare_class, msg_send_id, mutability,
        rc::{Allocated, Id},
        sel, ClassType, DeclaredClass, Encode, Encoding,
    },
    AppKit::{self, NSApplication, NSRunningApplication, NSWorkspace, NSWorkspaceApplicationKey},
    Foundation::{MainThreadMarker, NSNotification, NSNotificationCenter, NSObject},
};
use log::{trace, warn};

use crate::app::{self, NSRunningApplicationExt};
use crate::Event;

pub(crate) fn watch_for_notifications(events_tx: Sender<Event>) {
    #[repr(C)]
    struct Instance {
        events_tx: &'static mut Sender<Event>,
    }

    unsafe impl Encode for Instance {
        const ENCODING: Encoding = Encoding::Object;
    }

    declare_class! {
        struct NotificationHandler;

        // SAFETY:
        // - The superclass NSObject does not have any subclassing requirements.
        // - Interior mutability is a safe default.
        // - `NotificationHandler` does not implement `Drop`.
        unsafe impl ClassType for NotificationHandler {
            type Super = NSObject;
            type Mutability = mutability::InteriorMutable;
            const NAME: &'static str = "NotificationHandler";
        }

        impl DeclaredClass for NotificationHandler {
            type Ivars = Box<Instance>;
        }

        // SAFETY: Each of these method signatures must match their invocations.
        unsafe impl NotificationHandler {
            #[method_id(initWith:)]
            fn init(this: Allocated<Self>, instance: Instance) -> Option<Id<Self>> {
                let this = this.set_ivars(Box::new(instance));
                unsafe { msg_send_id![super(this), init] }
            }

            #[method(handleActivated:)]
            fn handle_activated(&self, notif: &NSNotification) {
                trace!("{notif:#?}");
                if let Some(app) = self.running_application(notif) {
                    self.send_event(Event::ApplicationActivated(app.processIdentifier()));
                }
            }

            #[method(handleLaunched:)]
            fn handle_launched(&self, notif: &NSNotification) {
                trace!("{notif:#?}");
                if let Some(app) = self.running_application(notif) {
                    app::spawn_app_thread(app.processIdentifier(), self.events_tx().clone());
                }
            }

            #[method(handleTerminated:)]
            fn handle_terminated(&self, notif: &NSNotification) {
                trace!("{notif:#?}");
                if let Some(app) = self.running_application(notif) {
                    self.send_event(Event::ApplicationTerminated(app.processIdentifier()));
                }
            }

            #[method(handleScreenChanged:)]
            fn handle_screen_changed(&self, notif: &NSNotification) {
                trace!("{notif:#?}");
                self.send_event(Event::ScreenParametersChanged);
            }
        }
    }

    impl NotificationHandler {
        fn new(events_tx: Sender<Event>) -> Id<Self> {
            let events_tx = Box::leak(Box::new(events_tx));
            let instance = Instance { events_tx };
            unsafe { msg_send_id![Self::alloc(), initWith: instance] }
        }

        fn send_event(&self, event: Event) {
            if let Err(err) = self.events_tx().send(event) {
                warn!("Failed to send event: {err:?}");
            }
        }

        fn events_tx(&self) -> &Sender<Event> {
            self.ivars().events_tx
        }

        fn running_application(&self, notif: &NSNotification) -> Option<Id<NSRunningApplication>> {
            let info = unsafe { notif.userInfo() };
            let Some(info) = info else {
                warn!("Got app notification without user info: {notif:?}");
                return None;
            };
            let app = unsafe { info.valueForKey(NSWorkspaceApplicationKey) };
            let Some(app) = app else {
                warn!("Got app notification without app object: {notif:?}");
                return None;
            };
            assert!(app.class() == NSRunningApplication::class());
            let app: Id<NSRunningApplication> = unsafe { mem::transmute(app) };
            Some(app)
        }
    }

    let handler = NotificationHandler::new(events_tx);

    // SAFETY: Selector must have signature fn(&self, &NSNotification)
    let register_unsafe = |selector, notif_name, center: &Id<NSNotificationCenter>, object| unsafe {
        center.addObserver_selector_name_object(&handler, selector, Some(notif_name), Some(object));
    };

    let workspace = &unsafe { NSWorkspace::sharedWorkspace() };
    let workspace_center = &unsafe { workspace.notificationCenter() };
    let default_center = &unsafe { NSNotificationCenter::defaultCenter() };
    let shared_app = &NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
    unsafe {
        use AppKit::*;
        register_unsafe(
            sel!(handleActivated:),
            NSWorkspaceDidActivateApplicationNotification,
            workspace_center,
            workspace,
        );
        register_unsafe(
            sel!(handleLaunched:),
            NSWorkspaceDidLaunchApplicationNotification,
            workspace_center,
            workspace,
        );
        register_unsafe(
            sel!(handleTerminated:),
            NSWorkspaceDidTerminateApplicationNotification,
            workspace_center,
            workspace,
        );
        register_unsafe(
            sel!(handleScreenChanged:),
            NSApplicationDidChangeScreenParametersNotification,
            default_center,
            shared_app,
        );
    };

    unsafe {
        CFRunLoopRun();
    }
}
