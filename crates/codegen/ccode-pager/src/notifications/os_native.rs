
use std::process::Command;

pub fn send_notification(title: &str, body: &str) {
    let _ = send_notification_inner(title, body);
}

#[cfg(target_os = "macos")]
fn send_notification_inner(title: &str, body: &str) -> std::io::Result<()> {
    let script = format!(
        "display notification {} with title {}",
        shell_escape(body),
        shell_escape(title),
    );
    Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn send_notification_inner(title: &str, body: &str) -> std::io::Result<()> {
    Command::new("notify-send")
        .arg(title)
        .arg(body)
        .output()?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn send_notification_inner(_title: &str, _body: &str) -> std::io::Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn shell_escape(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}
