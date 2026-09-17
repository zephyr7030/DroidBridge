use std::time::Duration;
use supervisor::RestartBackoff;

#[test]
fn i7_g01_supervisor_uses_exact_bounded_restart_schedule() {
    let mut backoff = RestartBackoff::default();
    let observed = (0..7)
        .map(|_| backoff.after_run(Duration::ZERO))
        .collect::<Vec<_>>();
    assert_eq!(observed, [1, 2, 4, 8, 16, 30, 30].map(Duration::from_secs));
    assert_eq!(
        backoff.after_run(Duration::from_secs(300)),
        Duration::from_secs(1)
    );
}
