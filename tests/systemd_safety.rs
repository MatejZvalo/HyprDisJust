use std::path::Path;

use hyprdisjust::systemd::render_user_service;

#[test]
fn systemd_execstart_quotes_paths_with_spaces() {
    let service = render_user_service(Path::new("/home/user/My Apps/hyprdisjust"), true).unwrap();

    assert!(service.contains("ExecStart=\"/home/user/My Apps/hyprdisjust\" daemon --unattended"));
}

#[test]
fn systemd_execstart_rejects_expansion_and_control_characters() {
    for path in [
        "/tmp/hypr%disjust",
        "/tmp/hypr$disjust",
        "/tmp/hypr\\disjust",
        "/tmp/hypr\"disjust",
        "/tmp/hypr\ndisjust",
    ] {
        assert!(
            render_user_service(Path::new(path), false).is_err(),
            "{path}"
        );
    }
}
