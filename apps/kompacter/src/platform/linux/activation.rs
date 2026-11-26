use slint::winit_030::winit::raw_window_handle::{
    HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
};
use slint::winit_030::winit::window::{UserAttentionType, Window};
use slint::winit_030::WinitWindowAccessor;
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
use wayland_client::{delegate_noop, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::XdgActivationV1;

/// Wayland leaves activation up to the compositor. Forward the desktop
/// launcher's token to the panel's current surface, when one was supplied.
pub fn activate_window(window: &slint::Window, token: Option<&str>) {
    window.with_winit_window(|window| {
        let is_wayland = window
            .display_handle()
            .is_ok_and(|handle| matches!(handle.as_raw(), RawDisplayHandle::Wayland(_)));
        if is_wayland
            && !window.has_focus()
            && !token.is_some_and(|token| activate_wayland(window, token).is_some())
        {
            window.request_user_attention(Some(UserAttentionType::Informational));
        }
    });
}

fn activate_wayland(window: &Window, token: &str) -> Option<()> {
    let RawDisplayHandle::Wayland(display) = window.display_handle().ok()?.as_raw() else {
        return None;
    };
    let RawWindowHandle::Wayland(surface) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    // Keep the winit window borrowed until the guest connection and proxy
    // have been dropped. The guest connection does not own either handle.
    let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
    let connection = Connection::from_backend(backend);
    let surface_id =
        unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.surface.as_ptr().cast()) }
            .ok()?;
    let surface = WlSurface::from_id(&connection, surface_id).ok()?;
    let (globals, queue) = registry_queue_init::<ActivationEvents>(&connection).ok()?;
    let manager: XdgActivationV1 = globals.bind(&queue.handle(), 1..=1, ()).ok()?;
    manager.activate(token.to_owned(), &surface);
    manager.destroy();
    connection.flush().ok()?;
    Some(())
}

struct ActivationEvents;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ActivationEvents {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(ActivationEvents: ignore XdgActivationV1);
