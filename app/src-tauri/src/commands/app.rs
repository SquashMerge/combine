//! App entry: builds and runs the Tauri application.

use std::sync::Arc;

use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};

use super::terminals::{self, TauriTerminalSink};
use super::{hosts, layout, open_url, panels, spawn, state};
use crate::hosts::service::{HostsService, ServiceDeps};
use crate::state::service::ClientStates;

/// The webview that owned the panel streams is gone or reloading: finish what they held.
fn close_streams(app: &AppHandle) {
    if let Some(service) = app.try_state::<HostsService>() {
        service.attachments().close_all_streams(&service.outbound());
    }
    if let Some(sink) = app.try_state::<Arc<TauriTerminalSink>>() {
        sink.clear();
    }
}

/// Runs the Combine window until it is closed.
pub fn run() {
    macro_rules! app_handler {
        ($($extra:tt)*) => {
            tauri::generate_handler![
                hosts::hosts_snapshot,
                hosts::retry_host,
                layout::watch_layout,
                layout::unwatch_layout,
                layout::mutate_layout,
                spawn::list_templates,
                spawn::spawn_terminal,
                state::client_state,
                state::set_client_space_state,
                state::set_last_space,
                terminals::terminal_open_stream,
                terminals::terminal_close_stream,
                terminals::terminal_attach,
                terminals::terminal_resume,
                terminals::terminal_detach,
                terminals::terminal_ack,
                terminals::terminal_resize,
                terminals::terminal_focus,
                terminals::terminal_set_display,
                terminals::terminal_request_ownership,
                terminals::terminal_input,
                terminals::terminal_automatic_reply,
                open_url::open_url,
                panels::get_panel,
                panels::report_render
                $($extra)*
            ]
        };
    }
    // Built apart from the builder, so the runtime is named here rather than inferred.
    type Handler = fn(tauri::ipc::Invoke) -> bool;
    let builder = tauri::Builder::default();
    #[cfg(feature = "csp-probe")]
    let builder = builder.plugin(super::csp_probe::plugin());
    #[cfg(not(feature = "csp-probe"))]
    let handler: Handler = app_handler!();
    #[cfg(feature = "csp-probe")]
    let handler: Handler = app_handler!(, super::csp_probe::csp_probe_report);
    builder
        .setup(|app| {
            #[cfg(not(feature = "csp-probe"))]
            let data_dir = app.path().app_data_dir()?;
            #[cfg(feature = "csp-probe")]
            let data_dir = super::csp_probe::data_dir()?;
            let terminal_sink = Arc::new(TauriTerminalSink::default());
            let service = HostsService::start(
                &data_dir,
                ServiceDeps::system(
                    layout::layout_sink(app.handle().clone()),
                    spawn::spawn_sink(app.handle().clone()),
                    terminal_sink.clone(),
                ),
                hosts::sink(app.handle().clone()),
            );
            app.manage(service);
            app.manage(terminal_sink);
            app.manage(ClientStates::new(data_dir));
            Ok(())
        })
        .on_page_load(|webview, payload| {
            if payload.event() == PageLoadEvent::Started {
                close_streams(webview.app_handle());
            }
        })
        .on_window_event(|window, event| {
            if window.label() == "main" && matches!(event, WindowEvent::Destroyed) {
                close_streams(window.app_handle());
            }
        })
        .invoke_handler(handler)
        .build(tauri::generate_context!())
        .expect("Combine failed to start")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                if let Some(service) = app.try_state::<HostsService>() {
                    service.stop();
                }
            }
        });
}
