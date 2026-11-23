//! App entry: builds and runs the Tauri application.

use tauri::{Manager, RunEvent};

use super::{hosts, layout, state};
use crate::hosts::service::{HostsService, ServiceDeps};
use crate::state::service::ClientStates;

/// Runs the Combine window until it is closed.
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let service = HostsService::start(
                &data_dir,
                ServiceDeps::system(layout::layout_sink(app.handle().clone())),
                hosts::sink(app.handle().clone()),
            );
            app.manage(service);
            app.manage(ClientStates::new(data_dir));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            hosts::hosts_snapshot,
            hosts::retry_host,
            layout::watch_layout,
            layout::unwatch_layout,
            layout::mutate_layout,
            state::client_state,
            state::set_client_space_state,
            state::set_last_space
        ])
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
