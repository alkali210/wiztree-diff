pub mod commands;
pub mod diff;
pub mod file_categories;
pub mod file_extensions;
pub mod global_treemap;
pub mod import;
pub mod store;
pub mod types;
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(commands::install)
        .invoke_handler(tauri::generate_handler![
            commands::start_comparison,
            commands::cancel_comparison,
            commands::get_job,
            commands::get_comparison,
            commands::list_children,
            commands::get_details,
            commands::list_roots,
            commands::list_extensions,
            commands::get_full_treemap,
            commands::hit_test_treemap,
            commands::get_treemap_bounds,
            commands::release_treemap,
            commands::get_file_categories
        ])
        .run(tauri::generate_context!())
        .expect("无法启动 WizTree Diff");
}
