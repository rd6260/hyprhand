//! Virtual HID device via evdev/uinput.
//!
//! Creates a virtual keyboard + mouse at startup. Requires `/dev/uinput` access;
//! add a udev rule like:
//!   KERNEL=="uinput", GROUP="input", MODE="0660"
//! and add your user to the `input` group.

use std::sync::{Mutex, OnceLock};

use evdev::{
    uinput::{VirtualDevice, VirtualDeviceBuilder},
    AttributeSet, EventType, InputEvent, Key, RelativeAxisType,
};
use protocol::MouseButton;

// ── Global device ─────────────────────────────────────────────────────────────

static DEVICE: OnceLock<Mutex<Option<VirtualDevice>>> = OnceLock::new();

fn device() -> &'static Mutex<Option<VirtualDevice>> {
    DEVICE.get_or_init(|| Mutex::new(create_device()))
}

fn create_device() -> Option<VirtualDevice> {
    let mut keys = AttributeSet::<Key>::new();
    // All standard keys (evdev Key is a newtype wrapping u16, constructed via Key::new)
    for k in 0u16..256 {
        keys.insert(Key::new(k));
    }
    // Mouse buttons
    keys.insert(Key::BTN_LEFT);
    keys.insert(Key::BTN_RIGHT);
    keys.insert(Key::BTN_MIDDLE);

    let mut axes = AttributeSet::<RelativeAxisType>::new();
    axes.insert(RelativeAxisType::REL_X);
    axes.insert(RelativeAxisType::REL_Y);
    axes.insert(RelativeAxisType::REL_WHEEL);
    axes.insert(RelativeAxisType::REL_HWHEEL);

    let result = VirtualDeviceBuilder::new()
        .and_then(|b| b.name("hyprhand-input").with_keys(&keys))
        .and_then(|b| b.with_relative_axes(&axes))
        .and_then(|b| b.build());

    match result {
        Ok(dev) => {
            tracing::info!("Created virtual HID device (hyprhand-input)");
            Some(dev)
        }
        Err(e) => {
            tracing::warn!("Cannot create virtual HID device: {e} — /dev/uinput access required");
            None
        }
    }
}

pub fn available() -> bool {
    device().lock().unwrap().is_some()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn emit(events: &[InputEvent]) -> anyhow::Result<()> {
    let mut guard = device().lock().unwrap();
    let dev = guard.as_mut().ok_or_else(|| anyhow::anyhow!("uinput unavailable"))?;
    dev.emit(events)?;
    Ok(())
}

fn syn() -> InputEvent {
    InputEvent::new(EventType::SYNCHRONIZATION, 0, 0)
}

fn key_ev(key: Key, down: bool) -> InputEvent {
    InputEvent::new(EventType::KEY, key.code(), if down { 1 } else { 0 })
}

fn rel_ev(axis: RelativeAxisType, value: i32) -> InputEvent {
    InputEvent::new(EventType::RELATIVE, axis.0, value)
}

// ── Mouse ─────────────────────────────────────────────────────────────────────

pub fn mouse_move(dx: i32, dy: i32) -> anyhow::Result<()> {
    emit(&[
        rel_ev(RelativeAxisType::REL_X, dx),
        rel_ev(RelativeAxisType::REL_Y, dy),
        syn(),
    ])
}

pub fn mouse_scroll(dy: i32) -> anyhow::Result<()> {
    emit(&[rel_ev(RelativeAxisType::REL_WHEEL, dy), syn()])
}

pub fn mouse_click(button: MouseButton) -> anyhow::Result<()> {
    let key = mouse_button_key(button);
    emit(&[key_ev(key, true), syn(), key_ev(key, false), syn()])
}

pub fn mouse_button_raw(button: MouseButton, down: bool) -> anyhow::Result<()> {
    let key = mouse_button_key(button);
    emit(&[key_ev(key, down), syn()])
}

fn mouse_button_key(button: MouseButton) -> Key {
    match button {
        MouseButton::Left => Key::BTN_LEFT,
        MouseButton::Right => Key::BTN_RIGHT,
        MouseButton::Middle => Key::BTN_MIDDLE,
    }
}

// ── Keyboard ──────────────────────────────────────────────────────────────────

pub fn key_event(key_name: &str, down: bool) -> anyhow::Result<()> {
    let key = name_to_key(key_name).ok_or_else(|| anyhow::anyhow!("unknown key: {key_name}"))?;
    emit(&[key_ev(key, down), syn()])
}

pub fn key_combo(keys: &[String]) -> anyhow::Result<()> {
    let mut resolved: Vec<Key> = vec![];
    for k in keys {
        resolved.push(name_to_key(k).ok_or_else(|| anyhow::anyhow!("unknown key: {k}"))?);
    }
    let mut events: Vec<InputEvent> = resolved.iter().map(|&k| key_ev(k, true)).collect();
    events.push(syn());
    for &k in resolved.iter().rev() {
        events.push(key_ev(k, false));
    }
    events.push(syn());
    emit(&events)
}

pub fn type_text(text: &str) -> anyhow::Result<()> {
    for ch in text.chars() {
        if let Some((key, shift)) = char_to_key(ch) {
            let mut events = vec![];
            if shift {
                events.push(key_ev(Key::KEY_LEFTSHIFT, true));
            }
            events.push(key_ev(key, true));
            events.push(syn());
            events.push(key_ev(key, false));
            if shift {
                events.push(key_ev(Key::KEY_LEFTSHIFT, false));
            }
            events.push(syn());
            emit(&events)?;
        }
    }
    Ok(())
}

// ── Key name map ──────────────────────────────────────────────────────────────

fn name_to_key(name: &str) -> Option<Key> {
    match name.to_lowercase().as_str() {
        "ctrl" | "control" => Some(Key::KEY_LEFTCTRL),
        "alt" => Some(Key::KEY_LEFTALT),
        "shift" => Some(Key::KEY_LEFTSHIFT),
        "super" | "meta" | "win" => Some(Key::KEY_LEFTMETA),
        "enter" | "return" => Some(Key::KEY_ENTER),
        "esc" | "escape" => Some(Key::KEY_ESC),
        "tab" => Some(Key::KEY_TAB),
        "backspace" => Some(Key::KEY_BACKSPACE),
        "delete" | "del" => Some(Key::KEY_DELETE),
        "up" => Some(Key::KEY_UP),
        "down" => Some(Key::KEY_DOWN),
        "left" => Some(Key::KEY_LEFT),
        "right" => Some(Key::KEY_RIGHT),
        "home" => Some(Key::KEY_HOME),
        "end" => Some(Key::KEY_END),
        "pageup" => Some(Key::KEY_PAGEUP),
        "pagedown" => Some(Key::KEY_PAGEDOWN),
        "space" => Some(Key::KEY_SPACE),
        "f1" => Some(Key::KEY_F1),
        "f2" => Some(Key::KEY_F2),
        "f3" => Some(Key::KEY_F3),
        "f4" => Some(Key::KEY_F4),
        "f5" => Some(Key::KEY_F5),
        "f6" => Some(Key::KEY_F6),
        "f7" => Some(Key::KEY_F7),
        "f8" => Some(Key::KEY_F8),
        "f9" => Some(Key::KEY_F9),
        "f10" => Some(Key::KEY_F10),
        "f11" => Some(Key::KEY_F11),
        "f12" => Some(Key::KEY_F12),
        "a" => Some(Key::KEY_A), "b" => Some(Key::KEY_B),
        "c" => Some(Key::KEY_C), "d" => Some(Key::KEY_D),
        "e" => Some(Key::KEY_E), "f" => Some(Key::KEY_F),
        "g" => Some(Key::KEY_G), "h" => Some(Key::KEY_H),
        "i" => Some(Key::KEY_I), "j" => Some(Key::KEY_J),
        "k" => Some(Key::KEY_K), "l" => Some(Key::KEY_L),
        "m" => Some(Key::KEY_M), "n" => Some(Key::KEY_N),
        "o" => Some(Key::KEY_O), "p" => Some(Key::KEY_P),
        "q" => Some(Key::KEY_Q), "r" => Some(Key::KEY_R),
        "s" => Some(Key::KEY_S), "t" => Some(Key::KEY_T),
        "u" => Some(Key::KEY_U), "v" => Some(Key::KEY_V),
        "w" => Some(Key::KEY_W), "x" => Some(Key::KEY_X),
        "y" => Some(Key::KEY_Y), "z" => Some(Key::KEY_Z),
        "0" => Some(Key::KEY_0), "1" => Some(Key::KEY_1),
        "2" => Some(Key::KEY_2), "3" => Some(Key::KEY_3),
        "4" => Some(Key::KEY_4), "5" => Some(Key::KEY_5),
        "6" => Some(Key::KEY_6), "7" => Some(Key::KEY_7),
        "8" => Some(Key::KEY_8), "9" => Some(Key::KEY_9),
        "comma" => Some(Key::KEY_COMMA),
        "period" => Some(Key::KEY_DOT),
        "slash" => Some(Key::KEY_SLASH),
        "semicolon" => Some(Key::KEY_SEMICOLON),
        "apostrophe" | "quote" => Some(Key::KEY_APOSTROPHE),
        "minus" => Some(Key::KEY_MINUS),
        "equal" => Some(Key::KEY_EQUAL),
        "leftbrace" => Some(Key::KEY_LEFTBRACE),
        "rightbrace" => Some(Key::KEY_RIGHTBRACE),
        "backslash" => Some(Key::KEY_BACKSLASH),
        "grave" | "backtick" => Some(Key::KEY_GRAVE),
        _ => None,
    }
}

/// US layout char → (key, needs_shift).
fn char_to_key(ch: char) -> Option<(Key, bool)> {
    match ch {
        'a'..='z' => name_to_key(&ch.to_string()).map(|k| (k, false)),
        'A'..='Z' => name_to_key(&ch.to_lowercase().to_string()).map(|k| (k, true)),
        '0'..='9' => name_to_key(&ch.to_string()).map(|k| (k, false)),
        ' ' => Some((Key::KEY_SPACE, false)),
        '\n' | '\r' => Some((Key::KEY_ENTER, false)),
        '\t' => Some((Key::KEY_TAB, false)),
        '!' => Some((Key::KEY_1, true)),
        '@' => Some((Key::KEY_2, true)),
        '#' => Some((Key::KEY_3, true)),
        '$' => Some((Key::KEY_4, true)),
        '%' => Some((Key::KEY_5, true)),
        '^' => Some((Key::KEY_6, true)),
        '&' => Some((Key::KEY_7, true)),
        '*' => Some((Key::KEY_8, true)),
        '(' => Some((Key::KEY_9, true)),
        ')' => Some((Key::KEY_0, true)),
        '-' => Some((Key::KEY_MINUS, false)),
        '_' => Some((Key::KEY_MINUS, true)),
        '=' => Some((Key::KEY_EQUAL, false)),
        '+' => Some((Key::KEY_EQUAL, true)),
        '[' => Some((Key::KEY_LEFTBRACE, false)),
        '{' => Some((Key::KEY_LEFTBRACE, true)),
        ']' => Some((Key::KEY_RIGHTBRACE, false)),
        '}' => Some((Key::KEY_RIGHTBRACE, true)),
        '\\' => Some((Key::KEY_BACKSLASH, false)),
        '|' => Some((Key::KEY_BACKSLASH, true)),
        ';' => Some((Key::KEY_SEMICOLON, false)),
        ':' => Some((Key::KEY_SEMICOLON, true)),
        '\'' => Some((Key::KEY_APOSTROPHE, false)),
        '"' => Some((Key::KEY_APOSTROPHE, true)),
        ',' => Some((Key::KEY_COMMA, false)),
        '<' => Some((Key::KEY_COMMA, true)),
        '.' => Some((Key::KEY_DOT, false)),
        '>' => Some((Key::KEY_DOT, true)),
        '/' => Some((Key::KEY_SLASH, false)),
        '?' => Some((Key::KEY_SLASH, true)),
        '`' => Some((Key::KEY_GRAVE, false)),
        '~' => Some((Key::KEY_GRAVE, true)),
        _ => None,
    }
}
