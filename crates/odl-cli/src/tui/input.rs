//! Single-line text input.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Default)]
pub struct Input {
    pub value: String,
    /// Cursor position in chars.
    pub cursor: usize,
}

pub enum InputResult {
    Edited,
    Submit,
    Cancel,
    /// A key the input does not use (e.g. a host shortcut).
    Ignored,
}

impl Input {
    pub fn new(value: &str) -> Self {
        Self { value: value.to_string(), cursor: value.chars().count() }
    }

    fn byte_idx(&self, char_idx: usize) -> usize {
        self.value.char_indices().nth(char_idx).map(|(i, _)| i).unwrap_or(self.value.len())
    }

    pub fn handle(&mut self, k: &KeyEvent) -> InputResult {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let len = self.value.chars().count();
        match k.code {
            KeyCode::Enter => return InputResult::Submit,
            KeyCode::Esc => return InputResult::Cancel,
            KeyCode::Char('c') if ctrl => return InputResult::Cancel,
            KeyCode::Char('u') if ctrl => {
                self.value.clear();
                self.cursor = 0;
            }
            KeyCode::Char('w') if ctrl => {
                let chars: Vec<char> = self.value.chars().collect();
                let mut i = self.cursor;
                while i > 0 && chars[i - 1] == ' ' {
                    i -= 1;
                }
                while i > 0 && chars[i - 1] != ' ' {
                    i -= 1;
                }
                let (a, b) = (self.byte_idx(i), self.byte_idx(self.cursor));
                self.value.replace_range(a..b, "");
                self.cursor = i;
            }
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = len,
            KeyCode::Char(_) if ctrl || alt => return InputResult::Ignored,
            KeyCode::Char(c) => {
                let i = self.byte_idx(self.cursor);
                self.value.insert(i, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let i = self.byte_idx(self.cursor - 1);
                self.value.remove(i);
                self.cursor -= 1;
            }
            KeyCode::Delete if self.cursor < len => {
                let i = self.byte_idx(self.cursor);
                self.value.remove(i);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            KeyCode::Backspace | KeyCode::Delete => {}
            _ => return InputResult::Ignored,
        }
        InputResult::Edited
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn edits() {
        let mut i = Input::new("ab");
        i.handle(&key(KeyCode::Left));
        i.handle(&key(KeyCode::Char('é')));
        assert_eq!(i.value, "aéb");
        i.handle(&key(KeyCode::Backspace));
        assert_eq!(i.value, "ab");
        i.handle(&key(KeyCode::End));
        i.handle(&key(KeyCode::Char(' ')));
        i.handle(&key(KeyCode::Char('x')));
        i.handle(&KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(i.value, "ab ");
        assert!(matches!(i.handle(&key(KeyCode::Enter)), InputResult::Submit));
    }

    // Review finding 6: Ctrl/Alt letters are not typed into the input.
    #[test]
    fn modifier_keys_are_not_text() {
        let mut i = Input::new("ab");
        for m in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            assert!(matches!(i.handle(&KeyEvent::new(KeyCode::Char('s'), m)), InputResult::Ignored));
        }
        assert_eq!(i.value, "ab");
        assert!(matches!(i.handle(&KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT)), InputResult::Edited));
        assert_eq!(i.value, "abS");
        assert!(matches!(i.handle(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), InputResult::Cancel));
    }
}
