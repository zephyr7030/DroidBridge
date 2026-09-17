//! How the privileged shell routes (Magisk and Shizuku) represent text and key input with the
//! platform `input` program.

/// Whether `input text` delivers `text` unchanged. It maps each character through the virtual
/// keyboard's key character map, which only covers printable ASCII, and rewrites `%s` to a space.
pub fn input_text_delivers(text: &str) -> bool {
    text.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) && !text.contains("%s")
}

/// The modifier keycodes `input keycombination` holds for an Android `meta_state`, in press order.
/// A state with bits no modifier key represents (caps/num/scroll lock, sym, function) has none.
pub fn meta_modifier_keys(meta_state: i32) -> Option<Vec<i32>> {
    // (generic, left, right, left keycode, right keycode) for shift, alt, ctrl and meta.
    const FAMILIES: [(i32, i32, i32, i32, i32); 4] = [
        (0x1, 0x40, 0x80, 59, 60),
        (0x2, 0x10, 0x20, 57, 58),
        (0x1000, 0x2000, 0x4000, 113, 114),
        (0x10000, 0x20000, 0x40000, 117, 118),
    ];
    let mut remaining = meta_state;
    let mut keys = Vec::new();
    for (generic, left, right, left_key, right_key) in FAMILIES {
        let family = meta_state & (generic | left | right);
        remaining &= !(generic | left | right);
        if family == 0 {
            continue;
        }
        if family & left != 0 || (family & generic != 0 && family & right == 0) {
            keys.push(left_key);
        }
        if family & right != 0 {
            keys.push(right_key);
        }
    }
    (remaining == 0).then_some(keys)
}

#[cfg(test)]
mod tests {
    use super::{input_text_delivers, meta_modifier_keys};

    #[test]
    fn only_printable_ascii_without_the_space_escape_is_typed() {
        assert!(input_text_delivers("abcXYZ123 !~"));
        for text in [
            "中文测试",
            "繁體測試",
            "，。！？「」",
            "ＡＢＣ１２３",
            "🙂🚀𠮷",
            "abc中文123",
            "tab	here",
            "100%s",
        ] {
            assert!(!input_text_delivers(text), "{text}");
        }
    }

    #[test]
    fn meta_states_become_the_modifier_keys_they_name() {
        assert_eq!(meta_modifier_keys(0), Some(vec![]));
        // META_CTRL_ON and META_CTRL_ON | META_CTRL_LEFT_ON both hold the left control key.
        assert_eq!(meta_modifier_keys(0x1000), Some(vec![113]));
        assert_eq!(meta_modifier_keys(0x3000), Some(vec![113]));
        assert_eq!(meta_modifier_keys(0x5000), Some(vec![114]));
        // Ctrl+Shift, in the fixed shift, alt, ctrl, meta order.
        assert_eq!(meta_modifier_keys(0x1001), Some(vec![59, 113]));
        assert_eq!(meta_modifier_keys(0x10002), Some(vec![57, 117]));
        // Caps lock (0x100000) and sym (0x4) have no key to hold.
        assert_eq!(meta_modifier_keys(0x100000), None);
        assert_eq!(meta_modifier_keys(0x1004), None);
    }
}
