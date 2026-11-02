//! The Community binary: the app with no extensions (`lib.rs`, §13).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    alamo_app_lib::run(
        tauri::generate_context!(),
        alamo_app_lib::Extensions::default(),
    );
}
