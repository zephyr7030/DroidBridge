//! I8-ANDROID Magisk helper family independence.

use contract::CapabilityState;
use daemon::{HelperFamily, helper_family_facts};

fn projection(
    facts: [daemon::HelperFamilyFact; 3],
) -> [(&'static str, CapabilityState, Option<&'static str>); 3] {
    facts.map(|fact| (fact.family.key(), fact.state, fact.reason))
}

#[test]
fn i8_android_g09_each_helper_family_remains_available_when_a_sibling_probe_fails() {
    let available = CapabilityState::Available;
    let unavailable = CapabilityState::Unavailable;
    assert_eq!(
        projection(helper_family_facts(
            true,
            |family| family != HelperFamily::Clipboard,
            |_| false,
        )),
        [
            ("magisk.launch", available, None),
            (
                "magisk.clipboard",
                unavailable,
                Some("CLIPBOARD_PROBE_FAILED")
            ),
            ("magisk.notifications", available, None),
        ]
    );
    assert_eq!(
        projection(helper_family_facts(
            true,
            |_| true,
            |family| family == HelperFamily::Launch,
        )),
        [
            ("magisk.launch", unavailable, Some("OPERATION_DENIED")),
            ("magisk.clipboard", available, None),
            ("magisk.notifications", available, None),
        ]
    );
    assert_eq!(
        projection(helper_family_facts(false, |_| true, |_| false)),
        [
            ("magisk.launch", unavailable, Some("HELPER_UNAVAILABLE")),
            ("magisk.clipboard", unavailable, Some("HELPER_UNAVAILABLE")),
            (
                "magisk.notifications",
                unavailable,
                Some("HELPER_UNAVAILABLE")
            ),
        ]
    );
}
