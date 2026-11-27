// Never open a console window on Windows, in debug or release builds.
#![windows_subsystem = "windows"]

use tauri_plugin_window_state::StateFlags;

mod commands;
mod secrets;
mod twitch;
mod window;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Remember the window's size, position and maximized state between launches.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(StateFlags::all() - StateFlags::VISIBLE - StateFlags::DECORATIONS)
                .build(),
        )
        .manage(commands::AppState::default())
        .invoke_handler(tauri::generate_handler![
            commands::restore_session,
            commands::sign_in,
            commands::cancel_sign_in,
            commands::sign_out,
            commands::chat_token,
            commands::helix,
            commands::fetch_json,
            commands::open_external,
            commands::check_for_update,
            commands::apply_update,
            window::start_window_drag,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Sylva Chat");
}
