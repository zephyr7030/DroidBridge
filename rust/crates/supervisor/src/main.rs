#[cfg(unix)]
fn main() -> std::process::ExitCode {
    supervisor::process::main()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(1)
}
