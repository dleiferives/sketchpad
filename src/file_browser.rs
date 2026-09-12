//! An in-app file browser that works with a pen and does not depend on a desktop portal.
use egui::{Align2, Color32, Vec2};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsavedChoice {
    Save,
    Discard,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePurpose {
    Open,
    Save,
    Import,
    Export { cropped: bool },
}
impl FilePurpose {
    pub fn title(self) -> &'static str {
        match self {
            Self::Open => "Open sketch",
            Self::Save => "Save sketch",
            Self::Import => "Import PNG as a layer",
            Self::Export { .. } => "Export PNG",
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::Open | Self::Save => "sketchpad",
            _ => "png",
        }
    }
    fn writes(self) -> bool {
        matches!(self, Self::Save | Self::Export { .. })
    }
}
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    path: PathBuf,
    directory: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct FileBrowser {
    pub purpose: FilePurpose,
    directory: PathBuf,
    location: String,
    name: String,
    entries: Arc<Vec<Entry>>,
    pub error: Option<String>,
    overwrite: Option<PathBuf>,
    show_hidden: bool,
    new_folder: Option<String>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum BrowserOutcome {
    None,
    Cancel,
    Chosen(FilePurpose, PathBuf),
}

impl FileBrowser {
    pub fn new(purpose: FilePurpose, suggested: PathBuf) -> Self {
        let name = if purpose.writes() {
            suggested
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let mut directory = if suggested.is_dir() {
            suggested
        } else {
            suggested.parent().unwrap_or(Path::new(".")).to_owned()
        };
        while !directory.is_dir() {
            if !directory.pop() {
                directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
                break;
            }
        }
        let mut result = Self {
            purpose,
            location: directory.display().to_string(),
            directory,
            name,
            entries: Arc::new(Vec::new()),
            error: None,
            overwrite: None,
            show_hidden: false,
            new_folder: None,
        };
        result.refresh();
        result
    }
    fn refresh(&mut self) {
        let result = (|| -> std::io::Result<Vec<Entry>> {
            let mut entries = Vec::new();
            for entry in std::fs::read_dir(&self.directory)? {
                let entry = entry?;
                if !self.show_hidden && entry.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                let path = entry.path();
                let directory = path.is_dir();
                if directory
                    || path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case(self.purpose.extension()))
                {
                    entries.push(Entry { path, directory });
                }
            }
            entries.sort_by_key(|entry| {
                (
                    !entry.directory,
                    entry
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_lowercase(),
                )
            });
            Ok(entries)
        })();
        match result {
            Ok(entries) => {
                self.entries = Arc::new(entries);
                self.error = None;
            }
            Err(error) => {
                self.entries = Arc::new(Vec::new());
                self.error = Some(format!("Cannot read this folder: {error}"));
            }
        }
    }
    fn navigate(&mut self, path: PathBuf) {
        let path = if path.is_absolute() {
            path
        } else {
            self.directory.join(path)
        };
        if !path.is_dir() {
            self.error = Some("That folder does not exist.".into());
            return;
        }
        self.directory = path;
        self.location = self.directory.display().to_string();
        self.overwrite = None;
        self.refresh();
    }
    fn candidate(&self) -> Result<PathBuf, String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Enter a file name or choose a file.".into());
        }
        let mut path = self.directory.join(name);
        if path.is_dir() {
            return Err("Choose a file name, rather than a folder.".into());
        }
        if self.purpose.writes()
            && !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case(self.purpose.extension()))
        {
            path.as_mut_os_string()
                .push(format!(".{}", self.purpose.extension()));
        }
        if !self.purpose.writes() && !path.is_file() {
            return Err("That file does not exist.".into());
        }
        if self.purpose == FilePurpose::Import
            && !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case(self.purpose.extension()))
        {
            return Err(format!("Choose a .{} file.", self.purpose.extension()));
        }
        Ok(path)
    }
    pub fn show(&mut self, ctx: &egui::Context) -> BrowserOutcome {
        let mut outcome = BrowserOutcome::None;
        let mut navigate = None;
        let mut choose = false;
        let width = (ctx.content_rect().width() - 64.0).clamp(280.0, 620.0);
        egui::Window::new(self.purpose.title())
            .id(egui::Id::new("file-browser"))
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .default_width(width)
            .title_bar(false)
            .frame(super::app_ui::panel_frame())
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                let widgets = &mut ui.visuals_mut().widgets;
                for style in [
                    &mut widgets.inactive,
                    &mut widgets.hovered,
                    &mut widgets.active,
                ] {
                    style.corner_radius = egui::CornerRadius::same(9);
                    style.bg_fill = Color32::from_rgb(245, 243, 238);
                    style.weak_bg_fill = Color32::from_rgb(245, 243, 238);
                    style.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(221, 219, 212));
                }
                widgets.hovered.weak_bg_fill = Color32::from_rgb(234, 240, 235);
                widgets.active.weak_bg_fill = Color32::from_rgb(216, 231, 220);
                ui.heading(self.purpose.title());
                ui.horizontal(|ui| {
                    if ui
                        .add_sized([48.0, 44.0], egui::Button::new("Up"))
                        .clicked()
                    {
                        navigate = self.directory.parent().map(Path::to_owned);
                    }
                    if ui
                        .add_sized([60.0, 44.0], egui::Button::new("Home"))
                        .clicked()
                    {
                        navigate = std::env::var_os("HOME").map(PathBuf::from);
                    }
                    if ui
                        .add_sized([72.0, 44.0], egui::Button::new("Refresh"))
                        .clicked()
                    {
                        self.refresh();
                    }
                    if ui
                        .add_sized([96.0, 44.0], egui::Button::new("New folder"))
                        .clicked()
                    {
                        self.new_folder = Some(String::new());
                    }
                    if ui.checkbox(&mut self.show_hidden, "Hidden files").changed() {
                        self.refresh();
                    }
                });
                let mut create_folder = None;
                if let Some(name) = &mut self.new_folder {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [width - 108.0, 40.0],
                            egui::TextEdit::singleline(name).hint_text("New folder name"),
                        );
                        if ui
                            .add_sized([96.0, 40.0], egui::Button::new("Create"))
                            .clicked()
                        {
                            create_folder = Some(name.trim().to_owned());
                        }
                    });
                }
                if let Some(name) = create_folder {
                    if name.is_empty()
                        || Path::new(&name).components().count() != 1
                        || name == "."
                        || name == ".."
                    {
                        self.error = Some("Enter a single folder name.".into());
                    } else {
                        let path = self.directory.join(name);
                        match std::fs::create_dir(&path) {
                            Ok(()) => {
                                self.new_folder = None;
                                navigate = Some(path);
                            }
                            Err(error) => {
                                self.error = Some(format!("Cannot create folder: {error}"))
                            }
                        }
                    }
                }
                ui.horizontal(|ui| {
                    let response = ui.add_sized(
                        [width - 60.0, 36.0],
                        egui::TextEdit::singleline(&mut self.location),
                    );
                    let enter = (response.has_focus() || response.lost_focus())
                        && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui
                        .add_sized([48.0, 36.0], egui::Button::new("Go"))
                        .clicked()
                        || enter
                    {
                        navigate = Some(PathBuf::from(&self.location));
                    }
                });
                ui.separator();
                if self.entries.is_empty() {
                    ui.label("No matching files in this folder.");
                }
                egui::ScrollArea::vertical()
                    .id_salt("files")
                    .max_height((ctx.content_rect().height() - 390.0).clamp(100.0, 320.0))
                    .auto_shrink([false, false])
                    .show_rows(ui, 44.0, self.entries.len(), |ui, range| {
                        for entry in &self.entries[range] {
                            let name = entry.path.file_name().unwrap_or_default().to_string_lossy();
                            let label = if entry.directory {
                                format!("{name}  /")
                            } else {
                                name.to_string()
                            };
                            let selected = !entry.directory && self.name == name;
                            let (rect, response) = ui.allocate_exact_size(
                                Vec2::new(width - 16.0, 44.0),
                                egui::Sense::click(),
                            );
                            let fill = if selected {
                                Color32::from_rgb(55, 102, 90)
                            } else if response.hovered() {
                                Color32::from_rgb(234, 240, 235)
                            } else {
                                Color32::from_rgb(245, 243, 238)
                            };
                            ui.painter().rect_filled(rect, 9, fill);
                            ui.painter().with_clip_rect(rect.shrink(8.0)).text(
                                rect.left_center() + Vec2::new(12.0, 0.0),
                                Align2::LEFT_CENTER,
                                &label,
                                egui::FontId::proportional(14.0),
                                if selected {
                                    Color32::WHITE
                                } else {
                                    Color32::from_rgb(46, 50, 47)
                                },
                            );
                            response.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    ui.is_enabled(),
                                    &label,
                                )
                            });
                            if response.clicked() {
                                if entry.directory {
                                    navigate = Some(entry.path.clone());
                                } else {
                                    self.name = name.into_owned();
                                    self.overwrite = None;
                                }
                            }
                            if response.double_clicked()
                                && !entry.directory
                                && !self.purpose.writes()
                            {
                                choose = true;
                            }
                        }
                    });
                ui.separator();
                ui.label(if self.purpose.writes() {
                    "File name"
                } else {
                    "Selected file or full path"
                });
                let response =
                    ui.add_sized([width, 40.0], egui::TextEdit::singleline(&mut self.name));
                if response.changed() {
                    self.overwrite = None;
                }
                if (response.has_focus() || response.lost_focus())
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                {
                    choose = true;
                }
                if let Some(error) = &self.error {
                    ui.colored_label(Color32::from_rgb(170, 45, 35), error);
                }
                if let Some(path) = self.overwrite.clone() {
                    ui.label(format!(
                        "{} already exists. Replace it?",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    ui.horizontal(|ui| {
                        if ui
                            .add_sized([120.0, 44.0], egui::Button::new("Replace file"))
                            .clicked()
                        {
                            self.overwrite = None;
                            outcome = BrowserOutcome::Chosen(self.purpose, path);
                        }
                        if ui
                            .add_sized([120.0, 44.0], egui::Button::new("Keep existing"))
                            .clicked()
                        {
                            self.overwrite = None;
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        if ui
                            .add_sized([130.0, 44.0], egui::Button::new(self.purpose.title()))
                            .clicked()
                        {
                            choose = true;
                        }
                        if ui
                            .add_sized([100.0, 44.0], egui::Button::new("Cancel"))
                            .clicked()
                        {
                            outcome = BrowserOutcome::Cancel;
                        }
                    });
                }
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            outcome = BrowserOutcome::Cancel;
        }
        if let Some(path) = navigate {
            self.navigate(path);
        }
        if choose {
            match self.candidate() {
                Ok(path) if self.purpose.writes() && path.exists() => self.overwrite = Some(path),
                Ok(path) => outcome = BrowserOutcome::Chosen(self.purpose, path),
                Err(error) => self.error = Some(error),
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn save_resolves_the_actual_file_before_overwrite_confirmation() {
        let mut browser = FileBrowser::new(
            FilePurpose::Save,
            std::env::temp_dir().join("test.sketchpad"),
        );
        browser.name = "drawing.v2".into();
        assert_eq!(
            browser.candidate().unwrap(),
            browser.directory.join("drawing.v2.sketchpad")
        );
        browser.name = "drawing.SKETCHPAD".into();
        assert_eq!(
            browser.candidate().unwrap(),
            browser.directory.join("drawing.SKETCHPAD")
        );
        browser.name = "   ".into();
        assert!(browser.candidate().is_err());
    }
    #[test]
    fn missing_suggested_folder_falls_back_to_existing_parent() {
        let browser = FileBrowser::new(
            FilePurpose::Save,
            std::env::temp_dir().join("sketchpad-missing-folder-for-test/nested/drawing.sketchpad"),
        );
        assert!(browser.directory.is_dir());
        assert_eq!(browser.name, "drawing.sketchpad");
    }
}
