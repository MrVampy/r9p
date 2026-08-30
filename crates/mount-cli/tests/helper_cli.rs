use std::process::Command;

#[test]
fn helper_preserves_the_r9p_mount_help_surface() {
    let output = Command::new(env!("CARGO_BIN_EXE_r9p-mount"))
        .args(["mount", "--help"])
        .output()
        .expect("mount helper help");
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 mount help");

    assert!(output.status.success());
    assert!(stderr.contains("usage: r9p mount"));
    assert!(stderr.contains("--coherent-read-cache"));
    assert!(stderr.contains("--read-cache-max-bytes"));
    assert!(stderr.contains("mount ensure|status|stop"));
}
