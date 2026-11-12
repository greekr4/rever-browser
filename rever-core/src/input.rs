//! Key naming shared by every input source.

/// GPUI names non-printable keys in lowercase ("enter", "left"); Servo wants the
/// W3C `KeyboardEvent.key` value ("Enter", "ArrowLeft"). `None` means the key is
/// not a named key — a printable key is passed through as its character instead.
pub fn w3c_key_name(gpui_key: &str) -> Option<&'static str> {
    Some(match gpui_key {
        "enter" => "Enter",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "tab" => "Tab",
        "escape" => "Escape",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 이름_있는_키는_w3c_이름으로_바꾸고_글자_키는_넘기지_않는다() {
        assert_eq!(w3c_key_name("enter"), Some("Enter"));
        assert_eq!(w3c_key_name("left"), Some("ArrowLeft"));
        assert_eq!(w3c_key_name("a"), None);
        assert_eq!(w3c_key_name("space"), None);
    }
}
