mod chores;
mod hyprland;
mod kwin;
mod light;
mod log;
mod niri;
mod prefs;
mod render;
mod screen;
mod snow;
mod source;
mod sway;
mod tray;
mod windows;
mod x11;

fn main() {
    snowhush_core::text::choose(&languages());
    let on_x11 = std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value == "x11")
        || (std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_some());
    let result = if on_x11 { x11::run() } else { snow::run() };
    if let Err(error) = result {
        log!("failed: {error}");
        eprintln!("Snowhush couldn't start: {error}");
        std::process::exit(1);
    }
}

/// The user's languages as gettext ranks them: `LANGUAGE`'s list, then `LC_ALL`, `LC_MESSAGES` and `LANG`.
fn languages() -> Vec<String> {
    let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let mut tags: Vec<String> = var("LANGUAGE").map(|list| list.split(':').map(str::to_string).collect()).unwrap_or_default();
    tags.extend(["LC_ALL", "LC_MESSAGES", "LANG"].into_iter().filter_map(var));
    tags
}
