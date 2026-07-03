use std::path::PathBuf;

use hyprdisjust::hyprland::ipc::{parse_monitor_event, socket2_path, MonitorSocketEvent};

#[test]
fn parses_socket2_monitor_events() {
    assert_eq!(
        parse_monitor_event("monitoradded>>DP-1\n"),
        Some(MonitorSocketEvent::Added)
    );
    assert_eq!(
        parse_monitor_event("monitorremoved>>DP-1\n"),
        Some(MonitorSocketEvent::Removed)
    );
    assert_eq!(
        parse_monitor_event("monitoraddedv2>>1,DP-1,Acme Panel\n"),
        Some(MonitorSocketEvent::AddedV2)
    );
    assert_eq!(
        parse_monitor_event("monitorremovedv2>>1,DP-1,Acme Panel\r\n"),
        Some(MonitorSocketEvent::RemovedV2)
    );
}

#[test]
fn ignores_unrelated_socket2_events() {
    assert_eq!(parse_monitor_event("workspace>>2\n"), None);
    assert_eq!(parse_monitor_event("monitoradded"), None);
    assert_eq!(parse_monitor_event(""), None);
}

#[test]
fn builds_socket2_path() {
    let path = socket2_path(PathBuf::from("/run/user/1000"), "signature");

    assert_eq!(
        path,
        PathBuf::from("/run/user/1000/hypr/signature/.socket2.sock")
    );
}
