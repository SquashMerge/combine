//! App entry: builds and runs the Tauri application.

use std::sync::Arc;

use tauri::webview::PageLoadEvent;
use tauri::{generate_handler, AppHandle, Manager, RunEvent, WindowEvent};

use super::terminals::{self, TauriTerminalSink};
use super::{hosts, layout, open_url, panels, spawn, state};
use crate::hosts::service::{HostsService, ServiceDeps};
use crate::state::service::ClientStates;

/// Calls `$then!` with every command the webview may invoke, as `module::function` paths, then
/// `$extra`. Tauri names a command after its function, so the IPC test reads the names from here.
macro_rules! with_commands {
    ($then:ident! $($extra:tt)*) => {
        $then!(
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
            panels::close_panel,
            panels::pin_panel,
            panels::report_render
            $($extra)*
        )
    };
}

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
    // Built apart from the builder, so the runtime is named here rather than inferred.
    type Handler = fn(tauri::ipc::Invoke) -> bool;
    let builder = tauri::Builder::default();
    #[cfg(feature = "csp-probe")]
    let builder = builder.plugin(super::csp_probe::plugin());
    #[cfg(not(feature = "csp-probe"))]
    let handler: Handler = with_commands!(generate_handler!);
    #[cfg(feature = "csp-probe")]
    let handler: Handler = with_commands!(generate_handler!, super::csp_probe::csp_probe_report);
    builder
        .setup(|app| {
            #[cfg(not(feature = "csp-probe"))]
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("Combine could not find its data directory: {e}"))?;
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::Value;

    macro_rules! command_paths {
        ($($($segment:ident)::+),+) => {
            [$(stringify!($($segment)::+)),+]
        };
    }

    // The paths `run` hands Tauri.
    #[cfg(not(feature = "csp-probe"))]
    const PATHS: &[&str] = &with_commands!(command_paths!);
    #[cfg(feature = "csp-probe")]
    const PATHS: &[&str] = &with_commands!(command_paths!, super::csp_probe::csp_probe_report);

    #[test]
    fn registered_commands_are_the_ipc_contract_commands() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../web/test/fixtures/ipc-contract.json"
        );
        let text = std::fs::read_to_string(path).expect("fixture is readable");
        let fixture: Value = serde_json::from_str(&text).expect("fixture is JSON");
        let named: Vec<&str> = fixture["commands"]
            .as_object()
            .expect("commands")
            .values()
            .map(|name| name.as_str().expect("command name"))
            .collect();
        // Tauri names a command after the last segment of its path.
        let registered: Vec<&str> = PATHS
            .iter()
            .map(|path| path.rsplit("::").next().expect("a segment").trim())
            .collect();
        let named_set: BTreeSet<&str> = named.iter().copied().collect();
        let registered_set: BTreeSet<&str> = registered.iter().copied().collect();
        assert_eq!(
            named_set.len(),
            named.len(),
            "the fixture repeats a command"
        );
        assert_eq!(
            registered_set.len(),
            registered.len(),
            "a command is registered twice"
        );
        let unnamed: Vec<_> = registered_set.difference(&named_set).collect();
        assert!(unnamed.is_empty(), "not in the fixture: {unnamed:?}");
        // Only the probe's command, which the csp-probe build alone registers, may be missing.
        let unregistered: Vec<_> = named_set.difference(&registered_set).collect();
        let probe_only = usize::from(!cfg!(feature = "csp-probe"));
        assert_eq!(
            unregistered.len(),
            probe_only,
            "not registered: {unregistered:?}"
        );
    }
}
