use std::process::Command;

#[test]
fn missing_database_is_a_startup_failure() {
    let output = Command::new(env!("CARGO_BIN_EXE_sequoia-server"))
        .env_remove("DATABASE_URL")
        .output()
        .expect("run server binary");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("DATABASE_URL is required"));
}
