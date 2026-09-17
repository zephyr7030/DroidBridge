#[cfg(unix)]
fn main() -> std::process::ExitCode {
    daemon::process::main()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(1)
}
