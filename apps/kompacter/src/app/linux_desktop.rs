use std::rc::Rc;

use slint::ComponentHandle;

use crate::frontend::Tray;
use crate::platform::linux::{self, Action, Activation, Primary};

use super::{output, with_app, App};

pub(super) fn attach(app: &Rc<App>) {
    let ui = app.ui();
    ui.set_resident_controls(true);
    let weak = Rc::downgrade(app);
    ui.on_hide_resident(move || {
        if let Some(app) = weak.upgrade() {
            app.close();
        }
    });
    let weak = Rc::downgrade(app);
    ui.on_quit_resident(move || {
        if let Some(app) = weak.upgrade() {
            app.trim_quit();
        }
    });
    linux::watch_tray_host(|| {
        with_app(|app| {
            if let Err(error) = install_tray(app) {
                eprintln!("Kompacter tray: {error}");
            }
        });
    });
}

fn install_tray(app: &Rc<App>) -> Result<(), slint::PlatformError> {
    let tray = Tray::new()?;
    tray.set_has_chooser(true);
    let app_weak = Rc::downgrade(app);
    tray.on_open(move || {
        if let Some(app) = app_weak.upgrade() {
            app.activate(Vec::new());
        }
    });
    let app_weak = Rc::downgrade(app);
    tray.on_choose(move || {
        if let Some(app) = app_weak.upgrade() {
            choose_files(&app);
        }
    });
    tray.on_open_folder(|| {
        if let Ok(dir) = output::app_dir() {
            crate::platform::open_folder(&dir);
        }
    });
    let app_weak = Rc::downgrade(app);
    tray.on_quit(move || {
        if let Some(app) = app_weak.upgrade() {
            app.trim_quit();
        }
    });
    *app.tray.borrow_mut() = Some(tray);
    Ok(())
}

pub(super) fn start(app: &Rc<App>, primary: Primary) {
    let initial = primary.listen(|activation| with_app(move |app| activate(app, activation)));
    activate(app, initial);
}

fn choose_files(app: &Rc<App>) {
    app.show();
    if !app.modal.get() {
        app.pick_files();
    }
}

fn activate(app: &Rc<App>, mut activation: Activation) {
    match activation.action {
        Action::Open => app.activate(activation.take_files()),
        Action::Choose => choose_files(app),
        Action::Quit => {
            app.trim_quit();
            return;
        }
        Action::Quiet => return,
    }
    linux::activate_window(app.ui().window(), activation.token.as_deref());
}
