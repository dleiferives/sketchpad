use serde::{Deserialize, Serialize};
use std::{
    fmt,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};
use winit::keyboard::{KeyCode, ModifiersState};

pub const BINDINGS_PER_COMMAND: usize = 2;
const KEYBINDING_FORMAT_VERSION: u32 = 1;

macro_rules! binding_keys {
    ($( $variant:ident => ($key_code:ident, $label:literal) ),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub enum BindingKey {
            $( $variant, )+
        }

        impl BindingKey {
            pub const fn label(self) -> &'static str {
                match self {
                    $( Self::$variant => $label, )+
                }
            }
        }

        impl TryFrom<KeyCode> for BindingKey {
            type Error = ();

            fn try_from(code: KeyCode) -> Result<Self, Self::Error> {
                match code {
                    $( KeyCode::$key_code => Ok(Self::$variant), )+
                    _ => Err(()),
                }
            }
        }
    };
}

binding_keys! {
    KeyA => (KeyA, "A"),
    KeyB => (KeyB, "B"),
    KeyC => (KeyC, "C"),
    KeyD => (KeyD, "D"),
    KeyE => (KeyE, "E"),
    KeyF => (KeyF, "F"),
    KeyG => (KeyG, "G"),
    KeyH => (KeyH, "H"),
    KeyI => (KeyI, "I"),
    KeyJ => (KeyJ, "J"),
    KeyK => (KeyK, "K"),
    KeyL => (KeyL, "L"),
    KeyM => (KeyM, "M"),
    KeyN => (KeyN, "N"),
    KeyO => (KeyO, "O"),
    KeyP => (KeyP, "P"),
    KeyQ => (KeyQ, "Q"),
    KeyR => (KeyR, "R"),
    KeyS => (KeyS, "S"),
    KeyT => (KeyT, "T"),
    KeyU => (KeyU, "U"),
    KeyV => (KeyV, "V"),
    KeyW => (KeyW, "W"),
    KeyX => (KeyX, "X"),
    KeyY => (KeyY, "Y"),
    KeyZ => (KeyZ, "Z"),
    Digit0 => (Digit0, "0"),
    Digit1 => (Digit1, "1"),
    Digit2 => (Digit2, "2"),
    Digit3 => (Digit3, "3"),
    Digit4 => (Digit4, "4"),
    Digit5 => (Digit5, "5"),
    Digit6 => (Digit6, "6"),
    Digit7 => (Digit7, "7"),
    Digit8 => (Digit8, "8"),
    Digit9 => (Digit9, "9"),
    F1 => (F1, "F1"),
    F2 => (F2, "F2"),
    F3 => (F3, "F3"),
    F4 => (F4, "F4"),
    F5 => (F5, "F5"),
    F6 => (F6, "F6"),
    F7 => (F7, "F7"),
    F8 => (F8, "F8"),
    F9 => (F9, "F9"),
    F10 => (F10, "F10"),
    F11 => (F11, "F11"),
    F12 => (F12, "F12"),
    F13 => (F13, "F13"),
    F14 => (F14, "F14"),
    F15 => (F15, "F15"),
    F16 => (F16, "F16"),
    F17 => (F17, "F17"),
    F18 => (F18, "F18"),
    F19 => (F19, "F19"),
    F20 => (F20, "F20"),
    F21 => (F21, "F21"),
    F22 => (F22, "F22"),
    F23 => (F23, "F23"),
    F24 => (F24, "F24"),
    ArrowUp => (ArrowUp, "Up"),
    ArrowDown => (ArrowDown, "Down"),
    ArrowLeft => (ArrowLeft, "Left"),
    ArrowRight => (ArrowRight, "Right"),
    Home => (Home, "Home"),
    End => (End, "End"),
    PageUp => (PageUp, "Page Up"),
    PageDown => (PageDown, "Page Down"),
    Insert => (Insert, "Insert"),
    Delete => (Delete, "Delete"),
    Space => (Space, "Space"),
    Enter => (Enter, "Enter"),
    Tab => (Tab, "Tab"),
    Backquote => (Backquote, "`"),
    Backslash => (Backslash, "\\"),
    BracketLeft => (BracketLeft, "["),
    BracketRight => (BracketRight, "]"),
    Comma => (Comma, ","),
    Equal => (Equal, "="),
    Minus => (Minus, "-"),
    Period => (Period, "."),
    Quote => (Quote, "'"),
    Semicolon => (Semicolon, ";"),
    Slash => (Slash, "/"),
    Numpad0 => (Numpad0, "Numpad 0"),
    Numpad1 => (Numpad1, "Numpad 1"),
    Numpad2 => (Numpad2, "Numpad 2"),
    Numpad3 => (Numpad3, "Numpad 3"),
    Numpad4 => (Numpad4, "Numpad 4"),
    Numpad5 => (Numpad5, "Numpad 5"),
    Numpad6 => (Numpad6, "Numpad 6"),
    Numpad7 => (Numpad7, "Numpad 7"),
    Numpad8 => (Numpad8, "Numpad 8"),
    Numpad9 => (Numpad9, "Numpad 9"),
    NumpadAdd => (NumpadAdd, "Numpad +"),
    NumpadSubtract => (NumpadSubtract, "Numpad -"),
    NumpadMultiply => (NumpadMultiply, "Numpad *"),
    NumpadDivide => (NumpadDivide, "Numpad /"),
    NumpadDecimal => (NumpadDecimal, "Numpad ."),
    NumpadEnter => (NumpadEnter, "Numpad Enter"),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct KeyChord {
    pub key: BindingKey,
    #[serde(default)]
    pub command: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
}

impl KeyChord {
    pub const fn new(key: BindingKey, command: bool, shift: bool, alt: bool) -> Self {
        Self {
            key,
            command,
            shift,
            alt,
        }
    }

    pub fn from_winit(code: KeyCode, modifiers: ModifiersState) -> Option<Self> {
        Some(Self {
            key: BindingKey::try_from(code).ok()?,
            command: modifiers.control_key() || modifiers.super_key(),
            shift: modifiers.shift_key(),
            alt: modifiers.alt_key(),
        })
    }

    pub fn label(self) -> String {
        let mut label = String::new();
        if self.command {
            label.push_str("Ctrl/Cmd+");
        }
        if self.alt {
            label.push_str("Alt+");
        }
        if self.shift {
            label.push_str("Shift+");
        }
        label.push_str(self.key.label());
        label
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum KeyCommand {
    ToggleInterface,
    Undo,
    Redo,
    SaveDocument,
    SaveDocumentAs,
    OpenDocument,
    ImportPng,
    ExportCanvas,
    ExportContent,
    ResetView,
    BrushSmaller,
    BrushLarger,
    BrushOpacityDown,
    BrushOpacityUp,
    ToggleEraser,
    CycleBrushPreset,
    RecentColorOlder,
    RecentColorNewer,
    PresetColor1,
    PresetColor2,
    PresetColor3,
    PresetColor4,
    PresetColor5,
    PresetColor6,
    CreateLayer,
    DuplicateLayer,
    DeleteLayer,
    ToggleLayerVisibility,
    SelectLayerAbove,
    SelectLayerBelow,
    MoveLayerAbove,
    MoveLayerBelow,
    NewDocument,
    SelectBrush,
    SelectEraser,
    PickColor,
    ShowColors,
    ShowLayers,
    ShowHelp,
    PanCanvas,
    AdjustOpacity,
}

pub const ALL_KEY_COMMANDS: [KeyCommand; 41] = [
    KeyCommand::ToggleInterface,
    KeyCommand::Undo,
    KeyCommand::Redo,
    KeyCommand::SaveDocument,
    KeyCommand::SaveDocumentAs,
    KeyCommand::OpenDocument,
    KeyCommand::ImportPng,
    KeyCommand::ExportCanvas,
    KeyCommand::ExportContent,
    KeyCommand::ResetView,
    KeyCommand::BrushSmaller,
    KeyCommand::BrushLarger,
    KeyCommand::BrushOpacityDown,
    KeyCommand::BrushOpacityUp,
    KeyCommand::ToggleEraser,
    KeyCommand::CycleBrushPreset,
    KeyCommand::RecentColorOlder,
    KeyCommand::RecentColorNewer,
    KeyCommand::PresetColor1,
    KeyCommand::PresetColor2,
    KeyCommand::PresetColor3,
    KeyCommand::PresetColor4,
    KeyCommand::PresetColor5,
    KeyCommand::PresetColor6,
    KeyCommand::CreateLayer,
    KeyCommand::DuplicateLayer,
    KeyCommand::DeleteLayer,
    KeyCommand::ToggleLayerVisibility,
    KeyCommand::SelectLayerAbove,
    KeyCommand::SelectLayerBelow,
    KeyCommand::MoveLayerAbove,
    KeyCommand::MoveLayerBelow,
    KeyCommand::NewDocument,
    KeyCommand::SelectBrush,
    KeyCommand::SelectEraser,
    KeyCommand::PickColor,
    KeyCommand::ShowColors,
    KeyCommand::ShowLayers,
    KeyCommand::ShowHelp,
    KeyCommand::PanCanvas,
    KeyCommand::AdjustOpacity,
];

impl KeyCommand {
    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::SelectBrush => "select_brush",
            Self::SelectEraser => "select_eraser",
            Self::PickColor => "pick_color",
            Self::ShowColors => "show_colors",
            Self::ShowLayers => "show_layers",
            Self::ShowHelp => "show_help",
            Self::PanCanvas => "pan_canvas",
            Self::AdjustOpacity => "adjust_opacity",
            Self::ToggleInterface => "toggle_interface",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::SaveDocument => "save_document",
            Self::SaveDocumentAs => "save_document_as",
            Self::NewDocument => "new_document",
            Self::OpenDocument => "open_document",
            Self::ImportPng => "import_png",
            Self::ExportCanvas => "export_canvas",
            Self::ExportContent => "export_content",
            Self::ResetView => "reset_view",
            Self::BrushSmaller => "brush_smaller",
            Self::BrushLarger => "brush_larger",
            Self::BrushOpacityDown => "brush_opacity_down",
            Self::BrushOpacityUp => "brush_opacity_up",
            Self::ToggleEraser => "toggle_eraser",
            Self::CycleBrushPreset => "cycle_brush_preset",
            Self::RecentColorOlder => "recent_color_older",
            Self::RecentColorNewer => "recent_color_newer",
            Self::PresetColor1 => "preset_color_1",
            Self::PresetColor2 => "preset_color_2",
            Self::PresetColor3 => "preset_color_3",
            Self::PresetColor4 => "preset_color_4",
            Self::PresetColor5 => "preset_color_5",
            Self::PresetColor6 => "preset_color_6",
            Self::CreateLayer => "create_layer",
            Self::DuplicateLayer => "duplicate_layer",
            Self::DeleteLayer => "delete_layer",
            Self::ToggleLayerVisibility => "toggle_layer_visibility",
            Self::SelectLayerAbove => "select_layer_above",
            Self::SelectLayerBelow => "select_layer_below",
            Self::MoveLayerAbove => "move_layer_above",
            Self::MoveLayerBelow => "move_layer_below",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        ALL_KEY_COMMANDS
            .into_iter()
            .find(|command| command.id() == id)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::SelectBrush => "Select brush",
            Self::SelectEraser => "Select eraser",
            Self::PickColor => "Pick canvas color",
            Self::ShowColors => "Open colors",
            Self::ShowLayers => "Open layers",
            Self::ShowHelp => "Help and shortcuts",
            Self::PanCanvas => "Hold to pan canvas",
            Self::AdjustOpacity => "Hold and drag for opacity",
            Self::ToggleInterface => "Show / hide interface",
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::SaveDocument => "Save document",
            Self::SaveDocumentAs => "Save document as",
            Self::NewDocument => "New document",
            Self::OpenDocument => "Open document",
            Self::ImportPng => "Import PNG",
            Self::ExportCanvas => "Export canvas",
            Self::ExportContent => "Export content",
            Self::ResetView => "Fit canvas",
            Self::BrushSmaller => "Decrease brush size",
            Self::BrushLarger => "Increase brush size",
            Self::BrushOpacityDown => "Decrease brush opacity",
            Self::BrushOpacityUp => "Increase brush opacity",
            Self::ToggleEraser => "Toggle pen / eraser",
            Self::CycleBrushPreset => "Next brush preset",
            Self::RecentColorOlder => "Older recent color",
            Self::RecentColorNewer => "Newer recent color",
            Self::PresetColor1 => "Select color 1",
            Self::PresetColor2 => "Select color 2",
            Self::PresetColor3 => "Select color 3",
            Self::PresetColor4 => "Select color 4",
            Self::PresetColor5 => "Select color 5",
            Self::PresetColor6 => "Select color 6",
            Self::CreateLayer => "Create layer",
            Self::DuplicateLayer => "Duplicate layer",
            Self::DeleteLayer => "Delete layer",
            Self::ToggleLayerVisibility => "Toggle layer visibility",
            Self::SelectLayerAbove => "Select layer above",
            Self::SelectLayerBelow => "Select layer below",
            Self::MoveLayerAbove => "Move layer above",
            Self::MoveLayerBelow => "Move layer below",
        }
    }

    pub const fn category(self) -> &'static str {
        match self {
            Self::SelectBrush => "BRUSH",
            Self::SelectEraser => "BRUSH",
            Self::PickColor => "COLOR",
            Self::ShowColors => "COLOR",
            Self::ShowLayers => "LAYERS",
            Self::ShowHelp => "GENERAL",
            Self::PanCanvas => "GENERAL",
            Self::AdjustOpacity => "BRUSH",
            Self::ToggleInterface | Self::ResetView => "GENERAL",
            Self::Undo
            | Self::Redo
            | Self::SaveDocument
            | Self::SaveDocumentAs
            | Self::NewDocument
            | Self::OpenDocument
            | Self::ImportPng
            | Self::ExportCanvas
            | Self::ExportContent => "DOCUMENT",
            Self::BrushSmaller
            | Self::BrushLarger
            | Self::BrushOpacityDown
            | Self::BrushOpacityUp
            | Self::ToggleEraser
            | Self::CycleBrushPreset => "BRUSH",
            Self::RecentColorOlder
            | Self::RecentColorNewer
            | Self::PresetColor1
            | Self::PresetColor2
            | Self::PresetColor3
            | Self::PresetColor4
            | Self::PresetColor5
            | Self::PresetColor6 => "COLOR",
            Self::CreateLayer
            | Self::DuplicateLayer
            | Self::DeleteLayer
            | Self::ToggleLayerVisibility
            | Self::SelectLayerAbove
            | Self::SelectLayerBelow
            | Self::MoveLayerAbove
            | Self::MoveLayerBelow => "LAYERS",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CommandBindings {
    pub slots: [Option<KeyChord>; BINDINGS_PER_COMMAND],
}

impl CommandBindings {
    const fn one(chord: KeyChord) -> Self {
        Self {
            slots: [Some(chord), None],
        }
    }

    const fn two(primary: KeyChord, secondary: KeyChord) -> Self {
        Self {
            slots: [Some(primary), Some(secondary)],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyBindings {
    commands: [CommandBindings; ALL_KEY_COMMANDS.len()],
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self {
            commands: std::array::from_fn(|index| default_bindings(ALL_KEY_COMMANDS[index])),
        }
    }
}

impl KeyBindings {
    pub fn for_command(self, command: KeyCommand) -> CommandBindings {
        self.commands[command.index()]
    }

    pub fn command_for(self, chord: KeyChord) -> Option<KeyCommand> {
        ALL_KEY_COMMANDS
            .into_iter()
            .find(|command| self.commands[command.index()].slots.contains(&Some(chord)))
    }

    /// Shift modifies the held pan gesture even when Shift was pressed first.
    /// Explicit user bindings take precedence over this navigation modifier.
    pub fn canvas_command_for(self, chord: KeyChord) -> Option<KeyCommand> {
        self.command_for(chord).or_else(|| {
            let plain = KeyChord {
                shift: false,
                ..chord
            };
            (chord.shift && self.command_for(plain) == Some(KeyCommand::PanCanvas))
                .then_some(KeyCommand::PanCanvas)
        })
    }

    pub fn set(
        &mut self,
        command: KeyCommand,
        slot: usize,
        chord: Option<KeyChord>,
    ) -> Option<(KeyCommand, usize)> {
        if slot >= BINDINGS_PER_COMMAND {
            return None;
        }
        let mut displaced = None;
        if let Some(chord) = chord {
            for other_command in ALL_KEY_COMMANDS {
                for other_slot in 0..BINDINGS_PER_COMMAND {
                    if (other_command != command || other_slot != slot)
                        && self.commands[other_command.index()].slots[other_slot] == Some(chord)
                    {
                        self.commands[other_command.index()].slots[other_slot] = None;
                        displaced = Some((other_command, other_slot));
                    }
                }
            }
        }
        self.commands[command.index()].slots[slot] = chord;
        displaced
    }

    pub fn load(path: &Path) -> Result<Self, KeyBindingError> {
        let bytes = fs::read(path)?;
        Self::decode(&bytes)
    }

    pub fn save(self, path: &Path) -> Result<(), KeyBindingError> {
        let parent = path.parent().ok_or_else(|| {
            KeyBindingError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "keybinding path has no parent",
            ))
        })?;
        fs::create_dir_all(parent)?;
        let encoded = serde_json::to_vec_pretty(&PersistedKeyBindings::from(self))?;
        let temporary = path.with_extension("json.tmp");
        let mut file = File::create(&temporary)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    }

    fn decode(bytes: &[u8]) -> Result<Self, KeyBindingError> {
        let persisted: PersistedKeyBindings = serde_json::from_slice(bytes)?;
        if persisted.version != KEYBINDING_FORMAT_VERSION {
            return Err(KeyBindingError::UnsupportedVersion(persisted.version));
        }
        // Validate explicit entries separately. Saved bindings take priority over
        // defaults added in later versions, without accepting duplicate saved chords.
        let mut explicit = Self {
            commands: [CommandBindings::default(); ALL_KEY_COMMANDS.len()],
        };
        let mut seen = [false; ALL_KEY_COMMANDS.len()];
        for entry in persisted.commands {
            let Some(command) = KeyCommand::from_id(&entry.command) else {
                continue;
            };
            if seen[command.index()] {
                return Err(KeyBindingError::DuplicateCommand(entry.command));
            }
            seen[command.index()] = true;
            explicit.commands[command.index()].slots = entry.bindings;
        }
        explicit.validate_unique()?;
        let mut result = Self::default();
        for command in ALL_KEY_COMMANDS {
            if seen[command.index()] {
                result.commands[command.index()] = CommandBindings::default();
            }
        }
        for command in ALL_KEY_COMMANDS {
            if seen[command.index()] {
                for (slot, chord) in explicit.for_command(command).slots.into_iter().enumerate() {
                    result.set(command, slot, chord);
                }
            }
        }
        result.validate_unique()?;
        Ok(result)
    }

    fn validate_unique(self) -> Result<(), KeyBindingError> {
        for (command_index, command) in ALL_KEY_COMMANDS.into_iter().enumerate() {
            for (slot, chord) in self.commands[command_index].slots.into_iter().enumerate() {
                let Some(chord) = chord else {
                    continue;
                };
                for other_command in ALL_KEY_COMMANDS.into_iter().skip(command_index) {
                    for other_slot in 0..BINDINGS_PER_COMMAND {
                        if other_command == command && other_slot <= slot {
                            continue;
                        }
                        if self.commands[other_command.index()].slots[other_slot] == Some(chord) {
                            return Err(KeyBindingError::Conflict {
                                chord,
                                first: command,
                                second: other_command,
                            });
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn default_bindings(key_command: KeyCommand) -> CommandBindings {
    use BindingKey as Key;
    use KeyCommand as Command;
    let plain = |key| KeyChord::new(key, false, false, false);
    let shift = |key| KeyChord::new(key, false, true, false);
    let primary = |key| KeyChord::new(key, true, false, false);
    let command_shift = |key| KeyChord::new(key, true, true, false);
    let command_alt_shift = |key| KeyChord::new(key, true, true, true);
    match key_command {
        Command::SelectBrush => CommandBindings::one(plain(Key::KeyB)),
        Command::SelectEraser => CommandBindings::one(plain(Key::KeyE)),
        Command::PickColor => CommandBindings::one(plain(Key::KeyI)),
        Command::ShowColors => CommandBindings::one(plain(Key::KeyC)),
        Command::ShowLayers => CommandBindings::one(plain(Key::KeyL)),
        Command::ShowHelp => CommandBindings::one(shift(Key::Slash)),
        Command::PanCanvas => CommandBindings::one(plain(Key::Space)),
        Command::AdjustOpacity => CommandBindings::one(plain(Key::KeyO)),
        Command::ToggleInterface => CommandBindings::two(plain(Key::Tab), plain(Key::F1)),
        Command::Undo => CommandBindings::one(primary(Key::KeyZ)),
        Command::Redo => CommandBindings::two(command_shift(Key::KeyZ), primary(Key::KeyY)),
        Command::SaveDocument => CommandBindings::one(primary(Key::KeyS)),
        Command::SaveDocumentAs => CommandBindings::one(command_shift(Key::KeyS)),
        Command::NewDocument => CommandBindings::one(primary(Key::KeyN)),
        Command::OpenDocument => CommandBindings::one(primary(Key::KeyO)),
        Command::ImportPng => CommandBindings::one(primary(Key::KeyI)),
        Command::ExportCanvas => CommandBindings::one(command_shift(Key::KeyE)),
        Command::ExportContent => CommandBindings::one(command_alt_shift(Key::KeyE)),
        Command::ResetView => CommandBindings::one(plain(Key::Home)),
        Command::BrushSmaller => CommandBindings::one(plain(Key::BracketLeft)),
        Command::BrushLarger => CommandBindings::one(plain(Key::BracketRight)),
        Command::BrushOpacityDown => CommandBindings::one(shift(Key::BracketLeft)),
        Command::BrushOpacityUp => CommandBindings::one(shift(Key::BracketRight)),
        Command::ToggleEraser => CommandBindings::default(),
        Command::CycleBrushPreset => CommandBindings::one(shift(Key::KeyB)),
        Command::RecentColorOlder => CommandBindings::one(plain(Key::KeyX)),
        Command::RecentColorNewer => CommandBindings::one(shift(Key::KeyX)),
        Command::PresetColor1 => CommandBindings::one(plain(Key::Digit1)),
        Command::PresetColor2 => CommandBindings::one(plain(Key::Digit2)),
        Command::PresetColor3 => CommandBindings::one(plain(Key::Digit3)),
        Command::PresetColor4 => CommandBindings::one(plain(Key::Digit4)),
        Command::PresetColor5 => CommandBindings::one(plain(Key::Digit5)),
        Command::PresetColor6 => CommandBindings::one(plain(Key::Digit6)),
        Command::CreateLayer => CommandBindings::one(command_shift(Key::KeyN)),
        Command::DuplicateLayer => CommandBindings::one(command_shift(Key::KeyD)),
        Command::DeleteLayer => CommandBindings::one(command_shift(Key::Delete)),
        Command::ToggleLayerVisibility => CommandBindings::one(command_shift(Key::KeyH)),
        Command::SelectLayerAbove => CommandBindings::one(plain(Key::PageUp)),
        Command::SelectLayerBelow => CommandBindings::one(plain(Key::PageDown)),
        Command::MoveLayerAbove => CommandBindings::one(primary(Key::PageUp)),
        Command::MoveLayerBelow => CommandBindings::one(primary(Key::PageDown)),
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct PersistedKeyBindings {
    version: u32,
    commands: Vec<PersistedCommand>,
}

impl From<KeyBindings> for PersistedKeyBindings {
    fn from(bindings: KeyBindings) -> Self {
        Self {
            version: KEYBINDING_FORMAT_VERSION,
            commands: ALL_KEY_COMMANDS
                .into_iter()
                .map(|command| PersistedCommand {
                    command: command.id().to_owned(),
                    bindings: bindings.for_command(command).slots,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct PersistedCommand {
    command: String,
    bindings: [Option<KeyChord>; BINDINGS_PER_COMMAND],
}

#[derive(Debug)]
pub enum KeyBindingError {
    Io(io::Error),
    Json(serde_json::Error),
    UnsupportedVersion(u32),
    DuplicateCommand(String),
    Conflict {
        chord: KeyChord,
        first: KeyCommand,
        second: KeyCommand,
    },
}

impl fmt::Display for KeyBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported keybinding format version {version}")
            }
            Self::DuplicateCommand(command) => {
                write!(formatter, "duplicate keybinding command {command:?}")
            }
            Self::Conflict {
                chord,
                first,
                second,
            } => write!(
                formatter,
                "shortcut {} is assigned to both {} and {}",
                chord.label(),
                first.label(),
                second.label()
            ),
        }
    }
}

impl std::error::Error for KeyBindingError {}

impl From<io::Error> for KeyBindingError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for KeyBindingError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub fn default_keybindings_path() -> PathBuf {
    if let Some(path) = nonempty_env("SKETCHPAD_KEYBINDINGS_PATH") {
        return PathBuf::from(path);
    }
    if let Some(config_home) = nonempty_env("XDG_CONFIG_HOME") {
        return PathBuf::from(config_home)
            .join("sketchpad")
            .join("keybindings.json");
    }
    if let Some(user_home) = nonempty_env("HOME") {
        return PathBuf::from(user_home)
            .join(".config")
            .join("sketchpad")
            .join("keybindings.json");
    }
    std::env::temp_dir().join("sketchpad-keybindings.json")
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    #[test]
    fn shifted_pan_works_with_saved_bindings_and_respects_explicit_shortcuts() {
        use super::*;
        let mut bindings = KeyBindings::default();
        let shifted = KeyChord::new(BindingKey::Space, false, true, false);
        assert_eq!(
            bindings.canvas_command_for(shifted),
            Some(KeyCommand::PanCanvas)
        );
        assert_eq!(
            bindings.canvas_command_for(KeyChord {
                shift: false,
                ..shifted
            }),
            Some(KeyCommand::PanCanvas)
        );
        bindings.set(KeyCommand::Undo, 0, Some(shifted));
        assert_eq!(bindings.canvas_command_for(shifted), Some(KeyCommand::Undo));
    }

    use super::*;

    #[test]
    fn saved_legacy_keys_take_priority_over_new_defaults() {
        // Build serialized chords through the public representation to keep the
        // compatibility test independent of serde's enum spelling.
        let mut saved = PersistedKeyBindings {
            version: 1,
            commands: Vec::new(),
        };
        saved.commands.push(PersistedCommand {
            command: "cycle_brush_preset".to_owned(),
            bindings: [
                Some(KeyChord::new(BindingKey::KeyB, false, false, false)),
                None,
            ],
        });
        let bindings = KeyBindings::decode(&serde_json::to_vec(&saved).unwrap()).unwrap();
        assert_eq!(
            bindings.command_for(KeyChord::new(BindingKey::KeyB, false, false, false)),
            Some(KeyCommand::CycleBrushPreset)
        );
        assert_eq!(
            bindings.for_command(KeyCommand::SelectBrush).slots,
            [None, None]
        );
        assert_eq!(
            bindings.command_for(KeyChord::new(BindingKey::Space, false, false, false)),
            Some(KeyCommand::PanCanvas)
        );
    }

    #[test]
    fn drawing_defaults_have_direct_tools_and_hold_commands() {
        let bindings = KeyBindings::default();
        for (key, command) in [
            (BindingKey::KeyB, KeyCommand::SelectBrush),
            (BindingKey::KeyE, KeyCommand::SelectEraser),
            (BindingKey::Space, KeyCommand::PanCanvas),
            (BindingKey::KeyO, KeyCommand::AdjustOpacity),
            (BindingKey::Tab, KeyCommand::ToggleInterface),
        ] {
            assert_eq!(
                bindings.command_for(KeyChord::new(key, false, false, false)),
                Some(command)
            );
        }
    }

    #[test]
    fn defaults_are_unique_and_preserve_the_existing_shortcuts() {
        let bindings = KeyBindings::default();
        bindings.validate_unique().unwrap();
        assert_eq!(
            bindings.command_for(KeyChord::new(BindingKey::KeyZ, true, false, false)),
            Some(KeyCommand::Undo)
        );
        assert_eq!(
            bindings.command_for(KeyChord::new(BindingKey::KeyZ, true, true, false)),
            Some(KeyCommand::Redo)
        );
        assert_eq!(
            bindings.command_for(KeyChord::new(BindingKey::KeyY, true, false, false)),
            Some(KeyCommand::Redo)
        );
    }

    #[test]
    fn assigning_a_conflict_displaces_the_previous_owner() {
        let mut bindings = KeyBindings::default();
        let chord = KeyChord::new(BindingKey::KeyZ, true, false, false);
        let displaced = bindings.set(KeyCommand::SaveDocument, 1, Some(chord));

        assert_eq!(displaced, Some((KeyCommand::Undo, 0)));
        assert_eq!(bindings.command_for(chord), Some(KeyCommand::SaveDocument));
        assert_eq!(bindings.for_command(KeyCommand::Undo).slots, [None, None]);
    }

    #[test]
    fn persisted_bindings_round_trip_exactly() {
        let mut bindings = KeyBindings::default();
        bindings.set(
            KeyCommand::CycleBrushPreset,
            0,
            Some(KeyChord::new(BindingKey::F12, false, true, true)),
        );
        bindings.set(KeyCommand::PresetColor6, 0, None);

        let encoded =
            serde_json::to_vec(&PersistedKeyBindings::from(bindings)).expect("encode bindings");
        let decoded = KeyBindings::decode(&encoded).expect("decode bindings");

        assert_eq!(decoded, bindings);
    }

    #[test]
    fn atomic_save_replaces_and_reloads_preferences() {
        let unique = format!(
            "sketchpad-keybindings-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        );
        let directory = std::env::temp_dir().join(unique);
        let path = directory.join("keybindings.json");
        let mut bindings = KeyBindings::default();
        bindings.save(&path).unwrap();
        bindings.set(
            KeyCommand::ToggleEraser,
            0,
            Some(KeyChord::new(BindingKey::KeyB, false, false, false)),
        );
        bindings.save(&path).unwrap();

        assert_eq!(KeyBindings::load(&path).unwrap(), bindings);

        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn invalid_persisted_conflicts_are_rejected() {
        let mut persisted = PersistedKeyBindings::from(KeyBindings::default());
        let undo = persisted
            .commands
            .iter()
            .find(|entry| entry.command == KeyCommand::Undo.id())
            .unwrap()
            .bindings[0];
        persisted
            .commands
            .iter_mut()
            .find(|entry| entry.command == KeyCommand::SaveDocument.id())
            .unwrap()
            .bindings[0] = undo;
        let encoded = serde_json::to_vec(&persisted).unwrap();

        assert!(matches!(
            KeyBindings::decode(&encoded),
            Err(KeyBindingError::Conflict { .. })
        ));
    }
}
