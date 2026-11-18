#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if matches!(
        args.first().and_then(|value| value.to_str()),
        Some("__soyo-file-worker" | "__soyo-sandbox-launch")
    ) {
        let code = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("internal worker runtime")
            .block_on(soyo_core::execution::internal_command(args))
            .unwrap_or(1);
        std::process::exit(code);
    }

    if let Err(err) = soyo_core::execution::pin_launcher_image() {
        eprintln!("soyo-desktop warning: failed to pin launcher image: {err}");
    }
    soyo_desktop::run();
}
