use gtk4::gdk::Display;
use gtk4::gio::File;
use gtk4::glib;
use gtk4::{Application, Box as GtkBox, CssProvider, Label, Orientation, prelude::*};
use gtk4_layer_shell::LayerShell;
use niri_ipc::{socket::Socket, Action, Request};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use self_update::cargo_crate_version;

mod altdock;
use altdock::spawn_altdock;

const APP_ID: &str = "ekah.scu.altdock";

fn load_css() {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let own = format!("{home}/.config/altdock/dark.css");
    let shared = format!("{home}/.config/capsule/dark.css");
    let path = if std::path::Path::new(&own).exists() { own } else { shared };

    let css = CssProvider::new();
    css.load_from_file(&File::for_path(&path));
    if let Some(display) = Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &css,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

const PROBE_PATH: &str = "/var/lib/cynager/info.probe";

fn read_shellout(path: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let set_start = content.find(":set")?;
    let set_body = &content[set_start + 4..];
    let set_body = &set_body[..set_body.find(":end")?];

    for line in set_body.lines() {
        if let Some(rest) = line.trim().strip_prefix("shellout") {
            if let Some(val) = rest.trim().strip_prefix(':') {
                return Some(val.trim().to_string());
            }
        }
    }
    None
}

fn pinned_monitor() -> Option<gtk4::gdk::Monitor> {
    let connector = read_shellout(PROBE_PATH)?;
    if connector.is_empty() || connector == "default" {
        return None;
    }
    let display = Display::default()?;
    let monitors = display.monitors();
    (0..monitors.n_items())
        .filter_map(|i| monitors.item(i)?.downcast::<gtk4::gdk::Monitor>().ok())
        .find(|m| m.connector().map(|c| c == connector.as_str()).unwrap_or(false))
}

fn niri_switch_workspace(down: bool) {
    std::thread::spawn(move || {
        let action = if down {
            Action::FocusWorkspaceDown {}
        } else {
            Action::FocusWorkspaceUp {}
        };
        match Socket::connect() {
            Ok(mut sock) => {
                if let Err(e) = sock.send(Request::Action(action)) {
                    eprintln!("niri ipc send failed: {e}");
                }
            }
            Err(e) => eprintln!("niri ipc connect failed: {e}"),
        }
    });
}

fn activate(app: &Application) {
    load_css();

    let dockbox = GtkBox::builder()
        .orientation(Orientation::Horizontal)
        .spacing(10)
        .halign(gtk4::Align::Center)
        .valign(gtk4::Align::Start)
        .visible(false)
        .build();
    dockbox.add_css_class("dockBox");
    let dockbox_anim = dockbox.clone();

    let scroll = gtk4::EventControllerScroll::new(
        gtk4::EventControllerScrollFlags::VERTICAL,
    );
    let acc: Rc<Cell<f64>> = Rc::new(Cell::new(0.0));
    scroll.connect_scroll(move |_, _, dy| {
        let total = acc.get() + dy;
        if total >= 1.0 {
            acc.set(0.0);
            niri_switch_workspace(true);
        } else if total <= -1.0 {
            acc.set(0.0);
            niri_switch_workspace(false);
        } else {
            acc.set(total);
        }
        glib::Propagation::Stop
    });
    dockbox_anim.add_controller(scroll);

    let dockerhover = GtkBox::new(Orientation::Horizontal, 0);
    dockerhover.set_size_request(200, 5);
    dockerhover.set_hexpand(true);
    dockerhover.set_halign(gtk4::Align::Center);
    dockerhover.append(
        &Label::builder()
            .css_classes(["dummytxt"])
            .label("Dont mess the css gng")
            .build(),
    );

    let dock = GtkBox::new(Orientation::Vertical, 5);
    dock.append(&dockerhover);
    dock.append(&dockbox_anim);

    let dock_win = spawn_altdock(app, dockbox);
    dock_win.set_child(Some(&dock));
    if let Some(m) = pinned_monitor() {
        dock_win.set_monitor(Some(&m));
    }

    let hover_ctrl = gtk4::EventControllerMotion::new();
    let pending: Rc<Cell<bool>> = Rc::new(Cell::new(false));

    let pending_enter = Rc::clone(&pending);
    hover_ctrl.connect_enter(move |_, _, _| {
        pending_enter.set(true);
        let dockbox_anim = dockbox_anim.clone();
        let pending_timeout = Rc::clone(&pending_enter);
        glib::timeout_add_local(Duration::from_millis(300), move || {
            if pending_timeout.get() {
                dockbox_anim.remove_css_class("dockleave");
                dockbox_anim.set_visible(true);
                dockbox_anim.add_css_class("dockcum");
            }
            glib::ControlFlow::Break
        });
    });
    let pending_leave = Rc::clone(&pending);
    hover_ctrl.connect_leave(move |_| pending_leave.set(false));
    dockerhover.add_controller(hover_ctrl);
}

struct UpdateOutcome {
    updated: bool,
    version: String,
}

const ALTDOCK_BIN_PATH: &str = "/usr/bin/altdock";

fn ensure_writable(path: &str) -> Result<(), String> {
    use std::ffi::CString;

    let c_path = CString::new(path).map_err(|e| e.to_string())?;

    let path_obj = std::path::Path::new(path);
    if path_obj.exists() {
        let writable = unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 };
        if !writable {
            return Err(format!(
                "no write permission for '{}'. Try: sudo altdock --update",
                path
            ));
        }
    }

    if let Some(parent) = path_obj.parent() {
        let c_parent = CString::new(parent.to_string_lossy().to_string())
            .map_err(|e| e.to_string())?;
        let dir_writable = unsafe { libc::access(c_parent.as_ptr(), libc::W_OK) == 0 };
        if !dir_writable {
            return Err(format!(
                "no write permission for directory '{}'. Try: sudo altdock --update",
                parent.display()
            ));
        }
    }

    Ok(())
}

fn check_for_updates() -> Result<UpdateOutcome, Box<dyn std::error::Error>> {
    let status = self_update::backends::github::Update::configure()
        .repo_owner("ekahPruthvi")
        .repo_name("altdock")
        .bin_name("altdock")
        .target("x86_64")
        .bin_install_path(ALTDOCK_BIN_PATH)
        .show_download_progress(true)
        .no_confirm(true)
        .current_version(cargo_crate_version!())
        .build()?
        .update()?;

    Ok(UpdateOutcome {
        updated: status.is_updated(),
        version: status.version().to_string(),
    })
}

fn run_update() -> i32 {
    println!("Checking for updates...");

    if let Err(msg) = ensure_writable(ALTDOCK_BIN_PATH) {
        eprintln!("Cannot update altdock: {}", msg);
        return 1;
    }

    match check_for_updates() {
        Ok(outcome) => {
            if outcome.updated {
                println!("altdock updated to version {}", outcome.version);
            } else {
                println!("altdock is already up to date ({})", outcome.version);
            }
            0
        }
        Err(e) => {
            eprintln!("Update failed: {}", e);
            1
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--version") | Some("-V") => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        }
        Some("--update") | Some("-U") => std::process::exit(run_update()),
        Some("--snapped") | Some("-f") => std::process::exit(altdock::cli_list_snapped()),
        Some("--unsnap") | Some("-u") => match args.get(2) {
            Some(name) => std::process::exit(altdock::cli_unsnap(name)),
            None => {
                eprintln!("usage: altdock -u <app name>");
                std::process::exit(2);
            }
        },
        Some("--help") | Some("-H") => {
            println!("usage: altdock [option]");
            println!("  -V, --version        print version");
            println!("  -U, --update         self-update");
            println!("  -f, --snapped        list snapped (tiled) apps");
            println!("  -u, --unsnap <app>   unsnap an app, restoring its size and position");
            std::process::exit(0);
        }
        _ => {}
    }

    let app = Application::new(Some(APP_ID), Default::default());
    app.connect_activate(activate);
    app.run();
}