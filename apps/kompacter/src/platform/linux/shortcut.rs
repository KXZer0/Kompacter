//! The global shortcut through the desktop's `org.freedesktop.portal.GlobalShortcuts`
//! portal (GNOME 48+, KDE Plasma, and others). Wayland gives apps no other way to hear
//! keys pressed in other windows. One background thread owns the portal session and
//! another waits for `Activated`. Without the portal (older desktops) the settings say
//! how to make a system shortcut that runs Kompacter instead.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use crate::platform::InputEvent;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
/// Kompacter's one shortcut, as the portal knows it.
const SHORTCUT_ID: &str = "quick-bar";

type Callback = Box<dyn Fn(InputEvent) + Send + Sync>;
static ON_INPUT: OnceLock<Callback> = OnceLock::new();
/// (Windows modifier flags, virtual key), as `settings::shortcut_keys` gives them.
type Key = Option<(u32, u32)>;
/// Requests for the worker thread: the shortcut to bind (None: none).
static REQUESTS: Mutex<Option<Sender<Key>>> = Mutex::new(None);
/// The session the bound shortcut belongs to (its `Activated` signals count).
static SESSION: Mutex<Option<String>> = Mutex::new(None);
static TOKEN: AtomicU32 = AtomicU32::new(0);

fn emit(e: InputEvent) {
    if let Some(f) = ON_INPUT.get() {
        f(e);
    }
}

/// The portal's trigger for Windows modifier flags (Alt 1, Ctrl 2, Shift 4, Win 8) and
/// a virtual key, e.g. "CTRL+ALT+k" (XDG shortcuts syntax: modifiers, then a keysym).
pub fn portal_trigger(mods: u32, vk: u32) -> Option<String> {
    let key = match vk {
        0x41..=0x5A => char::from(vk as u8).to_ascii_lowercase().to_string(),
        0x30..=0x39 => char::from(vk as u8).to_string(),
        0x20 => "space".into(),
        _ => return None,
    };
    let mut trigger = String::new();
    for (flag, name) in [(2, "CTRL"), (1, "ALT"), (4, "SHIFT"), (8, "LOGO")] {
        if mods & flag != 0 {
            trigger.push_str(name);
            trigger.push('+');
        }
    }
    trigger.push_str(&key);
    Some(trigger)
}

/// Start talking to the portal. `on_event` runs on a background thread; keep it quick.
pub fn start_input(on_event: impl Fn(InputEvent) + Send + Sync + 'static) {
    if ON_INPUT.set(Box::new(on_event)).is_err() {
        return;
    }
    let (tx, rx) = mpsc::channel();
    *REQUESTS.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    std::thread::spawn(move || {
        let portal = match Portal::connect() {
            Ok(portal) => portal,
            Err(error) => {
                eprintln!("Kompacter: no global shortcuts portal ({error})");
                REQUESTS.lock().unwrap_or_else(|e| e.into_inner()).take();
                emit(InputEvent::ShortcutUnsupported);
                return;
            }
        };
        portal.listen();
        portal.serve(rx);
    });
}

/// Bind the global shortcut: (modifiers, virtual key), or None for no shortcut.
/// The answer comes back as `InputEvent::ShortcutTaken`.
pub fn set_shortcut(key: Key) {
    if let Some(tx) = REQUESTS.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        let _ = tx.send(key);
    }
}

struct Portal {
    connection: Connection,
    shortcuts: Proxy<'static>,
    /// The unique bus name as used in request and session paths (":1.42" → "1_42").
    sender: String,
}

impl Portal {
    fn connect() -> zbus::Result<Self> {
        let connection = Connection::session()?;
        // A host app has to say who it is before its first portal call (newer portals
        // ask for it; older ones, or a sandbox, already know and refuse: that's fine).
        if let Ok(registry) = Proxy::new(
            &connection,
            PORTAL,
            PORTAL_PATH,
            "org.freedesktop.host.portal.Registry",
        ) {
            let options: HashMap<&str, Value> = HashMap::new();
            let _ = registry.call::<_, _, ()>("Register", &(super::APP_ID, options));
        }
        let shortcuts = zbus::blocking::proxy::Builder::<Proxy<'static>>::new(&connection)
            .destination(PORTAL)?
            .path(PORTAL_PATH)?
            .interface(SHORTCUTS)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()?;
        // Fails when the interface isn't there (no portal backend implements it).
        shortcuts.get_property::<u32>("version")?;
        let sender = connection
            .unique_name()
            .map(|n| n.as_str().trim_start_matches(':').replace('.', "_"))
            .unwrap_or_default();
        Ok(Self {
            connection,
            shortcuts,
            sender,
        })
    }

    /// Wait for the shortcut on its own thread.
    fn listen(&self) {
        let shortcuts = self.shortcuts.clone();
        std::thread::spawn(move || {
            let Ok(signals) = shortcuts.receive_signal("Activated") else {
                return;
            };
            for message in signals {
                type Activated = (OwnedObjectPath, String, u64, HashMap<String, OwnedValue>);
                let Ok((session, id, _, _)) = message.body().deserialize::<Activated>() else {
                    continue;
                };
                let ours = SESSION.lock().unwrap_or_else(|e| e.into_inner()).as_deref()
                    == Some(session.as_str());
                if ours && id == SHORTCUT_ID {
                    emit(InputEvent::Shortcut);
                }
            }
        });
    }

    fn serve(&self, rx: Receiver<Key>) {
        while let Ok(mut key) = rx.recv() {
            // Only the latest choice matters.
            while let Ok(next) = rx.try_recv() {
                key = next;
            }
            self.close_session();
            let Some((mods, vk)) = key else {
                emit(InputEvent::ShortcutTaken(false));
                continue;
            };
            let bound = portal_trigger(mods, vk)
                .ok_or_else(|| zbus::Error::Failure("no trigger for this key".into()))
                .and_then(|trigger| self.bind(&trigger));
            if let Err(error) = &bound {
                eprintln!("Kompacter: couldn't bind the global shortcut ({error})");
            }
            emit(InputEvent::ShortcutTaken(bound.is_err()));
        }
    }

    fn token(&self) -> String {
        format!(
            "kompacter_{}_{}",
            std::process::id(),
            TOKEN.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// Call a portal method that answers with a `Request` and wait for its results.
    fn request(
        &self,
        method: &str,
        body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
        token: &str,
    ) -> zbus::Result<HashMap<String, OwnedValue>> {
        // Subscribe before calling, so a quick answer isn't missed.
        let path = format!("{PORTAL_PATH}/request/{}/{token}", self.sender);
        let request = Proxy::new(
            &self.connection,
            PORTAL,
            path,
            "org.freedesktop.portal.Request",
        )?;
        let mut answers = request.receive_signal("Response")?;
        self.shortcuts.call::<_, _, OwnedObjectPath>(method, body)?;
        let answer = answers
            .next()
            .ok_or_else(|| zbus::Error::Failure("no answer from the portal".into()))?;
        let (code, results) = answer
            .body()
            .deserialize::<(u32, HashMap<String, OwnedValue>)>()?;
        match code {
            0 => Ok(results),
            1 => Err(zbus::Error::Failure("cancelled".into())),
            _ => Err(zbus::Error::Failure(format!("refused ({code})"))),
        }
    }

    fn bind(&self, trigger: &str) -> zbus::Result<()> {
        let token = self.token();
        let session_token = self.token();
        let options = HashMap::from([
            ("handle_token", Value::from(token.as_str())),
            ("session_handle_token", Value::from(session_token.as_str())),
        ]);
        let results = self.request("CreateSession", &(options,), &token)?;
        let session = match results.get("session_handle").map(|v| &**v) {
            Some(Value::Str(s)) => s.to_string(),
            Some(Value::ObjectPath(p)) => p.to_string(),
            _ => format!("{PORTAL_PATH}/session/{}/{session_token}", self.sender),
        };
        *SESSION.lock().unwrap_or_else(|e| e.into_inner()) = Some(session.clone());

        let token = self.token();
        let shortcut = HashMap::from([
            ("description", Value::from("Open the Kompacter quick bar")),
            ("preferred_trigger", Value::from(trigger)),
        ]);
        let options = HashMap::from([("handle_token", Value::from(token.as_str()))]);
        let results = self.request(
            "BindShortcuts",
            &(
                ObjectPath::try_from(session.as_str())?,
                vec![(SHORTCUT_ID, shortcut)],
                "",
                options,
            ),
            &token,
        )?;
        // The desktop may let the user decline or leave the shortcut unassigned.
        let bound = results
            .get("shortcuts")
            .and_then(|v| {
                <Vec<(String, HashMap<String, OwnedValue>)>>::try_from(v.try_clone().ok()?).ok()
            })
            .is_some_and(|list| list.iter().any(|(id, _)| id == SHORTCUT_ID));
        if bound {
            Ok(())
        } else {
            Err(zbus::Error::Failure("the shortcut wasn't assigned".into()))
        }
    }

    fn close_session(&self) {
        let Some(session) = SESSION.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return;
        };
        if let Ok(proxy) = Proxy::new(
            &self.connection,
            PORTAL,
            session,
            "org.freedesktop.portal.Session",
        ) {
            let _ = proxy.call::<_, _, ()>("Close", &());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::portal_trigger;
    use crate::app::settings::{shortcut_keys, SHORTCUTS};

    #[test]
    fn settings_shortcuts_become_portal_triggers() {
        let triggers: Vec<Option<String>> = SHORTCUTS
            .iter()
            .map(|(id, _)| shortcut_keys(id).and_then(|(m, vk)| portal_trigger(m, vk)))
            .collect();
        assert_eq!(
            triggers,
            [
                None,
                Some("CTRL+ALT+k".into()),
                Some("CTRL+SHIFT+k".into()),
                Some("CTRL+SHIFT+space".into()),
            ]
        );
        assert_eq!(portal_trigger(8, 0x31).as_deref(), Some("LOGO+1"));
        assert_eq!(portal_trigger(2, 0x70), None);
    }
}
