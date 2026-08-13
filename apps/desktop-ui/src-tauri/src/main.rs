#[cfg(not(test))]
fn main() {
    localdesk_desktop::run(tauri::generate_context!());
}

#[cfg(test)]
fn main() {}
